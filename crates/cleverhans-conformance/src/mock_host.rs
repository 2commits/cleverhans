//! A known-good §14 host: serves the four webhook endpoints from a
//! fixture's seam scripts (plus optional per-endpoint overrides from a
//! webhook-service vector's `host` block), records every delivery, and
//! enforces the §14.2 header discipline. Used to rerun the agent-layer
//! vectors through the webhook seams, to run `webhook/service/` vectors,
//! and as the `cleverhans mock-host` counterpart host implementers test
//! against.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::fixture::{
    AuthzBehavior, AuthzScript, DryRunBehavior, DryRunScript, ExecutionExpectation, ExecutionLog,
    Fixture, HandlerScript,
};

/// Default endpoint paths served by the mock host (§14.1).
pub const VERIFY_SESSION_PATH: &str = "/cleverhans/verify_session";
/// See [`VERIFY_SESSION_PATH`].
pub const AUTHORIZE_PATH: &str = "/cleverhans/authorize";
/// See [`VERIFY_SESSION_PATH`].
pub const DRY_RUN_PATH: &str = "/cleverhans/dry_run";
/// See [`VERIFY_SESSION_PATH`].
pub const EXECUTE_PATH: &str = "/cleverhans/execute";
/// See [`VERIFY_SESSION_PATH`]. Served only when the fixture action has a
/// declarative slot script (§14.9 is optional).
pub const BUILD_SLOTS_PATH: &str = "/cleverhans/build_slots";

/// The principal the mock host returns when `verify_session` is unscripted.
#[must_use]
pub fn default_principal() -> Value {
    json!({ "user_id": "vector-user" })
}

/// One scripted response override from a vector's `host` block.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBehavior {
    /// Respond with this status and body.
    #[serde(default)]
    pub respond: Option<HostResponse>,
    /// Never answer within any sane client timeout.
    #[serde(default)]
    pub timeout: bool,
}

/// The `respond` form of a [`HostBehavior`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostResponse {
    /// HTTP status.
    pub status: u16,
    /// JSON body.
    pub body: Value,
}

/// Per-endpoint override scripts, keyed by §14 endpoint name.
pub type HostScript = BTreeMap<String, Vec<HostBehavior>>;

/// One recorded webhook delivery.
#[derive(Debug, Clone)]
pub struct Delivery {
    /// §14 endpoint name (`verify_session`, `authorize`, `dry_run`,
    /// `execute`).
    pub endpoint: String,
    /// Request headers, keys lowercased.
    pub headers: BTreeMap<String, String>,
    /// Parsed JSON body.
    pub body: Value,
}

impl Delivery {
    /// The delivery as a matchable JSON value
    /// (`{"endpoint", "headers", "body"}`).
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "endpoint": self.endpoint,
            "headers": self.headers,
            "body": self.body,
        })
    }
}

/// Ordered log of every delivery the mock host received.
pub type DeliveryLog = Arc<Mutex<Vec<Delivery>>>;

struct HostState {
    secret: String,
    /// When set, every delivery must carry a valid §14.2 signature.
    signing_key: Option<String>,
    fixture: Fixture,
    authz: AuthzScript,
    overrides: HostScript,
    deliveries: DeliveryLog,
    executions: ExecutionLog,
    /// Executed idempotency keys → first outcome (idempotent replay, §12.14).
    executed: Mutex<BTreeMap<String, Value>>,
    /// Per-endpoint override cursors.
    cursors: Mutex<BTreeMap<String, usize>>,
    /// Global authorize call count (mirrors `ScriptedAuthz`).
    authz_calls: AtomicUsize,
    /// Per-action dry-run call counts (mirrors `ScriptedDryRun`).
    dry_run_calls: Mutex<BTreeMap<String, usize>>,
}

/// A running mock host.
pub struct MockHost {
    /// Bound address.
    pub addr: SocketAddr,
    /// Every delivery received, in order.
    pub deliveries: DeliveryLog,
    /// Handler invocations derived from execute deliveries, for the
    /// `executions` assertion.
    pub executions: ExecutionLog,
}

impl MockHost {
    /// Spawns a mock host on an ephemeral loopback port.
    ///
    /// # Panics
    ///
    /// On bind failure — test environment error.
    pub async fn spawn(
        fixture: Fixture,
        authz: AuthzScript,
        overrides: HostScript,
        secret: &str,
    ) -> Self {
        Self::spawn_at(fixture, authz, overrides, secret, None, "127.0.0.1:0").await
    }

