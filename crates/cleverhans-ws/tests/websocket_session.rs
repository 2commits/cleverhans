//! The WebSocket side of a session with a real client: subprotocol
//! selection at the upgrade, and the close frame that ends a session after a
//! non-recoverable error (spec §6.3).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::extract::Extension;
use axum::http::{HeaderMap, StatusCode};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use cleverhans_core::JsonMap;
use cleverhans_core::agent::Agent;
use cleverhans_core::envelope::Context;
use cleverhans_core::registry::{ParamSpec, Registry};
use cleverhans_core::seams::{AuthzDecision, AuthzResolver, ContextParamResolver};
use cleverhans_core::test_util::ScriptedLlm;
use cleverhans_ws::{
    PrincipalExtractor, SUBPROTOCOL, agent_router, agent_router_from_extension,
    offered_subprotocols,
};

type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const READ_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq)]
struct User {
    token: Option<String>,
}

struct AllowAll;

#[async_trait]
impl AuthzResolver<User> for AllowAll {
    async fn authorize(&self, _: &User, _: &str, _: &JsonMap) -> AuthzDecision {
        AuthzDecision::Allow
    }
}

struct NoContextParams;

impl ContextParamResolver for NoContextParams {
    fn resolve(&self, _: &str, _: &ParamSpec, _: &Context) -> Option<serde_json::Value> {
        None
    }
}

fn agent() -> Arc<Agent<User>> {
    Arc::new(Agent::new(
        Arc::new(Registry::builder().build().expect("empty registry")),
        Arc::new(ScriptedLlm::new([])),
        Arc::new(AllowAll),
        Arc::new(NoContextParams),
    ))
}

/// Takes the principal from the subprotocols the client offered besides the
/// binding's own — the way a browser carries a token — and records what it
/// saw.
#[derive(Default)]
struct SubprotocolToken {
    seen: Mutex<Vec<Option<String>>>,
}

impl PrincipalExtractor<User> for SubprotocolToken {
    fn extract(&self, headers: &HeaderMap) -> Result<User, StatusCode> {
        let token = offered_subprotocols(headers).next();
        self.seen.lock().expect("lock").push(token.clone());
        Ok(User { token })
    }
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    addr
}

async fn connect(addr: SocketAddr, protocols: Option<&'static str>) -> (Client, Option<String>) {
    let mut request = format!("ws://{addr}/agent")
        .into_client_request()
        .expect("client request");
    if let Some(protocols) = protocols {
        request.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static(protocols),
        );
    }
    let (client, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("handshake");
    let selected = response
        .headers()
        .get("sec-websocket-protocol")
        .map(|value| value.to_str().expect("ascii").to_owned());
    (client, selected)
}

/// Every frame the server sends until it closes or the stream ends.
async fn read_until_close(client: &mut Client) -> (Vec<Value>, Option<CloseFrame<'static>>) {
    let mut events = Vec::new();
    loop {
        let next = tokio::time::timeout(READ_TIMEOUT, client.next())
            .await
            .expect("the server neither answered nor closed");
        match next {
            Some(Ok(Message::Text(text))) => {
                events.push(serde_json::from_str(&text).expect("JSON frame"));
            }
            Some(Ok(Message::Close(frame))) => return (events, frame),
            Some(Ok(_)) => {}
            Some(Err(err)) => panic!("connection failed instead of closing: {err}"),
            None => return (events, None),
        }
    }
}

#[tokio::test]
async fn offering_the_binding_name_and_a_token_selects_the_name_and_hands_the_token_to_the_extractor()
 {
    let extractor = Arc::new(SubprotocolToken::default());
    let app = agent_router("/agent", agent(), Arc::clone(&extractor) as Arc<_>);
    let addr = serve(app).await;

    let (mut client, selected) = connect(addr, Some("cleverhans.v0.1, token-abc")).await;
    client
        .send(Message::Text(
            json!({"type": "init", "spec_version": "0.1", "context": {"route": "/"}}).to_string(),
        ))
        .await
        .expect("send init");
    client.close(None).await.expect("close");

    assert_eq!(selected.as_deref(), Some(SUBPROTOCOL));
    assert_eq!(SUBPROTOCOL, "cleverhans.v0.1");
    assert_eq!(
        *extractor.seen.lock().expect("lock"),
        vec![Some("token-abc".to_owned())]
    );
}

