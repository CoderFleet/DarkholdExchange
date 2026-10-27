//! Rate limits at the gateway's front door (docs/AUTH.md section 8).
//!
//! Every request passes through [`limit`] before it reaches a handler. Three buckets apply:
//!
//! * **per IP address, for everything**: a coarse ceiling so one machine cannot flood the server;
//! * **per IP address, for login and sign-up**: a much stricter one, so passwords cannot be
//!   guessed or accounts mass-created from one place;
//! * **per user, for commands that change something** (orders, cancels, auctions, escrow, wallet
//!   top-ups): one user cannot swamp the engine, however many addresses they use.
//!
//! A refused request gets `429` with a `Retry-After` header and the reason `rate_limited`. The
//! numbers come from environment variables (below). `DX_RATE_LIMIT=off` turns all of it off, which
//! the load tests use.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dx_limits::{Limit, Limiter, Verdict};
use dx_types::{ErrorBody, ReasonCode, UserId};

use crate::{Caller, identify};

/// Orders and the like, per user: a burst of 30, then 15 a second. A person clicking fast stays
/// far below this; a bot does not.
const DEFAULT_USER: &str = "30/15";
/// Everything, per IP address: a burst of 120, then 60 a second. One browser tab polling a few
/// pages stays far below this.
const DEFAULT_IP: &str = "120/60";
/// Login and sign-up, per IP address: a burst of 10, then one every 5 seconds.
const DEFAULT_AUTH: &str = "10/0.2";

/// What the limiter works with: the three limiters and how to find out who is asking.
pub(crate) struct RateLimits {
    enabled: bool,
    trust_proxy: bool,
    ip: Limiter<IpAddr>,
    auth: Limiter<IpAddr>,
    user: Limiter<UserId>,
}

impl RateLimits {
    pub(crate) fn new(
        enabled: bool,
        trust_proxy: bool,
        ip: Limit,
        auth: Limit,
        user: Limit,
    ) -> Self {
        RateLimits {
            enabled,
            trust_proxy,
            ip: Limiter::new(ip),
            auth: Limiter::new(auth),
            user: Limiter::new(user),
        }
    }

    /// Reads the settings:
    ///
    /// * `DX_RATE_LIMIT`: `off` (or `0`) disables everything. Default on.
    /// * `DX_RATE_USER`, `DX_RATE_IP`, `DX_RATE_AUTH`: `BURST/PER_SECOND`, for example `30/15`.
    /// * `DX_TRUST_PROXY`: `1` when the gateway sits behind a reverse proxy that adds
    ///   `X-Forwarded-For`. Otherwise that header is ignored, because anybody can fake it.
    ///
    /// A setting that cannot be read stops the start with a clear message, like the others.
    pub(crate) fn from_env() -> Self {
        let read = |name: &str, default: &str| {
            let text = std::env::var(name).unwrap_or_else(|_| default.to_string());
            Limit::parse(&text).unwrap_or_else(|error| panic!("{name}: {error}"))
        };
        let flag = |name: &str| std::env::var(name).is_ok_and(|value| value == "1");
        let off = std::env::var("DX_RATE_LIMIT").is_ok_and(|value| value == "off" || value == "0");
        RateLimits::new(
            !off,
            flag("DX_TRUST_PROXY"),
            read("DX_RATE_IP", DEFAULT_IP),
            read("DX_RATE_AUTH", DEFAULT_AUTH),
            read("DX_RATE_USER", DEFAULT_USER),
        )
    }
}

/// What the layer needs: the limiters, and the login check (to know who a command is from).
#[derive(Clone)]
pub(crate) struct RateState {
    limits: Arc<RateLimits>,
    auth: dx_auth::AuthState,
}

impl RateState {
    pub(crate) fn new(limits: RateLimits, auth: dx_auth::AuthState) -> Self {
        RateState {
            limits: Arc::new(limits),
            auth,
        }
    }
}

/// Which bucket a request belongs to.
#[derive(Debug, PartialEq, Eq)]
enum Class {
    /// Health checks: never limited, so a monitor cannot be locked out.
    Exempt,
    /// Login and sign-up.
    Auth,
    /// A request that changes something for one user.
    Command,
    /// Everything else (reads, the admin and issuer pages, the WebSocket upgrade).
    Other,
}