    /// Spawns a mock host on a specific address (the `cleverhans mock-host`
    /// subcommand), optionally requiring §14.2 payload signatures.
    ///
    /// # Panics
    ///
    /// On bind failure.
    pub async fn spawn_at(
        fixture: Fixture,
        authz: AuthzScript,
        overrides: HostScript,
        secret: &str,
        signing_key: Option<&str>,
        bind: &str,
    ) -> Self {
        let deliveries: DeliveryLog = Arc::new(Mutex::new(Vec::new()));
        let executions: ExecutionLog = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(HostState {
            secret: secret.to_owned(),
            signing_key: signing_key.map(str::to_owned),
            fixture,
            authz,
            overrides,
            deliveries: Arc::clone(&deliveries),
            executions: Arc::clone(&executions),
            executed: Mutex::new(BTreeMap::new()),
            cursors: Mutex::new(BTreeMap::new()),
            authz_calls: AtomicUsize::new(0),
            dry_run_calls: Mutex::new(BTreeMap::new()),
        });
        let app = Router::new()
            .route(VERIFY_SESSION_PATH, post(verify_session))
            .route(AUTHORIZE_PATH, post(authorize))
            .route(DRY_RUN_PATH, post(dry_run))
            .route(EXECUTE_PATH, post(execute))
            .route(BUILD_SLOTS_PATH, post(build_slots))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .expect("bind mock host");
        let addr = listener.local_addr().expect("mock host addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve mock host");
        });
        Self {
            addr,
            deliveries,
            executions,
        }
    }

    /// The host origin, e.g. `http://127.0.0.1:PORT`.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// §14.2 header discipline: bearer secret, known contract version, and —
/// when this host requires it — a valid payload signature over the exact
/// received bytes.
fn check_headers(
    state: &HostState,
    headers: &HeaderMap,
    raw_body: &[u8],
) -> Result<(), Box<Response>> {
    let authorized = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(&format!("Bearer {}", state.secret));
    if !authorized {
        return Err(Box::new(
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error": "bad secret"})),
            )
                .into_response(),
        ));
    }
    let version = headers
        .get("x-cleverhans-webhook-version")
        .and_then(|value| value.to_str().ok());
    if version != Some("1") {
        return Err(Box::new(
            (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": "unsupported_webhook_version", "supported": [1]})),
            )
                .into_response(),
        ));
    }
    if let Some(key) = &state.signing_key {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default();
        let verified = headers
            .get(cleverhans_webhook::sign::SIGNATURE_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(|header| {
                cleverhans_webhook::sign::verify_signature(
                    &[key.as_bytes()],
                    header,
                    raw_body,
                    now,
                    cleverhans_webhook::sign::DEFAULT_SKEW,
                )
            });
        if !matches!(verified, Some(Ok(()))) {
            return Err(Box::new(
                (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(json!({"error": "bad signature"})),
                )
                    .into_response(),
            ));
        }
    }
    Ok(())
}

