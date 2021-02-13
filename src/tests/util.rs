//! This module provides utility types and traits for managing a test session
//!
//! Tests start by using one of the `TestApp` constructors: `init`, `with_proxy`, or `full`.  This returns a
//! `TestAppBuilder` which provides convience methods for creating up to one user, optionally with
//! a token.  The builder methods all return at least an initialized `TestApp` and a
//! `MockAnonymousUser`.  The `MockAnonymousUser` can be used to issue requests in an
//! unauthenticated session.
//!
//! A `TestApp` value provides raw access to the database through the `db` function and can
//! construct new users via the `db_new_user` function.  This function returns a
//! `MockCookieUser`, which can be used to generate one or more tokens via its `db_new_token`
//! function, which in turn returns a `MockTokenUser`.
//!
//! All three user types implement the `RequestHelper` trait which provides convenience methods for
//! constructing requests.  Some of these methods, such as `publish` are expected to fail for an
//! unauthenticated user (or for other reasons) and return a `Response<T>`.  The `Response<T>`
//! provides several functions to check the response status and deserialize the JSON response.
//!
//! `MockCookieUser` and `MockTokenUser` provide an `as_model` function which returns a reference
//! to the underlying database model value (`User` and `ApiToken` respectively).

use crate::{
    builders::PublishBuilder, record, CategoryListResponse, CategoryResponse, CrateList,
    CrateResponse, GoodCrate, OkBool, OwnersResponse, VersionResponse,
};
use cargo_registry::{
    background_jobs::Environment,
    db::DieselPool,
    git::{Credentials, RepositoryConfig},
    models::{ApiToken, CreatedApiToken, User},
    util::AppResponse,
    App, Config,
};
use diesel::PgConnection;
use serde_json::Value;
use std::{marker::PhantomData, rc::Rc, sync::Arc, time::Duration};
use swirl::Runner;

use conduit::{Handler, HandlerResult, Method};
use conduit_cookie::SessionMiddleware;
use conduit_test::MockRequest;

use cargo_registry::git::Repository as WorkerRepository;
use git2::Repository as UpstreamRepository;

use url::Url;

pub use conduit::{header, StatusCode};
use cookie::Cookie;
use std::collections::HashMap;

pub fn init_logger() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .without_time()
        .with_test_writer()
        .try_init();
}

struct TestAppInner {
    app: Arc<App>,
    // The bomb (if created) needs to be held in scope until the end of the test.
    _bomb: Option<record::Bomb>,
    middle: conduit_middleware::MiddlewareBuilder,
    index: Option<UpstreamRepository>,
    runner: Option<Runner<Environment, DieselPool>>,
}

impl Drop for TestAppInner {
    fn drop(&mut self) {
        use diesel::prelude::*;
        use swirl::schema::background_jobs::dsl::*;

        // Avoid a double-panic if the test is already failing
        if std::thread::panicking() {
            return;
        }

        // Lazily run any remaining jobs
        if let Some(runner) = &self.runner {
            runner.run_all_pending_jobs().expect("Could not run jobs");
            runner.check_for_failed_jobs().expect("Failed jobs remain");
        }

        // Manually verify that all jobs have completed successfully
        // This will catch any tests that enqueued a job but forgot to initialize the runner
        let conn = self.app.primary_database.get().unwrap();
        let job_count: i64 = background_jobs.count().get_result(&*conn).unwrap();
        assert_eq!(
            0, job_count,
            "Unprocessed or failed jobs remain in the queue"
        );

        // TODO: If a runner was started, obtain the clone from it and ensure its HEAD matches the upstream index HEAD
    }
}

/// A representation of the app and its database transaction
#[derive(Clone)]
pub struct TestApp(Rc<TestAppInner>);

impl TestApp {
    /// Initialize an application with an `Uploader` that panics
    pub fn init() -> TestAppBuilder {
        init_logger();

        TestAppBuilder {
            config: crate::simple_config(),
            proxy: None,
            bomb: None,
            index: None,
            build_job_runner: false,
        }
    }