fn classify(method: &Method, path: &str) -> Class {
    if path == "/health" || path == "/api/health" {
        return Class::Exempt;
    }
    if *method == Method::POST && (path == "/api/auth/login" || path == "/api/auth/signup") {
        return Class::Auth;
    }
    let changes_something = *method == Method::POST || *method == Method::DELETE;
    let is_command = path == "/api/commands"
        || path == "/api/wallet/topup"
        || path == "/api/me/public-key"
        || path == "/api/orders"
        || path.starts_with("/api/orders/")
        || path.starts_with("/api/auctions")
        || path.starts_with("/api/escrow/");
    if changes_something && is_command {
        Class::Command
    } else {
        Class::Other
    }
}

/// The address the request really came from. Behind our own reverse proxy that is the last
/// `X-Forwarded-For` entry (the one the proxy added); anything earlier was written by the client.
fn client_ip(trust_proxy: bool, headers: &HeaderMap, peer: Option<IpAddr>) -> IpAddr {
    if trust_proxy {
        let forwarded = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .next_back()
            .and_then(|last| last.trim().parse::<IpAddr>().ok());
        if let Some(ip) = forwarded {
            return ip;
        }
    }
    peer.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
}

fn denied(retry_after_secs: u64) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(ErrorBody {
            reason: ReasonCode::RateLimited,
        }),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// The layer itself (`axum::middleware::from_fn_with_state`).