#[tokio::test]
async fn a_client_that_offers_no_subprotocol_connects_and_gets_none() {
    let extractor = Arc::new(SubprotocolToken::default());
    let app = agent_router("/agent", agent(), Arc::clone(&extractor) as Arc<_>);
    let addr = serve(app).await;

    let (_client, selected) = connect(addr, None).await;

    assert_eq!(selected, None);
    assert_eq!(*extractor.seen.lock().expect("lock"), vec![None]);
}

fn extension_app() -> Router {
    agent_router_from_extension("/agent", agent()).layer(Extension(User { token: None }))
}

#[tokio::test]
async fn the_extension_router_selects_the_binding_name_too() {
    let addr = serve(extension_app()).await;

    let (_client, selected) = connect(addr, Some("cleverhans.v0.1")).await;

    assert_eq!(selected.as_deref(), Some(SUBPROTOCOL));
}

#[tokio::test]
async fn init_required_closes_the_connection_with_1008_and_the_code() {
    let addr = serve(extension_app()).await;
    let (mut client, _) = connect(addr, None).await;

    client
        .send(Message::Text(
            json!({"type": "user_message", "text": "hi", "client_msg_id": "c-1"}).to_string(),
        ))
        .await
        .expect("send");
    let (events, close) = read_until_close(&mut client).await;

    assert_eq!(
        events,
        vec![json!({
            "type": "error",
            "code": "init_required",
            "message": "first message on a stream must be `init` (spec §6.1)",
            "recoverable": false,
        })]
    );
    let close = close.expect("a close frame, not a dropped connection");
    assert_eq!(
        (u16::from(close.code), close.reason.as_ref()),
        (1008, "init_required")
    );
}

#[tokio::test]
async fn unsupported_spec_version_closes_the_connection_with_1008_and_the_code() {
    let addr = serve(extension_app()).await;
    let (mut client, _) = connect(addr, None).await;

    client
        .send(Message::Text(
            json!({"type": "init", "spec_version": "9.9", "context": {"route": "/"}}).to_string(),
        ))
        .await
        .expect("send init");
    // Sent before the close arrives; the server must not answer it.
    client
        .send(Message::Text(
            json!({"type": "confirm_action", "proposal_id": "prop-1"}).to_string(),
        ))
        .await
        .expect("send confirm");
    let (events, close) = read_until_close(&mut client).await;

    assert_eq!(events.len(), 1, "nothing after the error: {events:?}");
    assert_eq!(
        (
            events[0]["code"].as_str(),
            events[0]["recoverable"].as_bool()
        ),
        (Some("unsupported_spec_version"), Some(false))
    );
    let close = close.expect("a close frame, not a dropped connection");
    assert_eq!(
        (close.code, close.reason.as_ref()),
        (CloseCode::Policy, "unsupported_spec_version")
    );
    // The client answers the close; the connection then ends cleanly.
    let after = tokio::time::timeout(READ_TIMEOUT, client.next())
        .await
        .expect("the connection ends after the close handshake");
    assert!(
        matches!(after, None | Some(Err(WsError::ConnectionClosed))),
        "got {after:?}"
    );
}

#[tokio::test]
async fn a_client_that_closes_first_gets_a_normal_close_answer() {
    let addr = serve(extension_app()).await;
    let (mut client, _) = connect(addr, None).await;
    client
        .send(Message::Text(
            json!({"type": "init", "spec_version": "0.1", "context": {"route": "/"}}).to_string(),
        ))
        .await
        .expect("send init");

    client
        .close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "bye".into(),
        }))
        .await
        .expect("close");
    let (events, close) = read_until_close(&mut client).await;

    assert!(events.is_empty(), "init is silent: {events:?}");
    let close = close.expect("the server answers the close frame");
    assert_eq!(close.code, CloseCode::Normal);
}
