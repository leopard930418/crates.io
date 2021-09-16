use crate::{
    config, db,
    models::Version,
    schema::{crates, readme_renderings, versions},
    uploaders::Uploader,
};
use std::{io::Read, path::Path, sync::Arc, thread};

use chrono::{TimeZone, Utc};
use cio_markdown::readme_to_html;
use clap::Clap;
use diesel::{dsl::any, prelude::*};
use flate2::read::GzDecoder;
use reqwest::{blocking::Client, header};
use tar::{self, Archive};

const CACHE_CONTROL_README: &str = "public,max-age=604800";
const USER_AGENT: &str = "crates-admin";

#[derive(Clap, Debug)]
#[clap(
    name = "render-readmes",
    about = "Iterates over every crate versions ever uploaded and (re-)renders their \
        readme using the readme renderer from the cargo_registry crate.",
    after_help = "Warning: this can take a lot of time."
)]
pub struct Opts {
    /// How many versions should be queried and processed at a time.
    #[clap(long, default_value = "25")]
    page_size: usize,

    /// Only rerender readmes that are older than this date.
    #[clap(long)]
    older_than: Option<String>,

    /// Only rerender readmes for the specified crate.
    #[clap(long = "crate")]
    crate_name: Option<String>,
}

pub fn run(opts: Opts) {
    let base_config = Arc::new(config::Base::from_environment());
    let conn = db::connect_now().unwrap();

    let start_time = Utc::now();

    let older_than = if let Some(ref time) = opts.older_than {
        Utc.datetime_from_str(time, "%Y-%m-%d %H:%M:%S")
            .expect("Could not parse --older-than argument as a time")
    } else {
        start_time
    };
    let older_than = older_than.naive_utc();

    println!("Start time:                   {}", start_time);
    println!("Rendering readmes older than: {}", older_than);

    let mut query = versions::table
        .inner_join(crates::table)
        .left_outer_join(readme_renderings::table)
        .filter(
            readme_renderings::rendered_at
                .lt(older_than)
                .or(readme_renderings::version_id.is_null()),
        )
        .select(versions::id)
        .into_boxed();

    if let Some(crate_name) = opts.crate_name {
        println!("Rendering readmes for {}", crate_name);
        query = query.filter(crates::name.eq(crate_name));
    }

    let version_ids: Vec<i32> = query.load(&conn).expect("error loading version ids");

    let total_versions = version_ids.len();
    println!("Rendering {} versions", total_versions);

    let page_size = opts.page_size;

    let total_pages = total_versions / page_size;
    let total_pages = if total_versions % page_size == 0 {
        total_pages
    } else {
        total_pages + 1
    };

    let client = Client::new();

    for (page_num, version_ids_chunk) in version_ids.chunks(page_size).enumerate() {
        println!(
            "= Page {} of {} ==================================",
            page_num + 1,
            total_pages
        );

        let versions: Vec<(Version, String)> = versions::table
            .inner_join(crates::table)
            .filter(versions::id.eq(any(version_ids_chunk)))
            .select((versions::all_columns, crates::name))
            .load(&conn)
            .expect("error loading versions");

        let mut tasks = Vec::with_capacity(page_size as usize);
        for (version, krate_name) in versions {
            Version::record_readme_rendering(version.id, &conn).unwrap_or_else(|_| {
                panic!(
                    "[{}-{}] Couldn't record rendering time",
                    krate_name, version.num
                )
            });
            let client = client.clone();
            let base_config = base_config.clone();
            let handle = thread::spawn(move || {
                println!("[{}-{}] Rendering README...", krate_name, version.num);
                let readme = get_readme(base_config.uploader(), &client, &version, &krate_name);
                if readme.is_none() {
                    return;
                }
                let readme = readme.unwrap();
                let content_length = readme.len() as u64;
                let content = std::io::Cursor::new(readme);
                let readme_path = format!("readmes/{0}/{0}-{1}.html", krate_name, version.num);
                let mut extra_headers = header::HeaderMap::new();
                extra_headers.insert(header::CACHE_CONTROL, CACHE_CONTROL_README.parse().unwrap());
                base_config
                    .uploader()
                    .upload(
                        &client,
                        &readme_path,
                        content,
                        content_length,
                        "text/html",
                        extra_headers,
                    )
                    .unwrap_or_else(|_| {
                        panic!(
                            "[{}-{}] Couldn't upload file to S3",
                            krate_name, version.num
                        )
                    });
            });
            tasks.push(handle);
        }
        for handle in tasks {
            if let Err(err) = handle.join() {
                println!("Thread panicked: {:?}", err);
            }
        }
    }
}

