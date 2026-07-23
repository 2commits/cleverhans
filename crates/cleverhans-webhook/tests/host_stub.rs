//! Delivery behavior against a live axum stub host: request shapes, header
//! discipline, the §14 outcome mappings, and the execute retry loop.

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::{Value, json};

use cleverhans_core::seams::{ActionHandler, AuthzDecision, AuthzResolver, DryRunHandler};
use cleverhans_webhook::client::{HostClient, HostClientConfig, RetryPolicy, Route, Timeouts};
use cleverhans_webhook::seams::{
    WebhookAuthz, WebhookDryRun, WebhookHandler, WebhookVerifier, session_principal,
};

/// One recorded delivery: path, headers, parsed JSON body.
type Deliveries = Arc<Mutex<Vec<(String, HeaderMap, Value)>>>;

#[derive(Clone)]
struct Stub {
    deliveries: Deliveries,
    /// Scripted `(status, body)` responses, popped per call; last repeats.
    script: Arc<Mutex<Vec<(u16, Value)>>>,
    /// Sleep before answering (timeout tests).
    delay: Option<Duration>,
}

async fn stub_handler(State(stub): State<Stub>, headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    // Record before any delay: on a client-side timeout hyper drops this
    // future, and the delivery must still be observable.
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    stub.deliveries
        .lock()
        .expect("deliveries lock")
        .push(("/hook".to_owned(), headers, parsed));
    if let Some(delay) = stub.delay {
        tokio::time::sleep(delay).await;
    }
    let (status, body) = {
        let mut script = stub.script.lock().expect("script lock");
        if script.len() > 1 {
            script.remove(0)
        } else {
            script.first().cloned().unwrap_or((200, json!({})))
        }
    };
    (
        StatusCode::from_u16(status).expect("scripted status"),
        axum::Json(body),
    )
}

async fn serve_stub(script: Vec<(u16, Value)>, delay: Option<Duration>) -> (SocketAddr, Deliveries) {
    let deliveries: Deliveries = Arc::new(Mutex::new(Vec::new()));
    let stub = Stub {
        deliveries: Arc::clone(&deliveries),
        script: Arc::new(Mutex::new(script)),
        delay,
    };
    let app = Router::new().route("/hook", post(stub_handler)).with_state(stub);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (addr, deliveries)
}

fn client(addr: SocketAddr, execute_timeout: Option<Duration>) -> Arc<HostClient> {
    Arc::new(
        HostClient::new(HostClientConfig {
            base_url: format!("http://{addr}"),
            secret: Some("test-secret".to_owned()),
            timeouts: Timeouts {
                execute: execute_timeout.unwrap_or(Timeouts::default().execute),
                ..Timeouts::default()
            },
            retry: RetryPolicy {
                execute_attempts: 2,
                backoff: Duration::from_millis(10),
            },
            danger_allow_remote_http: false,
            danger_allow_missing_secret: false,
        })
        .expect("client config"),
    )
}

fn route() -> Route {
    Route::from_str("POST /hook").expect("route")
}

fn params() -> cleverhans_core::JsonMap {
    let mut map = cleverhans_core::JsonMap::new();
    map.insert("recordId".to_owned(), json!("r-1"));
    map
}

fn wrapped() -> Value {
    session_principal("s_test", json!({"user_id": "alex", "roles": ["editor"]}))
}

#[tokio::test]
async fn execute_delivers_the_contract_shape_and_returns_the_result() {
    let (addr, deliveries) =
        serve_stub(vec![(200, json!({"outcome": "executed", "result": {"ok": true}}))], None).await;
    let handler = WebhookHandler::new(client(addr, None), "record.touch", route());

    let result = handler.execute(&params(), &wrapped()).await.expect("executed");

    assert_eq!(result, json!({"ok": true}));
    let log = deliveries.lock().expect("lock");
    let (_, headers, body) = &log[0];
    assert_eq!(headers["authorization"], "Bearer test-secret");
    assert_eq!(headers["x-cleverhans-webhook-version"], "1");
    assert!(headers.contains_key("x-cleverhans-delivery"));
    assert_eq!(body["kind"], "execute");
    assert_eq!(body["webhook_version"], 1);
    assert_eq!(body["session_id"], "s_test");
    assert_eq!(body["action_id"], "record.touch");
    assert_eq!(body["params"], json!({"recordId": "r-1"}));
    // The wire carries the host principal verbatim — no session envelope.
    assert_eq!(body["principal"], json!({"user_id": "alex", "roles": ["editor"]}));
    assert_eq!(body["attempt"], 1);
    assert!(body["idempotency_key"].is_string());
    assert!(body.get("headers").is_none());
}

