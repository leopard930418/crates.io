use crate::{env, uploaders::Uploader, Env, Replica};
use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub uploader: Uploader,
    pub session_key: String,
    pub git_repo_checkout: PathBuf,
    pub gh_client_id: String,
    pub gh_client_secret: String,
    pub db_url: String,
    pub env: Env,
    pub max_upload_size: u64,
    pub max_unpack_size: u64,
    pub mirror: Replica,
    pub api_protocol: String,
}

impl Default for Config {
    /// Returns a default value for the application's config
    ///
    /// Sets the following default values:
    ///
    /// - `Config::max_upload_size`: 10MiB
    /// - `Config::api_protocol`: `https`
    ///
    /// Pulls values from the following environment variables:
    ///
    /// - `GIT_REPO_CHECKOUT`: The directory where the registry index was cloned.
    /// - `MIRROR`: Is this instance of cargo_registry a mirror of crates.io.
    /// - `HEROKU`: Is this instance of cargo_registry currently running on Heroku.
    /// - `S3_BUCKET`: The S3 bucket used to store crate files. If not present during development,
    /// cargo_registry will fall back to a local uploader.
    /// - `S3_REGION`: The region in which the bucket was created. Optional if US standard.
    /// - `S3_ACCESS_KEY`: The access key to interact with S3. Optional if running a mirror.
    /// - `S3_SECRET_KEY`: The secret key to interact with S3. Optional if running a mirror.
    /// - `SESSION_KEY`: The key used to sign and encrypt session cookies.
    /// - `GH_CLIENT_ID`: The client ID of the associated GitHub application.
    /// - `GH_CLIENT_SECRET`: The client secret of the associated GitHub application.
    /// - `DATABASE_URL`: The URL of the postgres database to use.
    fn default() -> Config {
        let checkout = PathBuf::from(env("GIT_REPO_CHECKOUT"));
        let api_protocol = String::from("https");
        let mirror = if env::var("MIRROR").is_ok() {
            Replica::ReadOnlyMirror
        } else {
            Replica::Primary
        };
        let heroku = env::var("HEROKU").is_ok();
        let cargo_env = if heroku {
            Env::Production
        } else {
            Env::Development
        };
        let uploader = match (cargo_env, mirror) {
            (Env::Production, Replica::Primary) => {
                // `env` panics if these vars are not set, and in production for a primary instance,
                // that's what we want since we don't want to be able to start the server if the
                // server doesn't know where to upload crates.
                Uploader::S3 {
                    bucket: s3::Bucket::new(
                        env("S3_BUCKET"),
                        env::var("S3_REGION").ok(),
                        env("S3_ACCESS_KEY"),
                        env("S3_SECRET_KEY"),
                        &api_protocol,
                    ),
                    cdn: env::var("S3_CDN").ok(),
                    proxy: None,
                }
            }
            (Env::Production, Replica::ReadOnlyMirror) => {
                // Read-only mirrors don't need access key or secret key since by definition,
                // they'll only need to read from a bucket, not upload.
                //
                // Read-only mirrors might have access key or secret key, so use them if those
                // environment variables are set.
                //
                // Read-only mirrors definitely need bucket though, so that they know where
                // to serve crate files from.
                Uploader::S3 {
                    bucket: s3::Bucket::new(
                        env("S3_BUCKET"),
                        env::var("S3_REGION").ok(),
                        env::var("S3_ACCESS_KEY").unwrap_or_default(),
                        env::var("S3_SECRET_KEY").unwrap_or_default(),
                        &api_protocol,
                    ),
                    cdn: env::var("S3_CDN").ok(),
                    proxy: None,
                }
            }
            // In Development mode, either running as a primary instance or a read-only mirror
            _ => {
                if env::var("S3_BUCKET").is_ok() {
                    // If we've set the `S3_BUCKET` variable to any value, use all of the values
                    // for the related S3 environment variables and configure the app to upload to
                    // and read from S3 like production does. All values except for bucket are
                    // optional, like production read-only mirrors.
                    println!("Using S3 uploader");
                    Uploader::S3 {
                        bucket: s3::Bucket::new(
                            env("S3_BUCKET"),
                            env::var("S3_REGION").ok(),
                            env::var("S3_ACCESS_KEY").unwrap_or_default(),
                            env::var("S3_SECRET_KEY").unwrap_or_default(),
                            &api_protocol,
                        ),
                        cdn: env::var("S3_CDN").ok(),
                        proxy: None,
                    }
                } else {
                    // If we don't set the `S3_BUCKET` variable, we'll use a development-only
                    // uploader that makes it possible to run and publish to a locally-running
                    // crates.io instance without needing to set up an account and a bucket in S3.
                    println!(
                        "Using local uploader, crate files will be in the local_uploads directory"
                    );
                    Uploader::Local
                }
            }
        };
        Config {
            uploader,
            session_key: env("SESSION_KEY"),
            git_repo_checkout: checkout,
            gh_client_id: env("GH_CLIENT_ID"),
            gh_client_secret: env("GH_CLIENT_SECRET"),
            db_url: env("DATABASE_URL"),
            env: cargo_env,
            max_upload_size: 10 * 1024 * 1024, // 10 MB default file upload size limit
            max_unpack_size: 512 * 1024 * 1024, // 512 MB max when decompressed
            mirror,
            api_protocol,
        }
    }
}
