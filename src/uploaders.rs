use conduit::Request;
use flate2::read::GzDecoder;
use openssl::hash::{Hasher, MessageDigest};

use crate::util::LimitErrorReader;
use crate::util::{human, internal, CargoResult, ChainError, Maximums};

use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::sync::Arc;

use crate::app::App;
use crate::middleware::app::RequestApp;
use crate::models::Crate;

fn require_test_app_with_proxy() -> ! {
    panic!("No uploader is configured.  In tests, use `TestApp::with_proxy()`.");
}

#[derive(Clone, Debug)]
pub enum Uploader {
    /// For production usage, uploads and redirects to s3.
    /// For test usage with `TestApp::with_proxy()`, the recording proxy is used.
    S3 {
        bucket: s3::Bucket,
        cdn: Option<String>,
        proxy: Option<String>,
    },

    /// For development usage only: "uploads" crate files to `dist` and serves them
    /// from there as well to enable local publishing and download
    Local,

    /// For tests using `TestApp::init()`
    /// Attempts to get an outgoing HTTP handle will panic.
    Panic,
}

impl Uploader {
    pub fn proxy(&self) -> Option<&str> {
        match *self {
            Uploader::S3 { ref proxy, .. } => proxy.as_ref().map(String::as_str),
            Uploader::Local => None,
            Uploader::Panic => require_test_app_with_proxy(),
        }
    }

    /// Returns the URL of an uploaded crate's version archive.
    ///
    /// The function doesn't check for the existence of the file.
    /// It returns `None` if the current `Uploader` is `Panic`.
    pub fn crate_location(&self, crate_name: &str, version: &str) -> Option<String> {
        match *self {
            Uploader::S3 {
                ref bucket,
                ref cdn,
                ..
            } => {
                let host = match *cdn {
                    Some(ref s) => s.clone(),
                    None => bucket.host(),
                };
                let path = Uploader::crate_path(crate_name, version);
                Some(format!("https://{}/{}", host, path))
            }
            Uploader::Local => Some(format!("/{}", Uploader::crate_path(crate_name, version))),
            Uploader::Panic => require_test_app_with_proxy(),
        }
    }

    /// Returns the URL of an uploaded crate's version readme.
    ///
    /// The function doesn't check for the existence of the file.
    /// It returns `None` if the current `Uploader` is `Panic`.
    pub fn readme_location(&self, crate_name: &str, version: &str) -> Option<String> {
        match *self {
            Uploader::S3 {
                ref bucket,
                ref cdn,
                ..
            } => {
                let host = match *cdn {
                    Some(ref s) => s.clone(),
                    None => bucket.host(),
                };
                let path = Uploader::readme_path(crate_name, version);
                Some(format!("https://{}/{}", host, path))
            }
            Uploader::Local => Some(format!("/{}", Uploader::readme_path(crate_name, version))),
            Uploader::Panic => require_test_app_with_proxy(),
        }
    }

    /// Returns the interna path of an uploaded crate's version archive.
    fn crate_path(name: &str, version: &str) -> String {
        // No slash in front so we can use join
        format!("crates/{}/{}-{}.crate", name, name, version)
    }

    /// Returns the interna path of an uploaded crate's version readme.
    fn readme_path(name: &str, version: &str) -> String {
        format!("readmes/{}/{}-{}.html", name, name, version)
    }