    /// Initialize the app and a proxy that can record and playback outgoing HTTP requests
    pub fn with_proxy() -> TestAppBuilder {
        Self::init().with_proxy()
    }

    /// Initialize a full application, with a proxy, index, and background worker
    pub fn full() -> TestAppBuilder {
        Self::with_proxy().with_git_index().with_job_runner()
    }

    /// Obtain the database connection and pass it to the closure
    ///
    /// Within each test, the connection pool only has 1 connection so it is necessary to drop the
    /// connection before making any API calls.  Once the closure returns, the connection is
    /// dropped, ensuring it is returned to the pool and available for any future API calls.
    pub fn db<T, F: FnOnce(&PgConnection) -> T>(&self, f: F) -> T {
        let conn = self.0.app.primary_database.get().unwrap();
        f(&conn)
    }

    /// Create a new user with a verified email address in the database and return a mock user
    /// session
    ///
    /// This method updates the database directly
    pub fn db_new_user(&self, username: &str) -> MockCookieUser {
        use cargo_registry::schema::emails;
        use diesel::prelude::*;

        let user = self.db(|conn| {
            let email = "something@example.com";

            let user = crate::new_user(username)
                .create_or_update(None, conn)
                .unwrap();
            diesel::insert_into(emails::table)
                .values((
                    emails::user_id.eq(user.id),
                    emails::email.eq(email),
                    emails::verified.eq(true),
                ))
                .execute(conn)
                .unwrap();
            user
        });
        MockCookieUser {
            app: TestApp(Rc::clone(&self.0)),
            user,
        }
    }

    /// Obtain a reference to the upstream repository ("the index")
    pub fn upstream_repository(&self) -> &UpstreamRepository {
        self.0.index.as_ref().unwrap()
    }

    /// Obtain a list of crates from the index HEAD
    pub fn crates_from_index_head(&self, path: &str) -> Vec<cargo_registry::git::Crate> {
        let path = std::path::Path::new(path);
        let index = self.upstream_repository();
        let tree = index.head().unwrap().peel_to_tree().unwrap();
        let blob = tree
            .get_path(path)
            .unwrap()
            .to_object(&index)
            .unwrap()
            .peel_to_blob()
            .unwrap();
        let content = blob.content();

        // The index format consists of one JSON object per line
        // It is not a JSON array
        let lines = std::str::from_utf8(content).unwrap().lines();
        lines
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub fn run_pending_background_jobs(&self) {
        let runner = &self.0.runner;
        let runner = runner.as_ref().expect("Index has not been initialized");

        runner.run_all_pending_jobs().expect("Could not run jobs");
        runner
            .check_for_failed_jobs()
            .expect("Could not determine if jobs failed");
    }

    /// Obtain a reference to the inner `App` value
    pub fn as_inner(&self) -> &App {
        &self.0.app
    }

    /// Obtain a reference to the inner middleware builder
    pub fn as_middleware(&self) -> &conduit_middleware::MiddlewareBuilder {
        &self.0.middle
    }
}

/// This function can be used to create a `Cookie` header for mock requests that
/// include cookie-based authentication.
///
/// ```
/// let cookie = encode_session_header(session_key, user_id);
/// request.header(header::COOKIE, &cookie);
/// ```
///
/// The implementation matches roughly what is happening inside of the
/// `SessionMiddleware` from `conduit_cookie`.
pub fn encode_session_header(session_key: &str, user_id: i32) -> String {
    let cookie_name = "cargo_session";
    let cookie_key = cookie::Key::derive_from(session_key.as_bytes());

    // build session data map
    let mut map = HashMap::new();
    map.insert("user_id".into(), user_id.to_string());

    // encode the map into a cookie value string
    let session_middleware = SessionMiddleware::new(cookie_name, cookie_key.clone(), false);
    let encoded = session_middleware.encode(&map);

    // put the cookie into a signed cookie jar
    let cookie = Cookie::build(cookie_name, encoded).finish();
    let mut jar = cookie::CookieJar::new();
    jar.signed(&cookie_key).add(cookie);

    // read the raw cookie from the cookie jar
    jar.get(&cookie_name).unwrap().to_string()
}

pub struct TestAppBuilder {
    config: Config,
    proxy: Option<String>,
    bomb: Option<record::Bomb>,
    index: Option<UpstreamRepository>,
    build_job_runner: bool,
}

impl TestAppBuilder {
    /// Create a `TestApp` with an empty database
    pub fn empty(self) -> (TestApp, MockAnonymousUser) {
        use crate::git;

        let (app, middle) = crate::build_app(self.config, self.proxy);

        let runner = if self.build_job_runner {
            let repository_config = RepositoryConfig {
                index_location: Url::from_file_path(&git::bare()).unwrap(),
                credentials: Credentials::Missing,
            };
            let index = WorkerRepository::open(&repository_config).expect("Could not clone index");
            let environment = Environment::new(
                index,
                app.config.uploader.clone(),
                app.http_client().clone(),
            );

            Some(
                Runner::builder(environment)
                    // We only have 1 connection in tests, so trying to run more than
                    // 1 job concurrently will just block
                    .thread_count(1)
                    .connection_pool(app.primary_database.clone())
                    .job_start_timeout(Duration::from_secs(5))
                    .build(),
            )
        } else {
            None
        };

        let test_app_inner = TestAppInner {
            app,
            _bomb: self.bomb,
            middle,
            index: self.index,
            runner,
        };
        let test_app = TestApp(Rc::new(test_app_inner));
        let anon = MockAnonymousUser {
            app: test_app.clone(),
        };
        (test_app, anon)
    }

