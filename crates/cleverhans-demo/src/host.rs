//! `cleverhans-demo host` — the demo document store behind the spec §14
//! host webhook contract, so `cleverhans serve` (and the playground) can be
//! tested against a *real, stateful* backend: renames mutate the store,
//! bulk deletes shrink it, dry-runs preview live state. The webhook twin of
//! the in-process `serve` subcommand, sharing the same [`Store`] and
//! handler code via the registry's public seam registrations.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

use cleverhans::prelude::*;

use crate::registry::{DemoUser, Store, build_registry};

struct HostState {
    registry: Registry<DemoUser>,
    secret: String,
    /// Executed idempotency keys → first outcome (spec §12.14).
    executed: Mutex<BTreeMap<String, Value>>,
}

/// Runs the webhook host until ctrl-c.
pub async fn host(bind: &str, secret: String) -> anyhow::Result<()> {
    let store = Store::seeded();
    let state = Arc::new(HostState {
        registry: build_registry(&store),
        secret,
        executed: Mutex::new(BTreeMap::new()),
    });
    let app = Router::new()
        .route("/cleverhans/verify_session", post(verify_session))
        .route("/cleverhans/authorize", post(authorize))
        .route("/cleverhans/dry_run", post(dry_run))
        .route("/cleverhans/execute", post(execute))
        .route("/cleverhans/build_slots", post(build_slots))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(
        bind,
        "demo webhook host up — point `cleverhans serve` at this origin \
         (endpoints under /cleverhans/); restart to reseed the store"
    );
    axum::serve(listener, app).await?;
    Ok(())
}

/// §14.2 header discipline.
fn check_headers(state: &HostState, headers: &HeaderMap) -> Result<(), Box<Response>> {
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
    Ok(())
}

fn parts(body: &Value) -> Result<(&str, JsonMap, DemoUser), Box<Response>> {
    let action_id = body["action_id"].as_str().ok_or_else(|| {
        Box::new(
            (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": "missing action_id"})),
            )
                .into_response(),
        )
    })?;
    let params = body["params"].as_object().cloned().unwrap_or_default();
    // The principal is the verbatim JSON this host handed out at
    // verify_session (§12.13); map it back onto the demo's own type.
    let user = DemoUser {
        name: body["principal"]["name"]
            .as_str()
            .unwrap_or("demo")
            .to_owned(),
    };
    Ok((action_id, params, user))
}

async fn verify_session(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    axum::Json(_body): axum::Json<Value>,
) -> Response {
    if let Err(refusal) = check_headers(&state, &headers) {
        return *refusal;
    }
    // Demo-only: every stream is the same demo user. A real host reads the
    // forwarded session cookie / bearer token here (spec §14.3).
    axum::Json(json!({"principal": {"name": "demo"}})).into_response()
}

async fn authorize(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    axum::Json(_body): axum::Json<Value>,
) -> Response {
    if let Err(refusal) = check_headers(&state, &headers) {
        return *refusal;
    }
    // Everyone may do everything — dogfood host, not an auth reference.
    axum::Json(json!({"decision": "allow"})).into_response()
}

/// §14.9: delegates to the registry's real slot builders — the same
/// dynamic card phrasing the in-process demo renders (e.g. rename's
/// "New title: …"), now host-authored over the wire.
async fn build_slots(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if let Err(refusal) = check_headers(&state, &headers) {
        return *refusal;
    }
    let (action_id, params, _user) = match parts(&body) {
        Ok(parts) => parts,
        Err(refusal) => return *refusal,
    };
    let Some(registration) = state.registry.action(action_id) else {
        return unknown_action(action_id);
    };
    let Some(builder) = &registration.slot_builder else {
        return unknown_action(action_id);
    };
    let preview: Option<DryRunPreview> = body
        .get("preview")
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let slots = builder.build(&params, preview.as_ref());
    axum::Json(json!({ "slots": slots })).into_response()
}

async fn dry_run(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if let Err(refusal) = check_headers(&state, &headers) {
        return *refusal;
    }
    let (action_id, params, user) = match parts(&body) {
        Ok(parts) => parts,
        Err(refusal) => return *refusal,
    };
    let Some(registration) = state.registry.action(action_id) else {
        return unknown_action(action_id);
    };
    let Some(dry_run) = &registration.dry_run else {
        return unknown_action(action_id);
    };
    match dry_run.dry_run(&params, &user).await {
        Ok(preview) => {
            axum::Json(json!({"outcome": "preview", "preview": preview})).into_response()
        }
        Err(HandlerError::Rejected(reason)) => {
            axum::Json(json!({"outcome": "rejected", "reason": reason})).into_response()
        }
        Err(HandlerError::Internal(message)) => internal(&message),
    }
}

async fn execute(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if let Err(refusal) = check_headers(&state, &headers) {
        return *refusal;
    }
    let (action_id, params, user) = match parts(&body) {
        Ok(parts) => parts,
        Err(refusal) => return *refusal,
    };
    let key = body["idempotency_key"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if let Some(previous) = state.executed.lock().expect("executed").get(&key) {
        return axum::Json(previous.clone()).into_response();
    }
    let Some(registration) = state.registry.action(action_id) else {
        return unknown_action(action_id);
    };
    let outcome = match registration.handler.execute(&params, &user).await {
        Ok(result) => json!({"outcome": "executed", "result": result}),
        Err(HandlerError::Rejected(reason)) => json!({"outcome": "rejected", "reason": reason}),
        Err(HandlerError::Internal(message)) => return internal(&message),
    };
    if !key.is_empty() {
        state
            .executed
            .lock()
            .expect("executed")
            .insert(key, outcome.clone());
    }
    axum::Json(outcome).into_response()
}

fn unknown_action(action_id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(json!({"error": format!("unknown action `{action_id}`")})),
    )
        .into_response()
}

fn internal(message: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({"error": message})),
    )
        .into_response()
}