/// Records the delivery and applies any vector override for the endpoint.
async fn record_and_override(
    state: &HostState,
    endpoint: &str,
    headers: &HeaderMap,
    body: &Value,
) -> Option<Response> {
    state
        .deliveries
        .lock()
        .expect("delivery log")
        .push(Delivery {
            endpoint: endpoint.to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        value.to_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect(),
            body: body.clone(),
        });
    let behavior = {
        let script = state.overrides.get(endpoint)?;
        let mut cursors = state.cursors.lock().expect("cursors");
        let cursor = cursors.entry(endpoint.to_owned()).or_insert(0);
        let behavior = script.get(*cursor).or_else(|| script.last())?.clone();
        *cursor += 1;
        behavior
    };
    if behavior.timeout {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
    behavior.respond.map(|respond| {
        (
            StatusCode::from_u16(respond.status).expect("scripted status"),
            axum::Json(respond.body),
        )
            .into_response()
    })
}

fn parse(body: &Bytes) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

async fn verify_session(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let parsed = parse(&body);
    if let Err(refusal) = check_headers(&state, &headers, &body) {
        return *refusal;
    }
    let body = parsed;
    if let Some(response) = record_and_override(&state, "verify_session", &headers, &body).await {
        return response;
    }
    axum::Json(json!({ "principal": default_principal() })).into_response()
}

async fn authorize(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let parsed = parse(&body);
    if let Err(refusal) = check_headers(&state, &headers, &body) {
        return *refusal;
    }
    let body = parsed;
    if let Some(response) = record_and_override(&state, "authorize", &headers, &body).await {
        return response;
    }
    let call = state.authz_calls.fetch_add(1, Ordering::SeqCst);
    let behavior = match &state.authz {
        AuthzScript::Default { default } => default.clone(),
        AuthzScript::Sequence { sequence, then } => sequence.get(call).unwrap_or(then).clone(),
    };
    let response = match behavior {
        AuthzBehavior::Allow => json!({"decision": "allow"}),
        AuthzBehavior::Deny(reason) => json!({"decision": "deny", "reason": reason}),
    };
    axum::Json(response).into_response()
}

async fn dry_run(State(state): State<Arc<HostState>>, headers: HeaderMap, body: Bytes) -> Response {
    let parsed = parse(&body);
    if let Err(refusal) = check_headers(&state, &headers, &body) {
        return *refusal;
    }
    let body = parsed;
    if let Some(response) = record_and_override(&state, "dry_run", &headers, &body).await {
        return response;
    }
    let action_id = body["action_id"].as_str().unwrap_or_default().to_owned();
    let Some(script) = state
        .fixture
        .scripts
        .get(&action_id)
        .and_then(|script| script.dry_run.clone())
    else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": format!("no dry_run script for `{action_id}`")})),
        )
            .into_response();
    };
    let call = {
        let mut calls = state.dry_run_calls.lock().expect("dry-run calls");
        let counter = calls.entry(action_id).or_insert(0);
        let call = *counter;
        *counter += 1;
        call
    };
    let behavior = match &script {
        DryRunScript::One(behavior) => behavior.clone(),
        DryRunScript::Sequence { sequence, then } => sequence.get(call).unwrap_or(then).clone(),
    };
    let response = match behavior {
        DryRunBehavior::Preview(preview) => {
            json!({"outcome": "preview", "preview": preview})
        }
        DryRunBehavior::Fail(reason) => json!({"outcome": "rejected", "reason": reason}),
    };
    axum::Json(response).into_response()
}

/// §14.9: evaluates the fixture's declarative slot script server-side over
/// the request's params + preview — existing fixtures drive the endpoint
/// with zero new fixture syntax.
async fn build_slots(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    use cleverhans_core::declarative::DeclarativeSlots;
    use cleverhans_core::envelope::DryRunPreview;
    use cleverhans_core::seams::SlotBuilder;

    let parsed = parse(&body);
    if let Err(refusal) = check_headers(&state, &headers, &body) {
        return *refusal;
    }
    let body = parsed;
    if let Some(response) = record_and_override(&state, "build_slots", &headers, &body).await {
        return response;
    }
    let action_id = body["action_id"].as_str().unwrap_or_default().to_owned();
    let Some(slots) = state
        .fixture
        .scripts
        .get(&action_id)
        .and_then(|script| script.slots.clone())
    else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": format!("no slot script for `{action_id}`")})),
        )
            .into_response();
    };
    let params = body["params"].as_object().cloned().unwrap_or_default();
    let preview: Option<DryRunPreview> = body
        .get("preview")
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let built = DeclarativeSlots(slots).build(&params, preview.as_ref());
    axum::Json(json!({ "slots": built })).into_response()
}

async fn execute(State(state): State<Arc<HostState>>, headers: HeaderMap, body: Bytes) -> Response {
    let parsed = parse(&body);
    if let Err(refusal) = check_headers(&state, &headers, &body) {
        return *refusal;
    }
    let body = parsed;
    if let Some(response) = record_and_override(&state, "execute", &headers, &body).await {
        return response;
    }
    // §12.14: idempotent replay returns the first outcome without a second
    // execution.
    let key = body["idempotency_key"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if let Some(previous) = state.executed.lock().expect("executed").get(&key) {
        return axum::Json(previous.clone()).into_response();
    }
    let action_id = body["action_id"].as_str().unwrap_or_default().to_owned();
    let Some(script) = state.fixture.scripts.get(&action_id) else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": format!("no script for `{action_id}`")})),
        )
            .into_response();
    };
    let response = match &script.handler {
        HandlerScript::Return(result) => {
            state
                .executions
                .lock()
                .expect("execution log")
                .push(ExecutionExpectation {
                    action_id,
                    params: body["params"].as_object().cloned().unwrap_or_default(),
                });
            json!({"outcome": "executed", "result": result})
        }
        HandlerScript::Fail(reason) => json!({"outcome": "rejected", "reason": reason}),
    };
    if !key.is_empty() {
        state
            .executed
            .lock()
            .expect("executed")
            .insert(key, response.clone());
    }
    axum::Json(response).into_response()
}