    /// Create a proxy for use with this app
    pub fn with_proxy(mut self) -> Self {
        let (proxy, bomb) = record::proxy();
        self.proxy = Some(proxy);
        self.bomb = Some(bomb);
        self
    }

    // Create a `TestApp` with a database including a default user
    pub fn with_user(self) -> (TestApp, MockAnonymousUser, MockCookieUser) {
        let (app, anon) = self.empty();
        let user = app.db_new_user("foo");
        (app, anon, user)
    }

    /// Create a `TestApp` with a database including a default user and its token
    pub fn with_token(self) -> (TestApp, MockAnonymousUser, MockCookieUser, MockTokenUser) {
        let (app, anon) = self.empty();
        let user = app.db_new_user("foo");
        let token = user.db_new_token("bar");
        (app, anon, user, token)
    }

    pub fn with_config(mut self, f: impl FnOnce(&mut Config)) -> Self {
        f(&mut self.config);
        self
    }

    pub fn with_publish_rate_limit(self, rate: Duration, burst: i32) -> Self {
        self.with_config(|config| {
            config.publish_rate_limit.rate = rate;
            config.publish_rate_limit.burst = burst;
        })
    }

    pub fn with_git_index(mut self) -> Self {
        use crate::git;

        git::init();

        let thread_local_path = git::bare();
        self.index = Some(UpstreamRepository::open_bare(thread_local_path).unwrap());
        self
    }

    pub fn with_job_runner(mut self) -> Self {
        self.build_job_runner = true;
        self
    }
}

/// A collection of helper methods for the 3 authentication types
///
/// Helper methods go through public APIs, and should not modify the database directly
pub trait RequestHelper {
    fn request_builder(&self, method: Method, path: &str) -> MockRequest;
    fn app(&self) -> &TestApp;

    /// Run a request
    fn run<T>(&self, mut request: MockRequest) -> Response<T>
    where
        T: serde::de::DeserializeOwned,
    {
        Response::new(self.app().as_middleware().call(&mut request))
    }

    /// Create a get request
    fn get_request(&self, path: &str) -> MockRequest {
        self.request_builder(Method::GET, path)
    }

    /// Issue a GET request
    fn get<T>(&self, path: &str) -> Response<T>
    where
        for<'de> T: serde::Deserialize<'de>,
    {
        self.run(self.get_request(path))
    }

    /// Issue a GET request that includes query parameters
    fn get_with_query<T>(&self, path: &str, query: &str) -> Response<T>
    where
        for<'de> T: serde::Deserialize<'de>,
    {
        let mut request = self.request_builder(Method::GET, path);
        request.with_query(query);
        self.run(request)
    }