    /// Uploads a file using the configured uploader (either `S3`, `Local` or `Panic`).
    ///
    /// It returns a a tuple containing the path of the uploaded file
    /// and its checksum.
    pub fn upload(
        &self,
        client: &reqwest::Client,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> CargoResult<(Option<String>, Vec<u8>)> {
        let hash = hash(&body);
        match *self {
            Uploader::S3 { ref bucket, .. } => {
                bucket
                    .put(client, path, body, content_type)
                    .map_err(|e| internal(&format_args!("failed to upload to S3: {}", e)))?;
                Ok((Some(String::from(path)), hash))
            }
            Uploader::Local => {
                let filename = env::current_dir().unwrap().join("local_uploads").join(path);
                let dir = filename.parent().unwrap();
                fs::create_dir_all(dir)?;
                let mut file = File::create(&filename)?;
                file.write_all(&body)?;
                Ok((filename.to_str().map(String::from), hash))
            }
            Uploader::Panic => require_test_app_with_proxy(),
        }
    }

    /// Uploads a crate and its readme. Returns the checksum of the uploaded crate
    /// file, and bombs for the uploaded crate and the uploaded readme.
    pub fn upload_crate(
        &self,
        req: &mut dyn Request,
        krate: &Crate,
        readme: Option<String>,
        maximums: Maximums,
        vers: &semver::Version,
    ) -> CargoResult<(Vec<u8>, Bomb, Bomb)> {
        let app = Arc::clone(req.app());
        let (crate_path, checksum) = {
            let path = Uploader::crate_path(&krate.name, &vers.to_string());
            let mut body = Vec::new();
            LimitErrorReader::new(req.body(), maximums.max_upload_size).read_to_end(&mut body)?;
            verify_tarball(krate, vers, &body, maximums.max_unpack_size)?;
            self.upload(&app.http_client()?, &path, body, "application/x-tar")?
        };
        // We create the bomb for the crate file before uploading the readme so that if the
        // readme upload fails, the uploaded crate file is automatically deleted.
        let crate_bomb = Bomb {
            app: Arc::clone(&app),
            path: crate_path,
        };
        let (readme_path, _) = if let Some(rendered) = readme {
            let path = Uploader::readme_path(&krate.name, &vers.to_string());
            self.upload(
                &app.http_client()?,
                &path,
                rendered.into_bytes(),
                "text/html",
            )?
        } else {
            (None, vec![])
        };
        Ok((
            checksum,
            crate_bomb,
            Bomb {
                app: Arc::clone(&app),
                path: readme_path,
            },
        ))
    }

    /// Deletes an uploaded file.
    fn delete(&self, app: &Arc<App>, path: &str) -> CargoResult<()> {
        match *self {
            Uploader::S3 { ref bucket, .. } => {
                bucket.delete(&app.http_client()?, path)?;
                Ok(())
            }
            Uploader::Local => {
                fs::remove_file(path)?;
                Ok(())
            }
            Uploader::Panic => require_test_app_with_proxy(),
        }
    }
}

// Can't derive Debug because of App.
#[allow(missing_debug_implementations)]
pub struct Bomb {
    app: Arc<App>,
    pub path: Option<String>,
}

impl Drop for Bomb {
    fn drop(&mut self) {
        if let Some(ref path) = self.path {
            if let Err(e) = self.app.config.uploader.delete(&self.app, path) {
                println!("unable to delete {}, {:?}", path, e);
            }
        }
    }
}

fn verify_tarball(
    krate: &Crate,
    vers: &semver::Version,
    tarball: &[u8],
    max_unpack: u64,
) -> CargoResult<()> {
    // All our data is currently encoded with gzip
    let decoder = GzDecoder::new(tarball);

    // Don't let gzip decompression go into the weeeds, apply a fixed cap after
    // which point we say the decompressed source is "too large".
    let decoder = LimitErrorReader::new(decoder, max_unpack);

    // Use this I/O object now to take a peek inside
    let mut archive = tar::Archive::new(decoder);
    let prefix = format!("{}-{}", krate.name, vers);
    for entry in archive.entries()? {
        let entry = entry.chain_error(|| {
            human("uploaded tarball is malformed or too large when decompressed")
        })?;

        // Verify that all entries actually start with `$name-$vers/`.
        // Historically Cargo didn't verify this on extraction so you could
        // upload a tarball that contains both `foo-0.1.0/` source code as well
        // as `bar-0.1.0/` source code, and this could overwrite other crates in
        // the registry!
        if !entry.path()?.starts_with(&prefix) {
            return Err(human("invalid tarball uploaded"));
        }

        // Historical versions of the `tar` crate which Cargo uses internally
        // don't properly prevent hard links and symlinks from overwriting
        // arbitrary files on the filesystem. As a bit of a hammer we reject any
        // tarball with these sorts of links. Cargo doesn't currently ever
        // generate a tarball with these file types so this should work for now.
        let entry_type = entry.header().entry_type();
        if entry_type.is_hard_link() || entry_type.is_symlink() {
            return Err(human("invalid tarball uploaded"));
        }
    }
    Ok(())
}

fn hash(data: &[u8]) -> Vec<u8> {
    let mut hasher = Hasher::new(MessageDigest::sha256()).unwrap();
    hasher.update(data).unwrap();
    hasher.finish().unwrap().to_vec()
}