/// Renders the readme of an uploaded crate version.
fn get_readme(
    uploader: &Uploader,
    client: &Client,
    version: &Version,
    krate_name: &str,
) -> Option<String> {
    let location = uploader.crate_location(krate_name, &version.num.to_string());

    let location = match uploader {
        Uploader::S3 { .. } => location,
        Uploader::Local => format!("http://localhost:8888/{}", location),
    };

    let mut extra_headers = header::HeaderMap::new();
    extra_headers.insert(header::USER_AGENT, USER_AGENT.parse().unwrap());
    let response = match client.get(&location).headers(extra_headers).send() {
        Ok(r) => r,
        Err(err) => {
            println!(
                "[{}-{}] Unable to fetch crate: {}",
                krate_name, version.num, err
            );
            return None;
        }
    };

    if !response.status().is_success() {
        println!(
            "[{}-{}] Failed to get a 200 response: {}",
            krate_name,
            version.num,
            response.text().unwrap()
        );
        return None;
    }

    let reader = GzDecoder::new(response);
    let mut archive = Archive::new(reader);
    let mut entries = archive.entries().unwrap_or_else(|_| {
        panic!(
            "[{}-{}] Invalid tar archive entries",
            krate_name, version.num
        )
    });
    let manifest: Manifest = {
        let path = format!("{}-{}/Cargo.toml", krate_name, version.num);
        let contents = find_file_by_path(&mut entries, Path::new(&path), version, krate_name);
        toml::from_str(&contents).unwrap_or_else(|_| {
            panic!(
                "[{}-{}] Syntax error in manifest file",
                krate_name, version.num
            )
        })
    };

    let rendered = {
        let path = format!(
            "{}-{}/{}",
            krate_name, version.num, manifest.package.readme?
        );
        let contents = find_file_by_path(&mut entries, Path::new(&path), version, krate_name);
        readme_to_html(
            &contents,
            manifest
                .package
                .readme_file
                .as_ref()
                .map_or("README.md", |e| &**e),
            manifest.package.repository.as_deref(),
        )
    };
    return Some(rendered);

    #[derive(Deserialize)]
    struct Package {
        readme: Option<String>,
        readme_file: Option<String>,
        repository: Option<String>,
    }

    #[derive(Deserialize)]
    struct Manifest {
        package: Package,
    }
}

/// Search an entry by its path in a Tar archive.
fn find_file_by_path<R: Read>(
    entries: &mut tar::Entries<'_, R>,
    path: &Path,
    version: &Version,
    krate_name: &str,
) -> String {
    let mut file = entries
        .find(|entry| match *entry {
            Err(_) => false,
            Ok(ref file) => {
                let filepath = match file.path() {
                    Ok(p) => p,
                    Err(_) => return false,
                };
                filepath == path
            }
        })
        .unwrap_or_else(|| {
            panic!(
                "[{}-{}] couldn't open file: {}",
                krate_name,
                version.num,
                path.display()
            )
        })
        .unwrap_or_else(|_| {
            panic!(
                "[{}-{}] file is not present: {}",
                krate_name,
                version.num,
                path.display()
            )
        });
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap_or_else(|_| {
        panic!(
            "[{}-{}] Couldn't read file contents",
            krate_name, version.num
        )
    });
    contents
}