    /// Issue a PUT request
    fn put<T>(&self, path: &str, body: &[u8]) -> Response<T>
    where
        for<'de> T: serde::Deserialize<'de>,
    {
        let mut request = self.request_builder(Method::PUT, path);
        request.with_body(body);
        self.run(request)
    }

    /// Issue a DELETE request
    fn delete<T>(&self, path: &str) -> Response<T>
    where
        for<'de> T: serde::Deserialize<'de>,
    {
        let request = self.request_builder(Method::DELETE, path);
        self.run(request)
    }

    /// Issue a DELETE request with a body... yes we do it, for crate owner removal
    fn delete_with_body<T>(&self, path: &str, body: &[u8]) -> Response<T>
    where
        for<'de> T: serde::Deserialize<'de>,
    {
        let mut request = self.request_builder(Method::DELETE, path);
        request.with_body(body);
        self.run(request)
    }

    /// Search for crates matching a query string
    fn search(&self, query: &str) -> CrateList {
        self.get_with_query("/api/v1/crates", query).good()
    }

    /// Search for crates owned by the specified user.
    fn search_by_user_id(&self, id: i32) -> CrateList {
        self.search(&format!("user_id={}", id))
    }

    /// Enqueue a crate for publishing
    ///
    /// The publish endpoint will enqueue a background job to update the index.  A test must run
    /// any pending background jobs if it intends to observe changes to the index.
    ///
    /// Any pending jobs are run when the `TestApp` is dropped to ensure that the test fails unless
    /// all background tasks complete successfully.
    fn enqueue_publish(&self, publish_builder: PublishBuilder) -> Response<GoodCrate> {
        self.put("/api/v1/crates/new", &publish_builder.body())
    }

    /// Request the JSON used for a crate's page
    fn show_crate(&self, krate_name: &str) -> CrateResponse {
        let url = format!("/api/v1/crates/{}", krate_name);
        self.get(&url).good()
    }

    /// Request the JSON used to list a crate's owners
    fn show_crate_owners(&self, krate_name: &str) -> OwnersResponse {
        let url = format!("/api/v1/crates/{}/owners", krate_name);
        self.get(&url).good()
    }

    /// Request the JSON used for a crate version's page
    fn show_version(&self, krate_name: &str, version: &str) -> VersionResponse {
        let url = format!("/api/v1/crates/{}/{}", krate_name, version);
        self.get(&url).good()
    }

    fn show_category(&self, category_name: &str) -> CategoryResponse {
        let url = format!("/api/v1/categories/{}", category_name);
        self.get(&url).good()
    }

    fn show_category_list(&self) -> CategoryListResponse {
        let url = "/api/v1/categories";
        self.get(url).good()
    }
}

fn req(method: conduit::Method, path: &str) -> MockRequest {
    let mut request = MockRequest::new(method, path);
    request.header(header::USER_AGENT, "conduit-test");
    request
}

/// A type that can generate unauthenticated requests
pub struct MockAnonymousUser {
    app: TestApp,
}

impl RequestHelper for MockAnonymousUser {
    fn request_builder(&self, method: Method, path: &str) -> MockRequest {
        req(method, path)
    }

    fn app(&self) -> &TestApp {
        &self.app
    }
}

/// A type that can generate cookie authenticated requests
///
/// The `user.id` value is directly injected into a request extension and thus the conduit_cookie
/// session logic is not exercised.
pub struct MockCookieUser {
    app: TestApp,
    user: User,
}

impl RequestHelper for MockCookieUser {
    fn request_builder(&self, method: Method, path: &str) -> MockRequest {
        let session_key = &self.app.as_inner().session_key;
        let cookie = encode_session_header(session_key, self.user.id);

        let mut request = req(method, path);
        request.header(header::COOKIE, &cookie);
        request
    }

    fn app(&self) -> &TestApp {
        &self.app
    }
}

impl MockCookieUser {
    /// Creates an instance from a database `User` instance
    pub fn new(app: &TestApp, user: User) -> Self {
        Self {
            app: TestApp(Rc::clone(&app.0)),
            user,
        }
    }

