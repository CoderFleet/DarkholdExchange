//! Login, sign-up and "who am I" for the gateway. Contract: `docs/AUTH.md`.
//!
//! Users are kept in memory. If a [`UserRepo`] is attached (the gateway does this when
//! `DATABASE_URL` is set) every new account is also saved there, and the saved accounts are
//! loaded back at start-up, so accounts and their ids survive a restart. The password is
//! never stored: only an Argon2id hash. A session is a signed JWT sent as
//! `Authorization: Bearer <token>`.
//!
//! The gateway adds this with one line, `.merge(dx_auth::router(auth_state))`, and any
//! handler can ask for the caller with the [`AuthUser`] extractor.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRef, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

/// Same identity type as `dx_types::UserId`.
pub type UserId = u64;

/// How long a token stays valid (docs/AUTH.md A6). `DX_JWT_TTL_SECS` overrides it.
const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(2 * 60 * 60);
/// First id handed to a new sign-up. Lower ids are reserved for seeded accounts.
const FIRST_SIGNUP_ID: UserId = 200;
/// Failed logins allowed per username inside `LOGIN_WINDOW`, then `429`.
const MAX_FAILED_LOGINS: usize = 5;
const LOGIN_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Admin,
    Issuer,
}

impl Role {
    /// The word used for this role when it is saved (the same as in the JSON).
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Admin => "admin",
            Role::Issuer => "issuer",
        }
    }

    /// The reverse of [`Role::as_str`]. `None` for any other word.
    pub fn parse(text: &str) -> Option<Role> {
        match text {
            "user" => Some(Role::User),
            "admin" => Some(Role::Admin),
            "issuer" => Some(Role::Issuer),
            _ => None,
        }
    }
}

/// What the client is told about a user. Never contains the password or its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicUser {
    pub user_id: UserId,
    pub username: String,
    pub role: Role,
}

#[derive(Debug, Serialize)]
struct AuthResponse {
    token: String,
    user: PublicUser,
}

#[derive(Debug, Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: UserId,
    role: Role,
    iat: u64,
    exp: u64,
}

/// An error that renders as `{ "reason": "<code>" }` with the right HTTP status.
#[derive(Debug)]
pub struct AuthError {
    status: StatusCode,
    reason: &'static str,
    retry_after: Option<u64>,
}

impl AuthError {
    fn new(status: StatusCode, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            retry_after: None,
        }
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized")
    }
    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden")
    }
    fn invalid_request() -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request")
    }
    fn invalid_credentials() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "invalid_credentials")
    }
    fn username_taken() -> Self {
        Self::new(StatusCode::CONFLICT, "username_taken")
    }
    fn rate_limited(retry_after: u64) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            reason: "rate_limited",
            retry_after: Some(retry_after),
        }
    }
    fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({ "reason": self.reason }));
        let mut response = (self.status, body).into_response();
        if let Some(seconds) = self.retry_after
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

/// One account as it is saved: the password is only ever the Argon2id hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredUser {
    pub user_id: UserId,
    pub username: String,
    pub role: Role,
    pub password_hash: String,
}

/// Why saving an account failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoError {
    /// Somebody already has this username.
    UsernameTaken,
    /// Anything else (the database is down, ...). The text is only for the server log.
    Failed(String),
}

/// What a save or load returns. Written out by hand so the trait can be a `dyn` object.
pub type RepoFuture<T> = Pin<Box<dyn Future<Output = Result<T, RepoError>> + Send>>;

/// Where accounts are saved. The `dx-store` crate implements it with PostgreSQL; the tests
/// use a fake. Attach one with [`AuthState::attach_repo`].
pub trait UserRepo: Send + Sync {
    /// Saves a new account. `Err(UsernameTaken)` if the name or id is already used.
    fn insert(&self, user: StoredUser) -> RepoFuture<()>;
    /// Saves an account, replacing the saved one with the same username. Used for the
    /// admin and issuer accounts, whose password comes from the environment at every start.
    fn upsert(&self, user: StoredUser) -> RepoFuture<()>;
}

type UserRecord = StoredUser;

#[derive(Default)]
struct Store {
    by_name: HashMap<String, UserRecord>,
    name_by_id: HashMap<UserId, String>,
    next_id: UserId,
}

