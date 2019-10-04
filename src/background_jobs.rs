use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use swirl::PerformError;

use crate::db::{DieselPool, DieselPooledConn};
use crate::git::Repository;
use crate::uploaders::Uploader;
use crate::util::errors::{CargoErrToStdErr, CargoResult};

impl<'a> swirl::db::BorrowedConnection<'a> for DieselPool {
    type Connection = DieselPooledConn<'a>;
}

impl swirl::db::DieselPool for DieselPool {
    type Error = CargoErrToStdErr;

    fn get(&self) -> Result<swirl::db::DieselPooledConn<'_, Self>, Self::Error> {
        self.get().map_err(CargoErrToStdErr)
    }
}

#[allow(missing_debug_implementations)]
pub struct Environment {
    index: Arc<Mutex<Repository>>,
    pub credentials: Option<(String, String)>,
    // FIXME: https://github.com/sfackler/r2d2/pull/70
    pub connection_pool: AssertUnwindSafe<DieselPool>,
    pub uploader: Uploader,
    http_client: AssertUnwindSafe<reqwest::Client>,
}

// FIXME: AssertUnwindSafe should be `Clone`, this can be replaced with
// `#[derive(Clone)]` if that is fixed in the standard lib
impl Clone for Environment {
    fn clone(&self) -> Self {
        Self {
            index: self.index.clone(),
            credentials: self.credentials.clone(),
            connection_pool: AssertUnwindSafe(self.connection_pool.0.clone()),
            uploader: self.uploader.clone(),
            http_client: AssertUnwindSafe(self.http_client.0.clone()),
        }
    }
}

impl Environment {
    pub fn new(
        index: Repository,
        credentials: Option<(String, String)>,
        connection_pool: DieselPool,
        uploader: Uploader,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            index: Arc::new(Mutex::new(index)),
            credentials,
            connection_pool: AssertUnwindSafe(connection_pool),
            uploader,
            http_client: AssertUnwindSafe(http_client),
        }
    }

    pub fn credentials(&self) -> Option<(&str, &str)> {
        self.credentials
            .as_ref()
            .map(|(u, p)| (u.as_str(), p.as_str()))
    }

    pub fn connection(&self) -> Result<DieselPooledConn<'_>, PerformError> {
        self.connection_pool
            .get()
            .map_err(|e| CargoErrToStdErr(e).into())
    }

    pub fn lock_index(&self) -> CargoResult<MutexGuard<'_, Repository>> {
        let repo = self.index.lock().unwrap_or_else(PoisonError::into_inner);
        repo.reset_head()?;
        Ok(repo)
    }

    /// Returns a client for making HTTP requests to upload crate files.
    pub(crate) fn http_client(&self) -> &reqwest::Client {
        &self.http_client
    }
}