    /// Returns a reference to the database `User` model
    pub fn as_model(&self) -> &User {
        &self.user
    }

    /// Creates a token and wraps it in a helper struct
    ///
    /// This method updates the database directly
    pub fn db_new_token(&self, name: &str) -> MockTokenUser {
        let token = self
            .app
            .db(|conn| ApiToken::insert(conn, self.user.id, name).unwrap());
        MockTokenUser {
            app: TestApp(Rc::clone(&self.app.0)),
            token,
        }
    }
}

/// A type that can generate token authenticated requests
pub struct MockTokenUser {
    app: TestApp,
    token: CreatedApiToken,
}

impl RequestHelper for MockTokenUser {
    fn request_builder(&self, method: Method, path: &str) -> MockRequest {
        let mut request = req(method, path);
        request.header(header::AUTHORIZATION, &self.token.plaintext);
        request
    }

    fn app(&self) -> &TestApp {
        &self.app
    }
}

impl MockTokenUser {
    /// Returns a reference to the database `ApiToken` model
    pub fn as_model(&self) -> &ApiToken {
        &self.token.model
    }

    pub fn plaintext(&self) -> &str {
        &self.token.plaintext
    }

    /// Add to the specified crate the specified owners.
    pub fn add_named_owners(&self, krate_name: &str, owners: &[&str]) -> Response<OkBool> {
        self.modify_owners(krate_name, owners, Self::put)
    }

    /// Add a single owner to the specified crate.
    pub fn add_named_owner(&self, krate_name: &str, owner: &str) -> Response<OkBool> {
        self.add_named_owners(krate_name, &[owner])
    }

    /// Remove from the specified crate the specified owners.
    pub fn remove_named_owners(&self, krate_name: &str, owners: &[&str]) -> Response<OkBool> {
        self.modify_owners(krate_name, owners, Self::delete_with_body)
    }

    /// Remove a single owner to the specified crate.
    pub fn remove_named_owner(&self, krate_name: &str, owner: &str) -> Response<OkBool> {
        self.remove_named_owners(krate_name, &[owner])
    }

    fn modify_owners<F>(&self, krate_name: &str, owners: &[&str], method: F) -> Response<OkBool>
    where
        F: Fn(&MockTokenUser, &str, &[u8]) -> Response<OkBool>,
    {
        let url = format!("/api/v1/crates/{}/owners", krate_name);
        let body = json!({ "owners": owners }).to_string();
        method(&self, &url, body.as_bytes())
    }

    /// Add a user as an owner for a crate.
    pub fn add_user_owner(&self, krate_name: &str, username: &str) {
        self.add_named_owner(krate_name, username).good();
    }
}

#[derive(Deserialize, Debug)]
pub struct Error {
    pub detail: String,
}

/// A type providing helper methods for working with responses
#[must_use]
pub struct Response<T> {
    response: AppResponse,
    return_type: PhantomData<T>,
}

impl<T> Response<T>
where
    for<'de> T: serde::Deserialize<'de>,
{
    fn new(response: HandlerResult) -> Self {
        Self {
            response: assert_ok!(response),
            return_type: PhantomData,
        }
    }

    #[track_caller]
    pub fn json(mut self) -> Value {
        crate::json(&mut self.response)
    }

    /// Assert that the response is good and deserialize the message
    #[track_caller]
    pub fn good(mut self) -> T {
        if !self.status().is_success() {
            panic!("bad response: {:?}", self.status());
        }
        crate::json(&mut self.response)
    }

    pub fn status(&self) -> StatusCode {
        self.response.status()
    }

    #[track_caller]
    pub fn assert_redirect_ends_with(&self, target: &str) -> &Self {
        assert!(self
            .response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with(target));
        self
    }
}

impl Response<()> {
    /// Assert that the status code is 404
    #[track_caller]
    pub fn assert_not_found(&self) {
        assert_eq!(StatusCode::NOT_FOUND, self.status());
    }

    /// Assert that the status code is 403
    #[track_caller]
    pub fn assert_forbidden(&self) {
        assert_eq!(StatusCode::FORBIDDEN, self.status());
    }
}