struct Inner {
    encoding: EncodingKey,
    decoding: DecodingKey,
    ttl: Duration,
    store: Mutex<Store>,
    /// Where new accounts are saved. Empty (memory only) until `attach_repo` is called.
    repo: OnceLock<Arc<dyn UserRepo>>,
    /// Admin and issuer accounts whose password came from the environment: saved at attach.
    durable_seeds: Mutex<Vec<StoredUser>>,
    /// A real hash of a throwaway password. Checked when the username does not exist, so an
    /// unknown user takes as long as a wrong password (no username guessing by timing).
    dummy_hash: OnceLock<String>,
    failed_logins: Mutex<HashMap<String, Vec<Instant>>>,
    /// When true, `x-dev-user` may still be used by the gateway (`DX_DEV_AUTH`, default on).
    dev_auth: bool,
}

/// Shared auth state. Cheap to clone.
#[derive(Clone)]
pub struct AuthState(Arc<Inner>);

fn argon2() -> Argon2<'static> {
    if cfg!(test) {
        // Tiny cost so the tests run fast. Real hashes use the defaults below.
        Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            Params::new(8, 1, 1, None).expect("params"),
        )
    } else {
        Argon2::default()
    }
}

fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    argon2()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AuthError::internal())
}

fn password_matches(password: &str, stored_hash: &str) -> bool {
    match PasswordHash::new(stored_hash) {
        Ok(parsed) => argon2()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn valid_username(name: &str) -> bool {
    (3..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn valid_password(password: &str) -> bool {
    (8..=128).contains(&password.chars().count())
}

impl AuthState {
    /// `secret` signs every token: at least 32 bytes, kept out of git.
    pub fn new(secret: &[u8], ttl: Duration, dev_auth: bool) -> Self {
        Self(Arc::new(Inner {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
            ttl,
            store: Mutex::new(Store {
                next_id: FIRST_SIGNUP_ID,
                ..Store::default()
            }),
            repo: OnceLock::new(),
            durable_seeds: Mutex::new(Vec::new()),
            dummy_hash: OnceLock::new(),
            failed_logins: Mutex::new(HashMap::new()),
            dev_auth,
        }))
    }

    /// Reads the settings from environment variables.
    ///
    /// * `DX_JWT_SECRET`: signing secret (32+ chars). If unset, a random one is made, so every
    ///   restart logs everybody out. Set it for any real run.
    /// * `DX_JWT_TTL_SECS`: how long a login lasts, in seconds (default 2 hours).
    /// * `DX_DEV_AUTH`: `0` turns the `x-dev-user` fallback off (real runs). Default on.
    /// * `DX_ADMIN_PASSWORD`, `DX_ISSUER_PASSWORD`: passwords of the seeded `admin` and
    ///   `issuer` accounts. With dev auth on and no value, a demo password is used.
    pub fn from_env() -> Self {
        let dev_auth = std::env::var("DX_DEV_AUTH")
            .map(|v| v != "0")
            .unwrap_or(true);
        let secret = match std::env::var("DX_JWT_SECRET") {
            Ok(value) if value.len() >= 32 => value.into_bytes(),
            Ok(_) => panic!("DX_JWT_SECRET must be at least 32 characters"),
            Err(_) => {
                eprintln!(
                    "auth: DX_JWT_SECRET not set, using a random secret (logins end on restart)"
                );
                let mut bytes = [0u8; 48];
                OsRng.fill_bytes(&mut bytes);
                bytes.to_vec()
            }
        };
        let ttl = std::env::var("DX_JWT_TTL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| *seconds > 0)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_TOKEN_TTL);
        let state = Self::new(&secret, ttl, dev_auth);

        // The bool says whether the password came from the environment. Only those accounts
        // are saved to the database: a demo password must never be saved.
        let seed = |var: &str, demo: &str| -> Option<(String, bool)> {
            match std::env::var(var) {
                Ok(value) => Some((value, true)),
                Err(_) if dev_auth => Some((demo.to_string(), false)),
                Err(_) => None,
            }
        };
        let mut seeded = vec![];
        if let Some((password, from_env)) = seed("DX_ADMIN_PASSWORD", "admin-pass-1") {
            seeded.push((
                state.seed_user(100, "admin", &password, Role::Admin),
                from_env,
            ));
        }
        if let Some((password, from_env)) = seed("DX_ISSUER_PASSWORD", "issuer-pass-1") {
            seeded.push((
                state.seed_user(101, "issuer", &password, Role::Issuer),
                from_env,
            ));
        }
        for (record, from_env) in seeded {
            if from_env {
                state.0.durable_seeds.lock().expect("seeds").push(record);
            }
        }
        // Make the dummy hash now, so the first login with an unknown name is not slower.
        state.dummy_hash();
        if dev_auth {
            // The demo trader the simulator and the market page already use.
            state.seed_user(1, "trader1", "trader-pass-1", Role::User);
        }
        state
    }

    /// Adds an account with a fixed id and role (used for admin, issuer and demo users).
    /// Returns the account as it would be saved.
    pub fn seed_user(
        &self,
        user_id: UserId,
        username: &str,
        password: &str,
        role: Role,
    ) -> StoredUser {
        let password_hash = hash_password(password).expect("hash seed password");
        let record = UserRecord {
            user_id,
            username: username.to_string(),
            role,
            password_hash,
        };
        let mut store = self.0.store.lock().expect("auth store");
        store.name_by_id.insert(user_id, username.to_string());
        store.by_name.insert(username.to_string(), record.clone());
        record
    }

    /// Connects the database. `saved` are the accounts it already holds: they are loaded,
    /// and new sign-ups continue after the highest saved id, so an id is never handed out
    /// twice. Accounts seeded in memory keep their own entry (the environment wins). The
    /// admin and issuer accounts whose password came from the environment are saved.
    /// Call it once, before the server starts taking requests.
    pub async fn attach_repo(
        &self,
        repo: Arc<dyn UserRepo>,
        saved: Vec<StoredUser>,
    ) -> Result<(), String> {
        {
            let mut store = self.0.store.lock().expect("auth store");
            for user in saved {
                store.next_id = store.next_id.max(user.user_id + 1);
                if store.by_name.contains_key(&user.username) {
                    continue;
                }
                store.name_by_id.insert(user.user_id, user.username.clone());
                store.by_name.insert(user.username.clone(), user);
            }
        }
        let seeds = self.0.durable_seeds.lock().expect("seeds").clone();
        for seed in seeds {
            repo.upsert(seed)
                .await
                .map_err(|error| format!("saving a seeded account: {error:?}"))?;
        }
        self.0
            .repo
            .set(repo)
            .map_err(|_| "a user repo is already attached".to_string())
    }

    /// True while the gateway may still accept the `x-dev-user` header.
    pub fn dev_auth_enabled(&self) -> bool {
        self.0.dev_auth
    }

    /// Every known user id, sorted ascending. Solvency snapshots use this to
    /// include zero-balance users, so every signed-up user can obtain a proof.
    pub fn user_ids(&self) -> Vec<UserId> {
        let store = self.0.store.lock().expect("auth store");
        let mut ids: Vec<_> = store.name_by_id.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// The id of the account with this username, if there is one. Names are stored in lowercase,
    /// so capitals and spaces around the name do not matter. The issuer endpoint uses it, so
    /// an issuer can name the person to credit instead of knowing their number.
    pub fn user_id_of(&self, username: &str) -> Option<UserId> {
        let store = self.0.store.lock().expect("auth store");
        store
            .by_name
            .get(&username.trim().to_lowercase())
            .map(|record| record.user_id)
    }

    /// Signs a token for a user. The login handlers use it; tests in other crates can too.
    pub fn issue_token(&self, user: &PublicUser) -> Result<String, AuthError> {
        let iat = now_secs();
        let claims = Claims {
            sub: user.user_id,
            role: user.role,
            iat,
            exp: iat + self.0.ttl.as_secs(),
        };
        encode(&Header::default(), &claims, &self.0.encoding).map_err(|_| AuthError::internal())
    }

    /// Checks a token's signature and expiry. Used by the extractor and by WebSocket
    /// upgrades (`?token=`).
    pub fn verify_token(&self, token: &str) -> Result<AuthUser, AuthError> {
        let mut validation = Validation::default();
        validation.leeway = 0;
        let data = decode::<Claims>(token, &self.0.decoding, &validation)
            .map_err(|_| AuthError::unauthorized())?;
        Ok(AuthUser {
            user_id: data.claims.sub,
            role: data.claims.role,
        })
    }

    /// The hash used when the username does not exist. It is made once, on first use.
    fn dummy_hash(&self) -> &str {
        self.0
            .dummy_hash
            .get_or_init(|| hash_password("not-a-real-password").expect("hash the dummy password"))
    }

    fn login_blocked(&self, username: &str) -> Option<u64> {
        let mut map = self.0.failed_logins.lock().expect("login limiter");
        let attempts = map.get_mut(username)?;
        attempts.retain(|at| at.elapsed() < LOGIN_WINDOW);
        if attempts.len() >= MAX_FAILED_LOGINS {
            let oldest = attempts.first()?.elapsed();
            return Some(LOGIN_WINDOW.saturating_sub(oldest).as_secs().max(1));
        }
        None
    }

    fn record_failed_login(&self, username: &str) {
        let mut map = self.0.failed_logins.lock().expect("login limiter");
        if map.len() > 10_000 {
            map.retain(|_, attempts| attempts.iter().any(|at| at.elapsed() < LOGIN_WINDOW));
        }
        map.entry(username.to_string())
            .or_default()
            .push(Instant::now());
    }

    fn clear_failed_logins(&self, username: &str) {
        self.0
            .failed_logins
            .lock()
            .expect("login limiter")
            .remove(username);
    }
}

/// The verified caller. Add it as a handler argument and the request needs a valid token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthUser {
    pub user_id: UserId,
    pub role: Role,
}

impl AuthUser {
    /// `Ok` only for one of the listed roles, otherwise `403 forbidden`.
    pub fn require_role(&self, allowed: &[Role]) -> Result<(), AuthError> {
        if allowed.contains(&self.role) {
            Ok(())
        } else {
            Err(AuthError::forbidden())
        }
    }
}

impl<S> FromRequestParts<S> for AuthUser
where
    AuthState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth = AuthState::from_ref(state);
        let value = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(AuthError::unauthorized)?;
        let token = value
            .strip_prefix("Bearer ")
            .ok_or_else(AuthError::unauthorized)?;
        auth.verify_token(token.trim())
    }
}

/// `POST /api/auth/signup`, `POST /api/auth/login`, `GET /api/me`.
pub fn router(state: AuthState) -> Router {
    Router::new()
        .route("/api/auth/signup", post(signup))
        .route("/api/auth/login", post(login))
        .route("/api/me", get(me))
        .with_state(state)
}

fn read_credentials(
    body: Result<Json<Credentials>, JsonRejection>,
) -> Result<Credentials, AuthError> {
    body.map(|Json(c)| c)
        .map_err(|_| AuthError::invalid_request())
}

async fn signup(
    State(state): State<AuthState>,
    body: Result<Json<Credentials>, JsonRejection>,
) -> Result<(StatusCode, Json<AuthResponse>), AuthError> {
    let credentials = read_credentials(body)?;
    let username = credentials.username.trim().to_lowercase();
    if !valid_username(&username) || !valid_password(&credentials.password) {
        return Err(AuthError::invalid_request());
    }
    // Hashing is slow on purpose, so keep it off the async worker threads.
    let password = credentials.password;
    let password_hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|_| AuthError::internal())??;

    let user = match state.0.repo.get() {
        // Saved first, then remembered: if the save fails, nothing was created.
        Some(repo) => {
            let user_id = {
                let mut store = state.0.store.lock().expect("auth store");
                if store.by_name.contains_key(&username) {
                    return Err(AuthError::username_taken());
                }
                let user_id = store.next_id;
                store.next_id += 1;
                user_id
            };
            // Sign-up always makes a normal user (docs/AUTH.md A3).
            let record = UserRecord {
                user_id,
                username: username.clone(),
                role: Role::User,
                password_hash,
            };
            repo.insert(record.clone())
                .await
                .map_err(|error| match error {
                    RepoError::UsernameTaken => AuthError::username_taken(),
                    RepoError::Failed(why) => {
                        eprintln!("auth: could not save a new account: {why}");
                        AuthError::internal()
                    }
                })?;
            let mut store = state.0.store.lock().expect("auth store");
            store.name_by_id.insert(user_id, username.clone());
            store.by_name.insert(username.clone(), record);
            PublicUser {
                user_id,
                username,
                role: Role::User,
            }
        }
        None => {
            let mut store = state.0.store.lock().expect("auth store");
            if store.by_name.contains_key(&username) {
                return Err(AuthError::username_taken());
            }
            let user_id = store.next_id;
            store.next_id += 1;
            // Sign-up always makes a normal user (docs/AUTH.md A3).
            let record = UserRecord {
                user_id,
                username: username.clone(),
                role: Role::User,
                password_hash,
            };
            store.name_by_id.insert(user_id, username.clone());
            store.by_name.insert(username.clone(), record);
            PublicUser {
                user_id,
                username,
                role: Role::User,
            }
        }
    };
    let token = state.issue_token(&user)?;
    Ok((StatusCode::CREATED, Json(AuthResponse { token, user })))
}

async fn login(
    State(state): State<AuthState>,
    body: Result<Json<Credentials>, JsonRejection>,
) -> Result<Json<AuthResponse>, AuthError> {
    let credentials = read_credentials(body)?;
    let username = credentials.username.trim().to_lowercase();
    if username.is_empty() || username.len() > 64 || credentials.password.len() > 1024 {
        return Err(AuthError::invalid_request());
    }
    if let Some(wait) = state.login_blocked(&username) {
        return Err(AuthError::rate_limited(wait));
    }

    let record = state
        .0
        .store
        .lock()
        .expect("auth store")
        .by_name
        .get(&username)
        .cloned();
    // Always do one hash check, real or dummy, so the time does not reveal if the user exists.
    let real_hash = record.as_ref().map(|r| r.password_hash.clone());
    let password = credentials.password;
    let checker = state.clone();
    let matched = tokio::task::spawn_blocking(move || {
        let hash = real_hash.as_deref().unwrap_or_else(|| checker.dummy_hash());
        password_matches(&password, hash)
    })
    .await
    .map_err(|_| AuthError::internal())?;

    match record {
        Some(record) if matched => {
            state.clear_failed_logins(&username);
            let user = PublicUser {
                user_id: record.user_id,
                username: record.username,
                role: record.role,
            };
            let token = state.issue_token(&user)?;
            Ok(Json(AuthResponse { token, user }))
        }
        _ => {
            state.record_failed_login(&username);
            Err(AuthError::invalid_credentials())
        }
    }
}

async fn me(
    State(state): State<AuthState>,
    caller: AuthUser,
) -> Result<Json<PublicUser>, AuthError> {
    let store = state.0.store.lock().expect("auth store");
    let username = store
        .name_by_id
        .get(&caller.user_id)
        .ok_or_else(AuthError::unauthorized)?;
    let record = store
        .by_name
        .get(username)
        .ok_or_else(AuthError::unauthorized)?;
    Ok(Json(PublicUser {
        user_id: record.user_id,
        username: record.username.clone(),
        role: record.role,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn state() -> AuthState {
        let state = AuthState::new(
            b"test-secret-test-secret-test-secret-123",
            DEFAULT_TOKEN_TTL,
            true,
        );
        state.seed_user(100, "admin", "admin-pass-1", Role::Admin);
        state
    }

    async fn call(
        app: &Router,
        request: Request<Body>,
    ) -> (StatusCode, serde_json::Value, axum::http::HeaderMap) {
        let response = app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json, headers)
    }

    fn post_json(path: &str, body: serde_json::Value) -> Request<Body> {
        Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get_me(token: Option<&str>) -> Request<Body> {
        let mut builder = Request::get("/api/me");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn user_ids_are_sorted_for_solvency_snapshots() {
        let state = state();
        assert_eq!(state.user_ids(), vec![100]);
        state.seed_user(7, "seven", "password-123", Role::User);
        assert_eq!(state.user_ids(), vec![7, 100]);
    }

    #[tokio::test]
    async fn signup_then_me() {
        let app = router(state());
        let (status, body, _) = call(
            &app,
            post_json(
                "/api/auth/signup",
                serde_json::json!({"username": "Stuti_1", "password": "password-123"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["user"]["username"], "stuti_1");
        assert_eq!(body["user"]["role"], "user");
        assert_eq!(body["user"]["user_id"], 200);
        assert!(body["user"].get("password").is_none());
        let token = body["token"].as_str().unwrap().to_string();
        let (status, me, _) = call(&app, get_me(Some(&token))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(me, body["user"]);
    }

    #[tokio::test]
    async fn a_username_is_looked_up_ignoring_capitals_and_spaces() {
        let state = fresh_state();
        let app = router(state.clone());
        let (status, _) = signup(&app, "Carol").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(state.user_id_of("carol"), Some(200));
        assert_eq!(state.user_id_of("  CAROL "), Some(200));
        assert_eq!(state.user_id_of("nobody"), None);
    }

    #[tokio::test]
    async fn login_works_and_errors_look_the_same() {
        let app = router(state());
        let (status, body, _) = call(
            &app,
            post_json(
                "/api/auth/login",
                serde_json::json!({"username": "admin", "password": "admin-pass-1"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["user"]["role"], "admin");

        let (s1, b1, _) = call(
            &app,
            post_json(
                "/api/auth/login",
                serde_json::json!({"username": "admin", "password": "wrong-pass-1"}),
            ),
        )
        .await;
        let (s2, b2, _) = call(
            &app,
            post_json(
                "/api/auth/login",
                serde_json::json!({"username": "nobody", "password": "wrong-pass-1"}),
            ),
        )
        .await;
        assert_eq!(
            (s1, s2),
            (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
        );
        assert_eq!(b1, b2);
        assert_eq!(b1["reason"], "invalid_credentials");
    }

    #[tokio::test]
    async fn signup_rules() {
        let app = router(state());
        let (status, body, _) = call(
            &app,
            post_json(
                "/api/auth/signup",
                serde_json::json!({"username": "ADMIN", "password": "password-123"}),
            ),
        )
        .await;
        assert_eq!(
            (status, body["reason"].as_str()),
            (StatusCode::CONFLICT, Some("username_taken"))
        );
        for (name, password) in [
            ("ab", "password-123"),
            ("has space", "password-123"),
            ("good_name", "short"),
        ] {
            let (status, body, _) = call(
                &app,
                post_json(
                    "/api/auth/signup",
                    serde_json::json!({"username": name, "password": password}),
                ),
            )
            .await;
            assert_eq!(
                (status, body["reason"].as_str()),
                (StatusCode::BAD_REQUEST, Some("invalid_request"))
            );
        }
        let bad_body = Request::post("/api/auth/signup")
            .header("content-type", "application/json")
            .body(Body::from("not json"))
            .unwrap();
        let (status, body, _) = call(&app, bad_body).await;
        assert_eq!(
            (status, body["reason"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_request"))
        );
    }

    #[tokio::test]
    async fn me_needs_a_valid_token() {
        let app = router(state());
        let (status, body, _) = call(&app, get_me(None)).await;
        assert_eq!(
            (status, body["reason"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("unauthorized"))
        );
        let (status, _, _) = call(&app, get_me(Some("junk"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // A token signed with a different secret must be refused.
        let other = AuthState::new(
            b"another-secret-another-secret-12345",
            DEFAULT_TOKEN_TTL,
            true,
        );
        let forged = other
            .issue_token(&PublicUser {
                user_id: 100,
                username: "admin".into(),
                role: Role::Admin,
            })
            .unwrap();
        let (status, _, _) = call(&app, get_me(Some(&forged))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn expired_token_is_refused() {
        let state = state();
        let claims = Claims {
            sub: 100,
            role: Role::Admin,
            iat: now_secs() - 100,
            exp: now_secs() - 10,
        };
        let token = encode(&Header::default(), &claims, &state.0.encoding).unwrap();
        assert!(state.verify_token(&token).is_err());
    }

    #[tokio::test]
    async fn too_many_wrong_passwords_are_rate_limited() {
        let app = router(state());
        for _ in 0..MAX_FAILED_LOGINS {
            let (status, _, _) = call(
                &app,
                post_json(
                    "/api/auth/login",
                    serde_json::json!({"username": "admin", "password": "wrong-pass-1"}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        // Even the right password is refused while blocked.
        let (status, body, headers) = call(
            &app,
            post_json(
                "/api/auth/login",
                serde_json::json!({"username": "admin", "password": "admin-pass-1"}),
            ),
        )
        .await;
        assert_eq!(
            (status, body["reason"].as_str()),
            (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
        );
        assert!(headers.get("retry-after").is_some());
    }

    #[test]
    fn roles_are_checked() {
        let user = AuthUser {
            user_id: 1,
            role: Role::User,
        };
        assert!(user.require_role(&[Role::Admin]).is_err());
        assert!(user.require_role(&[Role::User, Role::Admin]).is_ok());
    }

    // ---- saved accounts (a fake database stands in for PostgreSQL) ----

    #[derive(Default)]
    struct FakeRepo {
        users: Mutex<Vec<StoredUser>>,
        down: std::sync::atomic::AtomicBool,
    }

    impl UserRepo for FakeRepo {
        fn insert(&self, user: StoredUser) -> RepoFuture<()> {
            let down = self.down.load(std::sync::atomic::Ordering::SeqCst);
            let mut users = self.users.lock().unwrap();
            let result = if down {
                Err(RepoError::Failed("the database is down".into()))
            } else if users
                .iter()
                .any(|u| u.username == user.username || u.user_id == user.user_id)
            {
                Err(RepoError::UsernameTaken)
            } else {
                users.push(user);
                Ok(())
            };
            Box::pin(async move { result })
        }

        fn upsert(&self, user: StoredUser) -> RepoFuture<()> {
            let mut users = self.users.lock().unwrap();
            users.retain(|u| u.username != user.username);
            users.push(user);
            Box::pin(async { Ok(()) })
        }
    }

    fn fresh_state() -> AuthState {
        AuthState::new(
            b"test-secret-test-secret-test-secret-123",
            DEFAULT_TOKEN_TTL,
            true,
        )
    }

    async fn signup(app: &Router, name: &str) -> (StatusCode, serde_json::Value) {
        let body = serde_json::json!({"username": name, "password": "password-123"});
        let (status, json, _) = call(app, post_json("/api/auth/signup", body)).await;
        (status, json)
    }

    #[tokio::test]
    async fn accounts_and_ids_survive_a_restart() {
        let repo = Arc::new(FakeRepo::default());
        let first = fresh_state();
        first.attach_repo(repo.clone(), vec![]).await.unwrap();
        let app = router(first);
        let (status, alice) = signup(&app, "alice").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(alice["user"]["user_id"], 200);

        // "Restart": a brand new state that loads what the database holds.
        let saved = repo.users.lock().unwrap().clone();
        let second = fresh_state();
        second.attach_repo(repo.clone(), saved).await.unwrap();
        let app = router(second);

        // Alice can still log in, and she is still user 200.
        let login = serde_json::json!({"username": "alice", "password": "password-123"});
        let (status, json, _) = call(&app, post_json("/api/auth/login", login)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["user"]["user_id"], 200);

        // A different person does not get her id, and cannot take her name.
        let (status, bob) = signup(&app, "bob").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(bob["user"]["user_id"], 201);
        assert_eq!(signup(&app, "alice").await.0, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn a_failed_save_creates_nothing() {
        let repo = Arc::new(FakeRepo::default());
        let state = fresh_state();
        state.attach_repo(repo.clone(), vec![]).await.unwrap();
        let app = router(state);

        repo.down.store(true, std::sync::atomic::Ordering::SeqCst);
        let (status, json) = signup(&app, "carol").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["reason"], "internal_error");
        assert!(repo.users.lock().unwrap().is_empty());

        // The name was not used up: signing up again works once the database is back.
        repo.down.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(signup(&app, "carol").await.0, StatusCode::CREATED);
        assert_eq!(repo.users.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn loading_keeps_seeded_accounts_and_continues_the_ids() {
        let state = state();
        let saved = vec![
            // An old saved admin with another password: the seeded one must win.
            StoredUser {
                user_id: 100,
                username: "admin".into(),
                role: Role::Admin,
                password_hash: "not-the-seeded-hash".into(),
            },
            StoredUser {
                user_id: 350,
                username: "zed".into(),
                role: Role::User,
                password_hash: hash_password("password-123").unwrap(),
            },
        ];
        state
            .attach_repo(Arc::new(FakeRepo::default()), saved)
            .await
            .unwrap();
        let app = router(state);

        let login = serde_json::json!({"username": "admin", "password": "admin-pass-1"});
        let (status, _, _) = call(&app, post_json("/api/auth/login", login)).await;
        assert_eq!(status, StatusCode::OK);
        let (_, next) = signup(&app, "yan").await;
        assert_eq!(next["user"]["user_id"], 351);
    }

    #[tokio::test]
    async fn only_environment_seeded_accounts_are_saved() {
        let repo = Arc::new(FakeRepo::default());
        let state = fresh_state();
        // A demo account: in memory only.
        state.seed_user(1, "trader1", "trader-pass-1", Role::User);
        // An account whose password came from the environment: saved.
        let real = state.seed_user(100, "admin", "a-real-password-1", Role::Admin);
        state.0.durable_seeds.lock().unwrap().push(real.clone());

        state.attach_repo(repo.clone(), vec![]).await.unwrap();
        assert_eq!(*repo.users.lock().unwrap(), vec![real]);
    }
}