#[tokio::test]
async fn execute_rejected_maps_to_handler_rejected_with_the_host_reason() {
    let (addr, _) =
        serve_stub(vec![(200, json!({"outcome": "rejected", "reason": "locked"}))], None).await;
    let handler = WebhookHandler::new(client(addr, None), "record.touch", route());

    let err = handler.execute(&params(), &wrapped()).await.expect_err("rejected");

    assert_eq!(err.to_string(), "rejected: locked");
}

#[tokio::test]
async fn execute_5xx_is_answered_internal_and_never_retried() {
    let (addr, deliveries) = serve_stub(vec![(500, json!({"error": "boom"}))], None).await;
    let handler = WebhookHandler::new(client(addr, None), "record.touch", route());

    let err = handler.execute(&params(), &wrapped()).await.expect_err("internal");

    assert!(err.to_string().starts_with("internal:"), "got {err}");
    assert_eq!(deliveries.lock().expect("lock").len(), 1, "answered call retried");
}

#[tokio::test]
async fn execute_timeout_retries_with_the_same_idempotency_key() {
    // Delay past the execute timeout: every attempt times out, retry loop
    // runs to exhaustion; assert both deliveries share the idempotency key.
    let (addr, deliveries) = serve_stub(
        vec![(200, json!({"outcome": "executed", "result": {}}))],
        Some(Duration::from_millis(300)),
    )
    .await;
    let handler = WebhookHandler::new(
        client(addr, Some(Duration::from_millis(80))),
        "record.touch",
        route(),
    );

    let err = handler.execute(&params(), &wrapped()).await.expect_err("unknown");

    assert_eq!(err.to_string(), "internal: execution outcome unknown");
    // Give the delayed stub handlers time to record both deliveries.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let log = deliveries.lock().expect("lock");
    assert_eq!(log.len(), 2, "expected two attempts");
    assert_eq!(log[0].2["idempotency_key"], log[1].2["idempotency_key"]);
    assert_eq!(log[0].2["attempt"], 1);
    assert_eq!(log[1].2["attempt"], 2);
    assert_ne!(
        log[0].1["x-cleverhans-delivery"], log[1].1["x-cleverhans-delivery"],
        "delivery id must change per attempt"
    );
}

#[tokio::test]
async fn dry_run_maps_preview_rejected_and_failure() {
    let (addr, _) = serve_stub(
        vec![
            (200, json!({"outcome": "preview", "preview": {"affected_count": 2}})),
            (200, json!({"outcome": "rejected", "reason": "no access"})),
            (500, json!({})),
        ],
        None,
    )
    .await;
    let dry_run = WebhookDryRun::new(client(addr, None), "record.touch", route());

    let preview = dry_run.dry_run(&params(), &wrapped()).await.expect("preview");
    assert_eq!(preview.affected_count, 2);

    let rejected = dry_run.dry_run(&params(), &wrapped()).await.expect_err("rejected");
    assert_eq!(rejected.to_string(), "rejected: no access");

    let failed = dry_run.dry_run(&params(), &wrapped()).await.expect_err("internal");
    assert!(failed.to_string().starts_with("internal:"), "got {failed}");
}

