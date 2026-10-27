//! Admin setup: events and their price tiers.
//!
//! An admin creates an event, adds tiers (one tier = one tradable market) and publishes them.
//! The price range ("collar") of a tier is never typed in by the admin: the server works it out
//! from the face value with the same `MarketConfig` maths the matching engine uses, so the
//! admin page and the engine can never disagree.
//!
//! Publishing locks a tier and hands its `MarketConfig` to a [`Publisher`], which is where the
//! engine's "create market" command plugs in. Events and tiers live in memory for now.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRef, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use dx_auth::{AuthError, AuthState, AuthUser, Role};
use dx_types::{MarketConfig, MarketId, MarketStatus, Minor};
use serde::{Deserialize, Serialize};

/// Collar policy: lowest allowed price is 50% of face value, highest is 120% (basis points).
/// The same numbers as the demo markets. Admins cannot change them per tier.
pub const COLLAR_FLOOR_BPS: u16 = 5_000;
pub const COLLAR_CAP_BPS: u16 = 12_000;

const DEFAULT_TICK_MINOR: Minor = 100;
const DEFAULT_HOLDING_LIMIT: u32 = 4;
const DEFAULT_TAKER_FEE_BPS: u16 = 100;
const MAX_FACE_VALUE_MINOR: Minor = 1_000_000_000_000;
const MAX_HOLDING_LIMIT: u32 = 100;
const MAX_TAKER_FEE_BPS: u16 = 1_000;
const MAX_NAME_LEN: usize = 80;
/// Seeded demo markets use event ids 1-2 and market ids 1-3, so new ones start higher.
const FIRST_EVENT_ID: u32 = 100;
const FIRST_MARKET_ID: MarketId = 100;

pub type PublishFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
/// Called once when a tier is published. Returns `Err` if the engine could not create the market.
pub type Publisher = Arc<dyn Fn(MarketConfig) -> PublishFuture + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TierStatus {
    Draft,
    Publishing,
    Published,
}

struct TierRec {
    tier_name: String,
    config: MarketConfig,
    status: TierStatus,
}

struct EventRec {
    event_id: u32,
    name: String,
    venue: Option<String>,
    tiers: Vec<TierRec>,
}

struct Data {
    events: Vec<EventRec>,
    next_event_id: u32,
    next_market_id: MarketId,
}

/// Shared admin state. Cheap to clone.
#[derive(Clone)]
pub struct AdminState {
    data: Arc<Mutex<Data>>,
    publisher: Publisher,
}

impl AdminState {
    pub fn new(publisher: Publisher) -> Self {
        Self {
            data: Arc::new(Mutex::new(Data {
                events: Vec::new(),
                next_event_id: FIRST_EVENT_ID,
                next_market_id: FIRST_MARKET_ID,
            })),
            publisher,
        }
    }
}

#[derive(Clone)]
struct AppState {
    admin: AdminState,
    auth: AuthState,
}

impl FromRef<AppState> for AuthState {
    fn from_ref(state: &AppState) -> AuthState {
        state.auth.clone()
    }
}

/// Either a login problem (401/403) or a problem with the request itself.
#[derive(Debug)]
pub enum AdminError {
    Auth(AuthError),
    Api {
        status: StatusCode,
        reason: &'static str,
    },
}

impl From<AuthError> for AdminError {
    fn from(error: AuthError) -> Self {
        Self::Auth(error)
    }
}

