//! End-to-end over the real transport: the assembled service (scripted
//! LLM), a MockHost upstream, and a genuine WebSocket client driving
//! init → user_message → action_proposal → confirm_action → executed.

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use cleverhans_conformance::fixture::AuthzScript;
use cleverhans_conformance::mock_host::{
    HostBehavior, HostResponse, HostScript, default_principal,
};
use cleverhans_conformance::{Fixture, MockHost};
use cleverhans_serve::config::Config;
use cleverhans_serve::{build_app, load_schema};

fn fixture() -> Fixture {
    let path = format!(
        "{}/../../spec/vectors/fixtures/co-buyer.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("read fixture")).expect("fixture")
}

fn registry_json() -> String {
    // The fixture wraps the registry document; the service loads the
    // document itself.
    let raw: Value = serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/../../spec/vectors/fixtures/co-buyer.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("read fixture"),
    )
    .expect("parse fixture");
    raw["registry"].to_string()
}

const SECRET: &str = "e2e-secret";

fn config_toml(upstream: &str) -> String {
    format!(
        r#"
[upstream]
base_url = "{upstream}"
secret_env = "E2E_SECRET"

[auth]
verify = "POST /cleverhans/verify_session"

[authz]
endpoint = "POST /cleverhans/authorize"

[llm]
provider = "scripted"
script = [[{{ tool_call = {{ name = "transaction.coBuyer.remove", arguments = {{}} }} }}]]

[actions."*"]
execute = "POST /cleverhans/execute"
dry_run = "POST /cleverhans/dry_run"

[actions."transaction.coBuyer.remove".slots]
title = {{ const = "Remove co-buyer" }}
detail = {{ preview = "summary" }}

[actions."transaction.setCountry".slots]
title = {{ const = "Set country" }}
"#
    )
}

async fn serve_app(host: &MockHost) -> SocketAddr {
    // SAFETY: test-only env mutation, name unique to this test binary.
    unsafe { std::env::set_var("E2E_SECRET", SECRET) };
    let schema = load_schema(&registry_json()).expect("schema");
    let config = Config::from_toml(&config_toml(&host.base_url())).expect("config");
    let resolved = config.resolve(&schema).expect("resolve");
    let llm = cleverhans::llm::build_llm(config.llm.resolve().expect("llm"));
    let app = build_app(&resolved, &schema, llm).expect("build app");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    addr
}

async fn next_event(
    ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
) -> Value {
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("event within 5s")
            .expect("stream open")
            .expect("frame");
        if let Message::Text(text) = message {
            let event: Value = serde_json::from_str(&text).expect("event JSON");
            // Skip streaming deltas; assert on authoritative events only.
            if event["type"] == "chat_message" && event["done"] == false {
                continue;
            }
            return event;
        }
    }
}

#[tokio::test]
async fn full_confirm_flow_executes_through_the_webhooks() {
    let host = MockHost::spawn(fixture(), AuthzScript::default(), HostScript::new(), SECRET).await;
    let addr = serve_app(&host).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/agent"))
        .await
        .expect("upgrade accepted");

    ws.send(Message::Text(
        json!({"type": "init", "spec_version": "0.1.0-draft",
               "context": {"route": "/transactions/tx_581",
                            "selected_record_id": "tx_581", "view_type": "detail"}})
        .to_string(),
    ))
    .await
    .expect("send init");
    ws.send(Message::Text(
        json!({"type": "user_message", "text": "remove the co-buyer", "client_msg_id": "c-1"})
            .to_string(),
    ))
    .await
    .expect("send message");

    let proposal = next_event(&mut ws).await;
    assert_eq!(proposal["type"], "action_proposal", "got {proposal}");
    assert_eq!(proposal["action_id"], "transaction.coBuyer.remove");
    assert_eq!(proposal["params"], json!({"transactionId": "tx_581"}));
    assert_eq!(proposal["slots"]["title"], "Remove co-buyer");
    assert_eq!(proposal["preview"]["affected_count"], 1);

    ws.send(Message::Text(
        json!({"type": "confirm_action", "proposal_id": proposal["proposal_id"]}).to_string(),
    ))
    .await
    .expect("send confirm");

    let executed = next_event(&mut ws).await;
    assert_eq!(executed["type"], "proposal_state_changed", "got {executed}");
    assert_eq!(executed["state"], "executed");
    assert_eq!(executed["result"], json!({"removed": true}));

    // The host saw the verbatim principal from verify_session on execute.
    let deliveries = host.deliveries.lock().expect("deliveries");
    let execute = deliveries
        .iter()
        .find(|delivery| delivery.endpoint == "execute")
        .expect("an execute delivery");
    assert_eq!(execute.body["principal"], default_principal());
    assert_eq!(execute.headers["authorization"], format!("Bearer {SECRET}"));
}

#[tokio::test]
async fn refused_verification_refuses_the_upgrade_with_the_same_status() {
    let overrides: HostScript = [(
        "verify_session".to_owned(),
        vec![HostBehavior {
            respond: Some(HostResponse {
                status: 401,
                body: json!({"reason": "expired session"}),
            }),
            timeout: false,
        }],
    )]
    .into();
    let host = MockHost::spawn(fixture(), AuthzScript::default(), overrides, SECRET).await;
    let addr = serve_app(&host).await;

    let err = tokio_tungstenite::connect_async(format!("ws://{addr}/agent"))
        .await
        .expect_err("upgrade must be refused");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 401);
        }
        other => panic!("expected an HTTP refusal, got {other:?}"),
    }
}