#[tokio::test]
async fn authorize_maps_allow_deny_and_fails_closed() {
    let (addr, deliveries) = serve_stub(
        vec![
            (200, json!({"decision": "allow"})),
            (200, json!({"decision": "deny", "reason": "editors only"})),
            (500, json!({})),
        ],
        None,
    )
    .await;
    let authz = WebhookAuthz::new(client(addr, None), route());

    assert!(matches!(
        authz.authorize(&wrapped(), "record.touch", &params()).await,
        AuthzDecision::Allow
    ));
    match authz.authorize(&wrapped(), "record.touch", &params()).await {
        AuthzDecision::Deny(reason) => assert_eq!(reason, "editors only"),
        AuthzDecision::Allow => panic!("expected deny"),
    }
    match authz.authorize(&wrapped(), "record.touch", &params()).await {
        AuthzDecision::Deny(reason) => assert_eq!(reason, "authorization unavailable"),
        AuthzDecision::Allow => panic!("expected fail-closed deny"),
    }
    assert_eq!(deliveries.lock().expect("lock")[0].2["kind"], "authorize");
}

#[tokio::test]
async fn verify_forwards_only_allowlisted_headers_and_wraps_the_principal() {
    let (addr, deliveries) =
        serve_stub(vec![(200, json!({"principal": {"user_id": "u_1"}}))], None).await;
    let verifier = WebhookVerifier::new(
        client(addr, None),
        route(),
        vec!["authorization".to_owned(), "cookie".to_owned()],
    );
    let mut headers = http::HeaderMap::new();
    headers.insert("authorization", "Bearer user-token".parse().expect("value"));
    headers.insert("x-forwarded-for", "10.0.0.1".parse().expect("value"));

    let principal = verifier.verify(&headers).await.expect("established");

    assert_eq!(principal["principal"], json!({"user_id": "u_1"}));
    assert!(principal["session_id"].as_str().expect("session id").starts_with("s_"));
    let log = deliveries.lock().expect("lock");
    assert_eq!(
        log[0].2["headers"],
        json!({"authorization": "Bearer user-token"}),
        "only allowlisted headers cross the wire"
    );
}

#[tokio::test]
async fn verify_maps_refusals_and_failures_per_the_spec_table() {
    let (addr, _) = serve_stub(vec![(401, json!({"reason": "expired"})), (500, json!({}))], None).await;
    let verifier = WebhookVerifier::new(client(addr, None), route(), vec![]);

    assert_eq!(
        verifier.verify(&http::HeaderMap::new()).await.expect_err("refused"),
        http::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        verifier.verify(&http::HeaderMap::new()).await.expect_err("failed closed"),
        http::StatusCode::SERVICE_UNAVAILABLE
    );
}

#[test]
fn client_refuses_remote_plaintext_and_missing_secret() {
    let remote = HostClient::new(HostClientConfig {
        base_url: "http://10.0.0.5:3000".to_owned(),
        secret: Some("s".to_owned()),
        timeouts: Timeouts::default(),
        retry: RetryPolicy::default(),
        danger_allow_remote_http: false,
        danger_allow_missing_secret: false,
    });
    assert!(remote.is_err(), "remote plaintext must be refused");

    let no_secret = HostClient::new(HostClientConfig {
        base_url: "http://127.0.0.1:3000".to_owned(),
        secret: None,
        timeouts: Timeouts::default(),
        retry: RetryPolicy::default(),
        danger_allow_remote_http: false,
        danger_allow_missing_secret: false,
    });
    assert!(no_secret.is_err(), "missing secret must be refused");

    let https_remote = HostClient::new(HostClientConfig {
        base_url: "https://host.internal".to_owned(),
        secret: Some("s".to_owned()),
        timeouts: Timeouts::default(),
        retry: RetryPolicy::default(),
        danger_allow_remote_http: false,
        danger_allow_missing_secret: false,
    });
    assert!(https_remote.is_ok(), "https remote is fine");
}

#[test]
fn route_parsing_accepts_method_path_and_rejects_garbage() {
    let route = Route::from_str("post /internal/hooks/x").expect("parses");
    assert_eq!(route.method, reqwest::Method::POST);
    assert_eq!(route.path, "/internal/hooks/x");

    assert!(Route::from_str("no-path").is_err());
    assert!(Route::from_str("POST relative/path").is_err());
    assert!(Route::from_str("POST /a b").is_err());
}