pub(crate) async fn limit(
    State(state): State<RateState>,
    request: Request,
    next: Next,
) -> Response {
    let limits = &state.limits;
    if !limits.enabled {
        return next.run(request).await;
    }
    let class = classify(request.method(), request.uri().path());
    if class == Class::Exempt {
        return next.run(request).await;
    }

    let now = Instant::now();
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    let ip = client_ip(limits.trust_proxy, request.headers(), peer);

    if let Verdict::Denied { retry_after_secs } = limits.ip.check(&ip, now) {
        return denied(retry_after_secs);
    }
    let verdict = match class {
        Class::Auth => limits.auth.check(&ip, now),
        Class::Command => match identify(&state.auth, request.headers()) {
            Ok(Caller::Token { user_id, .. } | Caller::Dev(user_id)) => {
                limits.user.check(&user_id, now)
            }
            // No valid identity: the handler will answer 401. The address limit above still applied.
            Err(_) => Verdict::Allowed,
        },
        Class::Exempt | Class::Other => Verdict::Allowed,
    };
    if let Verdict::Denied { retry_after_secs } = verdict {
        return denied(retry_after_secs);
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::{delete, get, post};
    use tower::ServiceExt;

    const SECRET: &[u8] = b"rate-test-secret-rate-test-secret-123";

    fn auth() -> dx_auth::AuthState {
        dx_auth::AuthState::new(SECRET, std::time::Duration::from_secs(60), true)
    }

    fn limit_of(burst: u32, per_second: f64) -> Limit {
        Limit::new(burst, per_second).unwrap()
    }

    /// A small app with the real layer and empty handlers, so only the limiting is tested.
    fn app(limits: RateLimits, auth: dx_auth::AuthState) -> Router {
        Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/api/markets", get(|| async { "ok" }))
            .route("/api/orders", post(|| async { "ok" }))
            .route("/api/orders/{id}", delete(|| async { "ok" }))
            .route("/api/auth/login", post(|| async { "ok" }))
            .route("/api/auth/signup", post(|| async { "ok" }))
            .route("/api/issuer/issue", post(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                RateState::new(limits, auth),
                limit,
            ))
    }

    fn limits(ip: Limit, auth: Limit, user: Limit) -> RateLimits {
        RateLimits::new(true, false, ip, auth, user)
    }

    /// Sends one request from `from`, as the dev user `dev_user` if given.
    async fn send(
        app: &Router,
        method: &str,
        path: &str,
        from: &str,
        dev_user: Option<&str>,
    ) -> Response {
        let mut builder = axum::http::Request::builder().method(method).uri(path);
        if let Some(user) = dev_user {
            builder = builder.header("x-dev-user", user);
        }
        let mut request = builder.body(Body::empty()).unwrap();
        let address: SocketAddr = format!("{from}:5000").parse().unwrap();
        request.extensions_mut().insert(ConnectInfo(address));
        app.clone().oneshot(request).await.unwrap()
    }

    fn retry_after(response: &Response) -> Option<String> {
        response
            .headers()
            .get(header::RETRY_AFTER)
            .map(|value| value.to_str().unwrap().to_string())
    }

    #[tokio::test]
    async fn one_user_cannot_swamp_the_commands_but_others_and_reads_are_fine() {
        let roomy = limit_of(1000, 1000.0);
        let app = app(limits(roomy, roomy, limit_of(3, 0.5)), auth());
        for _ in 0..3 {
            let reply = send(&app, "POST", "/api/orders", "10.0.0.1", Some("7")).await;
            assert_eq!(reply.status(), StatusCode::OK);
        }
        let reply = send(&app, "POST", "/api/orders", "10.0.0.1", Some("7")).await;
        assert_eq!(reply.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(retry_after(&reply).as_deref(), Some("2"));

        // The same user from another address is still the same user.
        let reply = send(&app, "POST", "/api/orders", "10.0.0.2", Some("7")).await;
        assert_eq!(reply.status(), StatusCode::TOO_MANY_REQUESTS);
        // Cancels count as commands too.
        let reply = send(&app, "DELETE", "/api/orders/9", "10.0.0.1", Some("7")).await;
        assert_eq!(reply.status(), StatusCode::TOO_MANY_REQUESTS);

        // Another user is untouched, and so are reads and the issuer endpoint.
        let reply = send(&app, "POST", "/api/orders", "10.0.0.1", Some("8")).await;
        assert_eq!(reply.status(), StatusCode::OK);
        let reply = send(&app, "GET", "/api/markets", "10.0.0.1", Some("7")).await;
        assert_eq!(reply.status(), StatusCode::OK);
        let reply = send(&app, "POST", "/api/issuer/issue", "10.0.0.1", Some("7")).await;
        assert_eq!(reply.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_refusal_names_its_reason() {
        let roomy = limit_of(1000, 1000.0);
        let app = app(limits(roomy, roomy, limit_of(1, 0.1)), auth());
        send(&app, "POST", "/api/orders", "10.0.0.1", Some("7")).await;
        let reply = send(&app, "POST", "/api/orders", "10.0.0.1", Some("7")).await;
        assert_eq!(reply.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = axum::body::to_bytes(reply.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], br#"{"reason":"rate_limited"}"#);
    }

    #[tokio::test]
    async fn login_and_signup_share_a_strict_limit_per_address() {
        let roomy = limit_of(1000, 1000.0);
        let app = app(limits(roomy, limit_of(2, 0.1), roomy), auth());
        let reply = send(&app, "POST", "/api/auth/signup", "10.0.0.1", None).await;
        assert_eq!(reply.status(), StatusCode::OK);
        let reply = send(&app, "POST", "/api/auth/login", "10.0.0.1", None).await;
        assert_eq!(reply.status(), StatusCode::OK);
        let reply = send(&app, "POST", "/api/auth/signup", "10.0.0.1", None).await;
        assert_eq!(reply.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(retry_after(&reply).as_deref(), Some("10"));
        // Another address has its own allowance, and other pages from the same one still work.
        let reply = send(&app, "POST", "/api/auth/signup", "10.0.0.2", None).await;
        assert_eq!(reply.status(), StatusCode::OK);
        let reply = send(&app, "GET", "/api/markets", "10.0.0.1", None).await;
        assert_eq!(reply.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn one_address_has_a_ceiling_on_everything() {
        let roomy = limit_of(1000, 1000.0);
        let app = app(limits(limit_of(5, 0.1), roomy, roomy), auth());
        for _ in 0..5 {
            let reply = send(&app, "GET", "/api/markets", "10.0.0.1", None).await;
            assert_eq!(reply.status(), StatusCode::OK);
        }
        let reply = send(&app, "GET", "/api/markets", "10.0.0.1", None).await;
        assert_eq!(reply.status(), StatusCode::TOO_MANY_REQUESTS);
        let reply = send(&app, "GET", "/api/markets", "10.0.0.9", None).await;
        assert_eq!(reply.status(), StatusCode::OK);
        // Health checks are never limited.
        for _ in 0..20 {
            let reply = send(&app, "GET", "/health", "10.0.0.1", None).await;
            assert_eq!(reply.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn a_request_without_a_valid_login_is_judged_by_address_only() {
        let roomy = limit_of(1000, 1000.0);
        let app = app(limits(roomy, roomy, limit_of(1, 0.1)), auth());
        // A bad token never reaches the user bucket, so it cannot use up somebody's allowance.
        for _ in 0..3 {
            let mut request = axum::http::Request::builder()
                .method("POST")
                .uri("/api/orders")
                .header("authorization", "Bearer junk")
                .body(Body::empty())
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("10.0.0.1:5000".parse::<SocketAddr>().unwrap()));
            let reply = app.clone().oneshot(request).await.unwrap();
            assert_eq!(reply.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn a_token_is_limited_by_its_user_not_by_the_dev_header() {
        let roomy = limit_of(1000, 1000.0);
        let auth = auth();
        let token = auth
            .issue_token(&dx_auth::PublicUser {
                user_id: 42,
                username: "x".into(),
                role: dx_auth::Role::User,
            })
            .unwrap();
        let app = app(limits(roomy, roomy, limit_of(2, 0.1)), auth);
        for expected in [
            StatusCode::OK,
            StatusCode::OK,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            let mut request = axum::http::Request::builder()
                .method("POST")
                .uri("/api/orders")
                .header("authorization", format!("Bearer {token}"))
                // Changing this header must not buy a fresh allowance.
                .header("x-dev-user", expected.as_u16().to_string())
                .body(Body::empty())
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("10.0.0.1:5000".parse::<SocketAddr>().unwrap()));
            let reply = app.clone().oneshot(request).await.unwrap();
            assert_eq!(reply.status(), expected);
        }
    }

    #[tokio::test]
    async fn the_proxy_header_is_used_only_when_trusted_and_only_its_last_entry() {
        let roomy = limit_of(1000, 1000.0);
        let tight = limit_of(1, 0.1);
        let ask = |app: Router, forwarded: &'static str| async move {
            let mut request = axum::http::Request::builder()
                .method("GET")
                .uri("/api/markets")
                .header("x-forwarded-for", forwarded)
                .body(Body::empty())
                .unwrap();
            request.extensions_mut().insert(ConnectInfo(
                "172.16.0.1:5000".parse::<SocketAddr>().unwrap(),
            ));
            app.oneshot(request).await.unwrap().status()
        };

        // Trusted: two different clients behind the proxy have separate allowances, and a fake
        // first entry changes nothing, because only the proxy's own (last) entry counts.
        let trusted = app(RateLimits::new(true, true, tight, roomy, roomy), auth());
        assert_eq!(
            ask(trusted.clone(), "1.1.1.1, 10.0.0.5").await,
            StatusCode::OK
        );
        assert_eq!(
            ask(trusted.clone(), "2.2.2.2, 10.0.0.5").await,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(ask(trusted, "9.9.9.9, 10.0.0.6").await, StatusCode::OK);

        // Not trusted: the header is ignored, so everything counts as the proxy's own address.
        let untrusted = app(RateLimits::new(true, false, tight, roomy, roomy), auth());
        assert_eq!(ask(untrusted.clone(), "1.1.1.1").await, StatusCode::OK);
        assert_eq!(
            ask(untrusted, "2.2.2.2").await,
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn switched_off_nothing_is_limited() {
        let tight = limit_of(1, 0.1);
        let off = RateLimits::new(false, false, tight, tight, tight);
        assert!(!off.enabled);
        let app = app(off, auth());
        for _ in 0..10 {
            let reply = send(&app, "POST", "/api/orders", "10.0.0.1", Some("7")).await;
            assert_eq!(reply.status(), StatusCode::OK);
        }
    }

    #[test]
    fn requests_are_sorted_into_the_right_bucket() {
        let get = Method::GET;
        let post = Method::POST;
        let del = Method::DELETE;
        assert_eq!(classify(&get, "/health"), Class::Exempt);
        assert_eq!(classify(&get, "/api/health"), Class::Exempt);
        assert_eq!(classify(&post, "/api/auth/login"), Class::Auth);
        assert_eq!(classify(&post, "/api/auth/signup"), Class::Auth);
        assert_eq!(classify(&get, "/api/auth/login"), Class::Other);
        assert_eq!(classify(&post, "/api/orders"), Class::Command);
        assert_eq!(classify(&del, "/api/orders/5"), Class::Command);
        assert_eq!(classify(&post, "/api/commands"), Class::Command);
        assert_eq!(classify(&post, "/api/auctions/3/accept"), Class::Command);
        assert_eq!(classify(&post, "/api/escrow/9/receipt"), Class::Command);
        assert_eq!(classify(&post, "/api/wallet/topup"), Class::Command);
        // Reads, and the trusted issuer, admin and dev endpoints, only meet the address limit.
        assert_eq!(classify(&get, "/api/orders/open"), Class::Other);
        assert_eq!(classify(&get, "/api/auctions"), Class::Other);
        assert_eq!(classify(&post, "/api/issuer/issue"), Class::Other);
        assert_eq!(classify(&post, "/api/admin/events"), Class::Other);
        assert_eq!(classify(&post, "/api/dev/deposit"), Class::Other);
        assert_eq!(classify(&get, "/ws"), Class::Other);
    }
}
