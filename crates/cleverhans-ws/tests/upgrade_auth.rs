//! Upgrade-time authentication for both router flavors: the principal is
//! established before any envelope traffic, or the request is refused.
//!
//! A real server on an ephemeral port, because the WebSocket upgrade needs a
//! live HTTP/1.1 connection — `tower::oneshot` cannot carry hyper's upgrade
//! capability.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::extract::Extension;
use axum::http::{HeaderMap, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use cleverhans_core::JsonMap;
use cleverhans_core::agent::Agent;
use cleverhans_core::envelope::Context;
use cleverhans_core::registry::{ParamSpec, Registry};
use cleverhans_core::seams::{AuthzDecision, AuthzResolver, ContextParamResolver};
use cleverhans_core::test_util::ScriptedLlm;
use cleverhans_ws::{
    AsyncPrincipalExtractor, PrincipalExtractor, agent_router, agent_router_async,
    agent_router_from_extension,
};

#[derive(Clone, Debug, PartialEq)]
struct User {
    name: String,
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

/// Performs the upgrade handshake and returns the HTTP status line's code.
async fn upgrade_status(addr: SocketAddr, extra_header: Option<(&str, &str)>) -> u16 {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let extra = extra_header
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET /agent HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Connection: Upgrade\r\n\
         Upgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         {extra}\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("send handshake");
    let mut response = vec![0u8; 256];
    let read = stream.read(&mut response).await.expect("read response");
    let status_line = String::from_utf8_lossy(&response[..read]);
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status code in response: {status_line}"))
}

#[tokio::test]
async fn extension_router_upgrades_when_middleware_supplied_a_principal() {
    let app = agent_router_from_extension("/agent", agent()).layer(Extension(User {
        name: "from-middleware".to_owned(),
    }));

    let status = upgrade_status(serve(app).await, None).await;

    assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS.as_u16());
}

#[tokio::test]
async fn extension_router_refuses_when_no_principal_was_installed() {
    let app = agent_router_from_extension("/agent", agent());

    let status = upgrade_status(serve(app).await, None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED.as_u16());
}

struct HeaderAuth;

impl PrincipalExtractor<User> for HeaderAuth {
    fn extract(&self, headers: &HeaderMap) -> Result<User, StatusCode> {
        headers
            .get("x-user")
            .and_then(|value| value.to_str().ok())
            .map(|name| User {
                name: name.to_owned(),
            })
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

#[tokio::test]
async fn header_router_upgrades_when_the_extractor_accepts() {
    let app = agent_router("/agent", agent(), Arc::new(HeaderAuth));

    let status = upgrade_status(serve(app).await, Some(("x-user", "alex"))).await;

    assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS.as_u16());
}

#[tokio::test]
async fn header_router_refuses_with_the_extractor_status() {
    let app = agent_router("/agent", agent(), Arc::new(HeaderAuth));

    let status = upgrade_status(serve(app).await, None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED.as_u16());
}

/// An awaiting extractor, as a session-store lookup or the standalone
/// service's verify_session webhook would be.
struct AsyncHeaderAuth;

#[async_trait]
impl AsyncPrincipalExtractor<User> for AsyncHeaderAuth {
    async fn extract(&self, headers: &HeaderMap) -> Result<User, StatusCode> {
        tokio::task::yield_now().await; // prove the await point is real
        headers
            .get("x-user")
            .and_then(|value| value.to_str().ok())
            .map(|name| User {
                name: name.to_owned(),
            })
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)
    }
}

#[tokio::test]
async fn async_router_upgrades_when_the_extractor_accepts() {
    let app = agent_router_async("/agent", agent(), Arc::new(AsyncHeaderAuth));

    let status = upgrade_status(serve(app).await, Some(("x-user", "alex"))).await;

    assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS.as_u16());
}

#[tokio::test]
async fn async_router_refuses_with_the_extractor_status_before_upgrade() {
    let app = agent_router_async("/agent", agent(), Arc::new(AsyncHeaderAuth));

    let status = upgrade_status(serve(app).await, None).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE.as_u16());
}
