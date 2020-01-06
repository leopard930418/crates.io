use conduit::Request;
use flate2::read::GzDecoder;
use openssl::error::ErrorStack;
use openssl::hash::{Hasher, MessageDigest};
use reqwest::header;

use crate::util::errors::{cargo_err, internal, AppResult, ChainError};
use crate::util::{LimitErrorReader, Maximums};

use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::sync::Arc;

use crate::middleware::app::RequestApp;
use crate::models::Crate;

const CACHE_CONTROL_IMMUTABLE: &str = "public,max-age=31536000,immutable";
const CACHE_CONTROL_README: &str = "public,max-age=604800";

#[derive(Clone, Debug)]
pub enum Uploader {
    /// For production usage, uploads and redirects to s3.
    /// For test usage with `TestApp::with_proxy()`, the recording proxy is used.
    S3 {
        bucket: s3::Bucket,
        cdn: Option<String>,
    },

    /// For development usage only: "uploads" crate files to `dist` and serves them
    /// from there as well to enable local publishing and download
    Local,
}

impl Uploader {
    /// Returns the URL of an uploaded crate's version archive.
    ///
    /// The function doesn't check for the existence of the file.
    pub fn crate_location(&self, crate_name: &str, version: &str) -> String {
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
                format!("https://{}/{}", host, path)
            }
            Uploader::Local => format!("/{}", Uploader::crate_path(crate_name, version)),
        }
    }

    /// Returns the URL of an uploaded crate's version readme.
    ///
    /// The function doesn't check for the existence of the file.
    pub fn readme_location(&self, crate_name: &str, version: &str) -> String {
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
                format!("https://{}/{}", host, path)
            }
            Uploader::Local => format!("/{}", Uploader::readme_path(crate_name, version)),
        }
    }

    /// Returns the internal path of an uploaded crate's version archive.
    fn crate_path(name: &str, version: &str) -> String {
        // No slash in front so we can use join
        format!("crates/{}/{}-{}.crate", name, name, version)
    }

    /// Returns the internal path of an uploaded crate's version readme.
    fn readme_path(name: &str, version: &str) -> String {
        format!("readmes/{}/{}-{}.html", name, name, version)
    }

    /// Uploads a file using the configured uploader (either `S3`, `Local`).
    ///
    /// It returns the path of the uploaded file.
    ///
    /// # Panics
    ///
    /// This function can panic on an `Self::Local` during development.
    /// Production and tests use `Self::S3` which should not panic.
    pub fn upload<R: std::io::Read + Send + 'static>(
        &self,
        client: &reqwest::Client,
        path: &str,
        mut content: R,
        content_length: u64,
        content_type: &str,
        extra_headers: header::HeaderMap,
    ) -> Result<Option<String>, Box<dyn Error>> {
        match *self {
            Uploader::S3 { ref bucket, .. } => {
                bucket
                    .put(
                        client,
                        path,
                        content,
                        content_length,
                        content_type,
                        extra_headers,
                    )
                    .map_err(Box::new)?;
                Ok(Some(String::from(path)))
            }
            Uploader::Local => {
                let filename = env::current_dir().unwrap().join("local_uploads").join(path);
                let dir = filename.parent().unwrap();
                fs::create_dir_all(dir).map_err(Box::new)?;
                let mut file = File::create(&filename).map_err(Box::new)?;
                std::io::copy(&mut content, &mut file).map_err(Box::new)?;
                Ok(filename.to_str().map(String::from))
            }
        }
    }

    /// Uploads a crate and returns the checksum of the uploaded crate file.
    pub fn upload_crate(
        &self,
        req: &mut dyn Request,
        krate: &Crate,
        maximums: Maximums,
        vers: &semver::Version,
    ) -> AppResult<Vec<u8>> {
        let app = Arc::clone(req.app());
        let path = Uploader::crate_path(&krate.name, &vers.to_string());
        let mut body = Vec::new();
        LimitErrorReader::new(req.body(), maximums.max_upload_size).read_to_end(&mut body)?;
        verify_tarball(krate, vers, &body, maximums.max_unpack_size)?;
        let checksum = hash(&body)?;
        let content_length = body.len() as u64;
        let content = Cursor::new(body);
        let mut extra_headers = header::HeaderMap::new();
        extra_headers.insert(
            header::CACHE_CONTROL,
            CACHE_CONTROL_IMMUTABLE.parse().unwrap(),
        );
        self.upload(
            app.http_client(),
            &path,
            content,
            content_length,
            "application/x-tar",
            extra_headers,
        )
        .map_err(|e| internal(&format_args!("failed to upload crate: {}", e)))?;
        Ok(checksum)
    }

    pub(crate) fn upload_readme(
        &self,
        http_client: &reqwest::Client,
        crate_name: &str,
        vers: &str,
        readme: String,
    ) -> Result<(), Box<dyn Error>> {
        let path = Uploader::readme_path(crate_name, vers);
        let content_length = readme.len() as u64;
        let content = Cursor::new(readme);
        let mut extra_headers = header::HeaderMap::new();
        extra_headers.insert(header::CACHE_CONTROL, CACHE_CONTROL_README.parse().unwrap());
        self.upload(
            http_client,
            &path,
            content,
            content_length,
            "text/html",
            extra_headers,
        )?;
        Ok(())
    }
}

fn verify_tarball(
    krate: &Crate,
    vers: &semver::Version,
    tarball: &[u8],
    max_unpack: u64,
) -> AppResult<()> {
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
            cargo_err("uploaded tarball is malformed or too large when decompressed")
        })?;

        // Verify that all entries actually start with `$name-$vers/`.
        // Historically Cargo didn't verify this on extraction so you could
        // upload a tarball that contains both `foo-0.1.0/` source code as well
        // as `bar-0.1.0/` source code, and this could overwrite other crates in
        // the registry!
        if !entry.path()?.starts_with(&prefix) {
            return Err(cargo_err("invalid tarball uploaded"));
        }

        // Historical versions of the `tar` crate which Cargo uses internally
        // don't properly prevent hard links and symlinks from overwriting
        // arbitrary files on the filesystem. As a bit of a hammer we reject any
        // tarball with these sorts of links. Cargo doesn't currently ever
        // generate a tarball with these file types so this should work for now.
        let entry_type = entry.header().entry_type();
        if entry_type.is_hard_link() || entry_type.is_symlink() {
            return Err(cargo_err("invalid tarball uploaded"));
        }
    }
    Ok(())
}

fn hash(data: &[u8]) -> Result<Vec<u8>, ErrorStack> {
    let mut hasher = Hasher::new(MessageDigest::sha256())?;
    hasher.update(data)?;
    Ok(hasher.finish()?.to_vec())
}