impl AdminError {
    fn api(status: StatusCode, reason: &'static str) -> Self {
        Self::Api { status, reason }
    }
    fn invalid() -> Self {
        Self::api(StatusCode::BAD_REQUEST, "invalid_request")
    }
    fn not_found() -> Self {
        Self::api(StatusCode::NOT_FOUND, "not_found")
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        match self {
            Self::Auth(error) => error.into_response(),
            Self::Api { status, reason } => {
                (status, Json(serde_json::json!({ "reason": reason }))).into_response()
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct NewEvent {
    name: String,
    venue: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NewTier {
    name: String,
    face_value_minor: Minor,
    tick_size_minor: Option<Minor>,
    holding_limit: Option<u32>,
    taker_fee_bps: Option<u16>,
    escrow_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CollarPreviewRequest {
    face_value_minor: Minor,
    tick_size_minor: Option<Minor>,
}

#[derive(Debug, Serialize)]
struct CollarView {
    collar_floor_bps: u16,
    collar_cap_bps: u16,
    floor_ticks: u64,
    cap_ticks: u64,
    floor_minor: Minor,
    cap_minor: Minor,
}

#[derive(Debug, Serialize)]
struct TierView {
    market_id: MarketId,
    event_id: u32,
    name: String,
    market_name: String,
    face_value_minor: Minor,
    tick_size_minor: Minor,
    holding_limit: u32,
    taker_fee_bps: u16,
    escrow_enabled: bool,
    #[serde(flatten)]
    collar: CollarView,
    status: TierStatus,
}

#[derive(Debug, Serialize)]
struct EventView {
    event_id: u32,
    name: String,
    venue: Option<String>,
    tiers: Vec<TierView>,
}

fn collar_view(config: &MarketConfig) -> CollarView {
    let floor_ticks = config.floor_ticks();
    let cap_ticks = config.cap_ticks();
    let to_minor = |ticks: u64| {
        Minor::try_from(ticks as u128 * config.tick_size_minor as u128).unwrap_or(Minor::MAX)
    };
    CollarView {
        collar_floor_bps: config.collar_floor_bps,
        collar_cap_bps: config.collar_cap_bps,
        floor_ticks,
        cap_ticks,
        floor_minor: to_minor(floor_ticks),
        cap_minor: to_minor(cap_ticks),
    }
}

fn tier_view(tier: &TierRec) -> TierView {
    TierView {
        market_id: tier.config.id,
        event_id: tier.config.event_id,
        name: tier.tier_name.clone(),
        market_name: tier.config.name.clone(),
        face_value_minor: tier.config.face_value_minor,
        tick_size_minor: tier.config.tick_size_minor,
        holding_limit: tier.config.holding_limit,
        taker_fee_bps: tier.config.taker_fee_bps,
        escrow_enabled: tier.config.escrow_enabled,
        collar: collar_view(&tier.config),
        status: tier.status,
    }
}

fn event_view(event: &EventRec, only_published: bool) -> EventView {
    EventView {
        event_id: event.event_id,
        name: event.name.clone(),
        venue: event.venue.clone(),
        tiers: event
            .tiers
            .iter()
            .filter(|tier| !only_published || tier.status == TierStatus::Published)
            .map(tier_view)
            .collect(),
    }
}

/// Trims a name and checks its length. Control characters are refused.
fn clean_name(raw: &str) -> Result<String, AdminError> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN || name.chars().any(char::is_control)
    {
        return Err(AdminError::invalid());
    }
    Ok(name.to_string())
}

#[derive(Debug, Default)]
struct MarketOptions {
    tick_size_minor: Option<Minor>,
    holding_limit: Option<u32>,
    taker_fee_bps: Option<u16>,
    escrow_enabled: Option<bool>,
}

/// Builds a market from the admin's numbers and the collar policy, and validates it with the
/// shared `MarketConfig::validate` rules.
fn build_config(
    market_id: MarketId,
    event_id: u32,
    name: String,
    face_value_minor: Minor,
    options: MarketOptions,
) -> Result<MarketConfig, AdminError> {
    let holding_limit = options.holding_limit.unwrap_or(DEFAULT_HOLDING_LIMIT);
    let taker_fee_bps = options.taker_fee_bps.unwrap_or(DEFAULT_TAKER_FEE_BPS);
    if face_value_minor > MAX_FACE_VALUE_MINOR
        || holding_limit > MAX_HOLDING_LIMIT
        || taker_fee_bps > MAX_TAKER_FEE_BPS
    {
        return Err(AdminError::invalid());
    }
    let config = MarketConfig {
        id: market_id,
        event_id,
        name,
        face_value_minor,
        tick_size_minor: options.tick_size_minor.unwrap_or(DEFAULT_TICK_MINOR),
        collar_floor_bps: COLLAR_FLOOR_BPS,
        collar_cap_bps: COLLAR_CAP_BPS,
        holding_limit,
        taker_fee_bps,
        escrow_enabled: options.escrow_enabled.unwrap_or(false),
        // A draft cannot trade. Publishing switches it to `Open`.
        status: MarketStatus::Closed,
    };
    config.validate().map_err(|_| AdminError::invalid())?;
    Ok(config)
}

fn admin_only(caller: &AuthUser) -> Result<(), AdminError> {
    Ok(caller.require_role(&[Role::Admin])?)
}

fn read_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, AdminError> {
    body.map(|Json(value)| value)
        .map_err(|_| AdminError::invalid())
}

/// Public: `GET /api/events`. Admin: `POST /api/admin/events`, `GET /api/admin/events`,
/// `POST /api/admin/events/{id}/tiers`, `POST /api/admin/tiers/{market_id}/publish`,
/// `POST /api/admin/collar-preview`.
pub fn router(admin: AdminState, auth: AuthState) -> Router {
    Router::new()
        .route("/api/events", get(public_events))
        .route("/api/admin/events", get(admin_events).post(create_event))
        .route("/api/admin/events/{event_id}/tiers", post(create_tier))
        .route("/api/admin/tiers/{market_id}/publish", post(publish_tier))
        .route("/api/admin/collar-preview", post(collar_preview))
        .with_state(AppState { admin, auth })
}

async fn public_events(State(app): State<AppState>) -> Json<Vec<EventView>> {
    let data = app.admin.data.lock().expect("admin data");
    Json(
        data.events
            .iter()
            .map(|event| event_view(event, true))
            .filter(|event| !event.tiers.is_empty())
            .collect(),
    )
}

async fn admin_events(
    State(app): State<AppState>,
    caller: AuthUser,
) -> Result<Json<Vec<EventView>>, AdminError> {
    admin_only(&caller)?;
    let data = app.admin.data.lock().expect("admin data");
    Ok(Json(
        data.events
            .iter()
            .map(|event| event_view(event, false))
            .collect(),
    ))
}

async fn create_event(
    State(app): State<AppState>,
    caller: AuthUser,
    body: Result<Json<NewEvent>, JsonRejection>,
) -> Result<(StatusCode, Json<EventView>), AdminError> {
    admin_only(&caller)?;
    let request = read_body(body)?;
    let name = clean_name(&request.name)?;
    let venue = match request.venue {
        Some(venue) if !venue.trim().is_empty() => Some(clean_name(&venue)?),
        _ => None,
    };
    let mut data = app.admin.data.lock().expect("admin data");
    let event = EventRec {
        event_id: data.next_event_id,
        name,
        venue,
        tiers: Vec::new(),
    };
    data.next_event_id += 1;
    let view = event_view(&event, false);
    data.events.push(event);
    Ok((StatusCode::CREATED, Json(view)))
}

async fn create_tier(
    State(app): State<AppState>,
    caller: AuthUser,
    Path(event_id): Path<u32>,
    body: Result<Json<NewTier>, JsonRejection>,
) -> Result<(StatusCode, Json<TierView>), AdminError> {
    admin_only(&caller)?;
    let request = read_body(body)?;
    let tier_name = clean_name(&request.name)?;

    let mut data = app.admin.data.lock().expect("admin data");
    let market_id = data.next_market_id;
    let event = data
        .events
        .iter_mut()
        .find(|event| event.event_id == event_id)
        .ok_or_else(AdminError::not_found)?;
    if event
        .tiers
        .iter()
        .any(|tier| tier.tier_name.eq_ignore_ascii_case(&tier_name))
    {
        return Err(AdminError::api(StatusCode::CONFLICT, "tier_exists"));
    }
    let config = build_config(
        market_id,
        event_id,
        format!("{} {}", event.name, tier_name),
        request.face_value_minor,
        MarketOptions {
            tick_size_minor: request.tick_size_minor,
            holding_limit: request.holding_limit,
            taker_fee_bps: request.taker_fee_bps,
            escrow_enabled: request.escrow_enabled,
        },
    )?;
    let tier = TierRec {
        tier_name,
        config,
        status: TierStatus::Draft,
    };
    let view = tier_view(&tier);
    event.tiers.push(tier);
    data.next_market_id += 1;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn collar_preview(
    caller: AuthUser,
    body: Result<Json<CollarPreviewRequest>, JsonRejection>,
) -> Result<Json<CollarView>, AdminError> {
    admin_only(&caller)?;
    let request = read_body(body)?;
    let config = build_config(
        0,
        0,
        "preview".into(),
        request.face_value_minor,
        MarketOptions {
            tick_size_minor: request.tick_size_minor,
            ..MarketOptions::default()
        },
    )?;
    Ok(Json(collar_view(&config)))
}

async fn publish_tier(
    State(app): State<AppState>,
    caller: AuthUser,
    Path(market_id): Path<MarketId>,
) -> Result<Json<TierView>, AdminError> {
    admin_only(&caller)?;

    // Step 1, under the lock: claim the tier so two clicks cannot publish it twice.
    let config = {
        let mut data = app.admin.data.lock().expect("admin data");
        let tier = find_tier(&mut data, market_id)?;
        if tier.status != TierStatus::Draft {
            return Err(AdminError::api(StatusCode::CONFLICT, "already_published"));
        }
        tier.status = TierStatus::Publishing;
        MarketConfig {
            status: MarketStatus::Open,
            ..tier.config.clone()
        }
    };

    // Step 2, without the lock: ask the engine to create the market.
    let result = (app.admin.publisher)(config.clone()).await;

    // Step 3, under the lock: lock the tier for good, or put it back as a draft.
    let mut data = app.admin.data.lock().expect("admin data");
    let tier = find_tier(&mut data, market_id)?;
    match result {
        Ok(()) => {
            tier.config = config;
            tier.status = TierStatus::Published;
            Ok(Json(tier_view(tier)))
        }
        Err(_) => {
            tier.status = TierStatus::Draft;
            Err(AdminError::api(
                StatusCode::BAD_GATEWAY,
                "market_setup_failed",
            ))
        }
    }
}

fn find_tier(data: &mut Data, market_id: MarketId) -> Result<&mut TierRec, AdminError> {
    data.events
        .iter_mut()
        .flat_map(|event| event.tiers.iter_mut())
        .find(|tier| tier.config.id == market_id)
        .ok_or_else(AdminError::not_found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    type Seen = Arc<Mutex<Vec<MarketConfig>>>;

    struct Harness {
        app: Router,
        seen: Seen,
        admin_token: String,
        user_token: String,
    }

    async fn call(app: &Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn json_request(
        method: &str,
        path: &str,
        token: Option<&str>,
        body: serde_json::Value,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    fn get_request(path: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::get(path);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn harness(fail_publish: bool) -> Harness {
        let auth = AuthState::new(
            b"test-secret-test-secret-test-secret-123",
            std::time::Duration::from_secs(3600),
            true,
        );
        auth.seed_user(100, "admin", "admin-pass-1", Role::Admin);
        auth.seed_user(1, "trader1", "trader-pass-1", Role::User);
        let seen: Seen = Arc::default();
        let recorder = seen.clone();
        let publisher: Publisher = Arc::new(move |config| {
            let recorder = recorder.clone();
            Box::pin(async move {
                if fail_publish {
                    return Err("engine unavailable".to_string());
                }
                recorder.lock().unwrap().push(config);
                Ok(())
            })
        });
        let app = dx_auth::router(auth.clone()).merge(router(AdminState::new(publisher), auth));
        let login = |name: &str, password: &str| {
            json_request(
                "POST",
                "/api/auth/login",
                None,
                serde_json::json!({"username": name, "password": password}),
            )
        };
        let (_, admin) = call(&app, login("admin", "admin-pass-1")).await;
        let (_, user) = call(&app, login("trader1", "trader-pass-1")).await;
        Harness {
            app,
            seen,
            admin_token: admin["token"].as_str().unwrap().to_string(),
            user_token: user["token"].as_str().unwrap().to_string(),
        }
    }

    async fn new_event(h: &Harness) -> serde_json::Value {
        let (status, body) = call(
            &h.app,
            json_request(
                "POST",
                "/api/admin/events",
                Some(&h.admin_token),
                serde_json::json!({"name": "Day 1", "venue": "Main hall"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        body
    }

    async fn new_tier(
        h: &Harness,
        event_id: u64,
        name: &str,
        face: i64,
    ) -> (StatusCode, serde_json::Value) {
        call(
            &h.app,
            json_request(
                "POST",
                &format!("/api/admin/events/{event_id}/tiers"),
                Some(&h.admin_token),
                serde_json::json!({"name": name, "face_value_minor": face}),
            ),
        )
        .await
    }

    #[tokio::test]
    async fn only_admins_may_use_admin_endpoints() {
        let h = harness(false).await;
        let body = serde_json::json!({"name": "Day 1"});
        let (status, _) = call(
            &h.app,
            json_request("POST", "/api/admin/events", None, body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, reply) = call(
            &h.app,
            json_request("POST", "/api/admin/events", Some(&h.user_token), body),
        )
        .await;
        assert_eq!(
            (status, reply["reason"].as_str()),
            (StatusCode::FORBIDDEN, Some("forbidden"))
        );
        let (status, _) = call(
            &h.app,
            get_request("/api/admin/events", Some(&h.user_token)),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn collar_is_worked_out_by_the_server() {
        let h = harness(false).await;
        let event = new_event(&h).await;
        let (status, tier) =
            new_tier(&h, event["event_id"].as_u64().unwrap(), "General", 500_000).await;
        assert_eq!(status, StatusCode::CREATED);
        // Same numbers the demo market shows: Rs 2,500.00 to Rs 6,000.00.
        assert_eq!(tier["floor_minor"], 250_000);
        assert_eq!(tier["cap_minor"], 600_000);
        assert_eq!(tier["floor_ticks"], 2_500);
        assert_eq!(tier["cap_ticks"], 6_000);
        assert_eq!(tier["status"], "draft");
        assert_eq!(tier["market_name"], "Day 1 General");

        let (status, preview) = call(
            &h.app,
            json_request(
                "POST",
                "/api/admin/collar-preview",
                Some(&h.admin_token),
                serde_json::json!({"face_value_minor": 500_000}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(preview["floor_minor"], 250_000);
    }

    #[tokio::test]
    async fn bad_input_is_refused() {
        let h = harness(false).await;
        let event = new_event(&h).await;
        let id = event["event_id"].as_u64().unwrap();
        for face in [0, -5, 150, 2_000_000_000_000] {
            let (status, body) = new_tier(&h, id, "Bad", face).await;
            assert_eq!(
                (status, body["reason"].as_str()),
                (StatusCode::BAD_REQUEST, Some("invalid_request")),
                "face {face}"
            );
        }
        let (status, _) = new_tier(&h, id, "   ", 500_000).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, body) = new_tier(&h, 9_999, "General", 500_000).await;
        assert_eq!(
            (status, body["reason"].as_str()),
            (StatusCode::NOT_FOUND, Some("not_found"))
        );
        let (status, body) = call(
            &h.app,
            json_request(
                "POST",
                "/api/admin/events",
                Some(&h.admin_token),
                serde_json::json!({"nope": 1}),
            ),
        )
        .await;
        assert_eq!(
            (status, body["reason"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_request"))
        );
    }

    #[tokio::test]
    async fn duplicate_tier_name_is_refused() {
        let h = harness(false).await;
        let event = new_event(&h).await;
        let id = event["event_id"].as_u64().unwrap();
        assert_eq!(
            new_tier(&h, id, "General", 500_000).await.0,
            StatusCode::CREATED
        );
        let (status, body) = new_tier(&h, id, "general", 600_000).await;
        assert_eq!(
            (status, body["reason"].as_str()),
            (StatusCode::CONFLICT, Some("tier_exists"))
        );
    }

    #[tokio::test]
    async fn publish_locks_the_tier_and_sends_it_to_the_engine() {
        let h = harness(false).await;
        let event = new_event(&h).await;
        let (_, tier) = new_tier(&h, event["event_id"].as_u64().unwrap(), "General", 500_000).await;
        let market_id = tier["market_id"].as_u64().unwrap();

        // Not public until published.
        let (_, public) = call(&h.app, get_request("/api/events", None)).await;
        assert_eq!(public.as_array().unwrap().len(), 0);

        let path = format!("/api/admin/tiers/{market_id}/publish");
        let (status, published) = call(
            &h.app,
            json_request("POST", &path, Some(&h.admin_token), serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(published["status"], "published");

        let seen = h.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].id as u64, market_id);
        assert_eq!(seen[0].status, MarketStatus::Open);
        seen[0].validate().unwrap();

        // A second publish is refused, and the engine hears nothing more.
        let (status, body) = call(
            &h.app,
            json_request("POST", &path, Some(&h.admin_token), serde_json::json!({})),
        )
        .await;
        assert_eq!(
            (status, body["reason"].as_str()),
            (StatusCode::CONFLICT, Some("already_published"))
        );
        assert_eq!(h.seen.lock().unwrap().len(), 1);

        // Now it shows in the public list.
        let (_, public) = call(&h.app, get_request("/api/events", None)).await;
        assert_eq!(public[0]["tiers"][0]["status"], "published");
        assert!(public[0]["tiers"][0].get("password").is_none());
    }

    #[tokio::test]
    async fn failed_engine_setup_leaves_a_draft() {
        let h = harness(true).await;
        let event = new_event(&h).await;
        let (_, tier) = new_tier(&h, event["event_id"].as_u64().unwrap(), "General", 500_000).await;
        let path = format!(
            "/api/admin/tiers/{}/publish",
            tier["market_id"].as_u64().unwrap()
        );
        let (status, body) = call(
            &h.app,
            json_request("POST", &path, Some(&h.admin_token), serde_json::json!({})),
        )
        .await;
        assert_eq!(
            (status, body["reason"].as_str()),
            (StatusCode::BAD_GATEWAY, Some("market_setup_failed"))
        );
        let (_, all) = call(
            &h.app,
            get_request("/api/admin/events", Some(&h.admin_token)),
        )
        .await;
        assert_eq!(all[0]["tiers"][0]["status"], "draft");
    }

    #[tokio::test]
    async fn publishing_an_unknown_tier_is_not_found() {
        let h = harness(false).await;
        let (status, _) = call(
            &h.app,
            json_request(
                "POST",
                "/api/admin/tiers/424242/publish",
                Some(&h.admin_token),
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
