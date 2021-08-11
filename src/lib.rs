//! This crate implements the backend server for <https://crates.io/>
//!
//! All implemented routes are defined in the [middleware](fn.middleware.html) function and
//! implemented in the [category](category/index.html), [keyword](keyword/index.html),
//! [krate](krate/index.html), [user](user/index.html) and [version](version/index.html) modules.

#![warn(clippy::all, rust_2018_idioms)]

#[cfg(test)]
#[macro_use]
extern crate claim;
#[macro_use]
extern crate derive_deref;
#[macro_use]
extern crate diesel;
#[macro_use]
extern crate diesel_migrations;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate serde_json;
#[macro_use]
extern crate tracing;

pub use crate::{app::App, email::Emails, uploaders::Uploader};
use std::str::FromStr;
use std::sync::Arc;

use conduit_middleware::MiddlewareBuilder;
use jemallocator::Jemalloc;

#[global_allocator]
static ALLOC: Jemalloc = Jemalloc;

pub mod admin;
mod app;
pub mod background_jobs;
pub mod boot;
pub mod config;
pub mod db;
mod downloads_counter;
pub mod email;
pub mod git;
pub mod github;
pub mod metrics;
pub mod middleware;
pub mod rate_limiter;
pub mod render;
pub mod schema;
pub mod tasks;
mod test_util;
pub mod uploaders;
#[macro_use]
pub mod util;

pub mod controllers;
pub mod models;
mod router;
pub mod views;

/// Used for setting different values depending on whether the app is being run in production,
/// in development, or for testing.
///
/// The app's `config.env` value is set in *src/bin/server.rs* to `Production` if the environment
/// variable `HEROKU` is set and `Development` otherwise. `config.env` is set to `Test`
/// unconditionally in *src/test/all.rs*.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Env {
    Development,
    Test,
    Production,
}

/// Used for setting different values depending on the type of registry this instance is.
///
/// `Primary` indicates this instance is a primary registry that is the source of truth for these
/// crates' information. `ReadOnlyMirror` indicates this instanceis a read-only mirror of crate
/// information that exists on another instance.
///
/// The app's `config.mirror` value is set in *src/bin/server.rs* to `ReadOnlyMirror` if the
/// `MIRROR` environment variable is set and to `Primary` otherwise.
///
/// There may be more ways to run crates.io servers in the future, such as a
/// mirror that also has private crates that crates.io does not have.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Replica {
    Primary,
    ReadOnlyMirror,
}

/// Configures routes, sessions, logging, and other middleware.
///
/// Called from *src/bin/server.rs*.
pub fn build_handler(app: Arc<App>) -> MiddlewareBuilder {
    let endpoints = router::build_router(&app);
    middleware::build_middleware(app, endpoints)
}

/// Convenience function requiring that an environment variable is set.
///
/// Ensures that we've initialized the dotenv crate in order to read environment variables
/// from a *.env* file if present. Don't use this for optionally set environment variables.
///
/// # Panics
///
/// Panics if the environment variable with the name passed in as an argument is not defined
/// in the current environment.
#[track_caller]
pub fn env(s: &str) -> String {
    dotenv::var(s).unwrap_or_else(|_| panic!("must have `{}` defined", s))
}

/// Parse an optional environment variable
///
/// Ensures that we've initialized the dotenv crate in order to read environment variables
/// from a *.env* file if present. A variable that is set to invalid unicode will be handled
/// as if it was unset.
///
/// # Panics
///
/// Panics if the environment variable is set but cannot be parsed as the requested type.
#[track_caller]
pub fn env_optional<T: FromStr>(s: &str) -> Option<T> {
    dotenv::var(s).ok().map(|s| {
        s.parse()
            .unwrap_or_else(|_| panic!("`{}` was defined but could not be parsed", s))
    })
}

sql_function!(fn lower(x: ::diesel::sql_types::Text) -> ::diesel::sql_types::Text);
