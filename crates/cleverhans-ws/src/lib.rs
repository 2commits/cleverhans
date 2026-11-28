//! Axum adapter for the WebSocket + JSON binding of the CleverHans envelope
//! (spec §11). This is the binding `@cleverhans/react`'s
//! `createWebSocketTransport` speaks; the session loop itself lives in
//! `cleverhans-ws-core` and is framework-neutral.
//!
//! Same transport rules as the gRPC binding: the stream is authenticated
//! before any envelope traffic (principal from HTTP headers at upgrade), and
//! the first client message must be `init` (spec §6.1).
//!
//! WebSocket specifics handled here:
//!
//! - **Subprotocol.** The adapter selects [`SUBPROTOCOL`]
//!   (`cleverhans.v0.1`) whenever the client offers it, so a browser that
//!   offers it — `@cleverhans/react` does whenever its caller passes
//!   `protocols`, for example to carry a token — completes the handshake.
//!   Every other offered value stays in the upgrade request's
//!   `Sec-WebSocket-Protocol` header for the principal extractor to read
//!   ([`offered_subprotocols`]). A client that offers none gets none.
//! - **Close frame.** When the session ends after a non-recoverable error
//!   (spec §6.3) the adapter sends a close frame with code `1008` (policy
//!   violation) and the error's `code` as the reason, then waits briefly
//!   for the client's answer. When the client closes first, the WebSocket
//!   library answers its close frame.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::http::header::SEC_WEBSOCKET_PROTOCOL;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use cleverhans_core::agent::Agent;
use cleverhans_core::async_trait;
pub use cleverhans_ws_core::{
    CLOSE_POLICY_VIOLATION, SUBPROTOCOL, SessionEnd, SessionExit, run_session, to_json,
};

/// How long the adapter waits for the client's close frame after it sent
/// its own, before it drops the connection.
const CLOSE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// The subprotocols the client offered in the upgrade request, in order,
/// without [`SUBPROTOCOL`] — the values a host may use to carry a
/// credential (a browser cannot set other headers on a WebSocket). Read it
/// from a [`PrincipalExtractor`]:
///
/// ```
/// # use axum::http::HeaderMap;
/// let mut headers = HeaderMap::new();
/// headers.insert("sec-websocket-protocol", "cleverhans.v0.1, token-abc".parse().unwrap());
/// let token = cleverhans_ws::offered_subprotocols(&headers).next();
/// assert_eq!(token.as_deref(), Some("token-abc"));
/// ```
pub fn offered_subprotocols(headers: &HeaderMap) -> impl Iterator<Item = String> + '_ {
    headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != SUBPROTOCOL)
        .map(str::to_owned)
}

/// Authenticates an upgrade request: maps HTTP headers (cookie, bearer
/// token, …) onto the app's principal. The framework never constructs a
/// principal (spec §10).
pub trait PrincipalExtractor<P>: Send + Sync {
    /// Extracts the principal, or refuses the upgrade.
    ///
    /// # Errors
    ///
    /// A status (typically `401 UNAUTHORIZED`) that rejects the request
    /// before any envelope traffic is processed.
    fn extract(&self, headers: &HeaderMap) -> Result<P, StatusCode>;
}

/// [`PrincipalExtractor`] for verifications that await — a session-store
/// lookup, or the standalone service's `verify_session` webhook (spec
/// §14.3). Verification completes *before* the upgrade is accepted, so a
/// refused principal is an HTTP status, never a connected-then-dropped
/// socket.
#[async_trait]
pub trait AsyncPrincipalExtractor<P>: Send + Sync {
    /// Extracts the principal, or refuses the upgrade.
    ///
    /// # Errors
    ///
    /// A status (typically `401 UNAUTHORIZED`, or `503` for a failed
    /// upstream verification) that rejects the request before any envelope
    /// traffic is processed.
    async fn extract(&self, headers: &HeaderMap) -> Result<P, StatusCode>;
}

struct WsState<P> {
    agent: Arc<Agent<P>>,
    principals: Arc<dyn PrincipalExtractor<P>>,
}

struct AsyncWsState<P> {
    agent: Arc<Agent<P>>,
    principals: Arc<dyn AsyncPrincipalExtractor<P>>,
}

/// A router serving the envelope stream at the given path (e.g. `/agent`).
/// Merge it into the app's axum router.
pub fn agent_router<P: Send + Sync + 'static>(
    path: &str,
    agent: Arc<Agent<P>>,
    principals: Arc<dyn PrincipalExtractor<P>>,
) -> Router {
    Router::new()
        .route(path, get(upgrade_handler::<P>))
        .with_state(Arc::new(WsState { agent, principals }))
}

/// A router serving the envelope stream with the principal taken from
/// request [extensions](axum::Extension) — for apps whose existing auth
/// middleware (a tower/axum layer) already resolves the caller. The
/// framework still never constructs a principal; a request that reaches the
/// route without one is refused with `401` before any envelope traffic.
///
/// ```no_run
/// # use std::sync::Arc;
/// # use axum::{Extension, Router};
/// # #[derive(Clone)] struct MyUser;
/// # fn app(agent: Arc<cleverhans_core::agent::Agent<MyUser>>, auth_layer: Extension<MyUser>) -> Router {
/// Router::new()
///     .merge(cleverhans_ws::agent_router_from_extension("/agent", agent))
///     .layer(auth_layer) // your existing middleware inserts MyUser
/// # }
/// ```
pub fn agent_router_from_extension<P: Clone + Send + Sync + 'static>(
    path: &str,
    agent: Arc<Agent<P>>,
) -> Router {
    Router::new()
        .route(path, get(extension_upgrade_handler::<P>))
        .with_state(agent)
}

async fn extension_upgrade_handler<P: Clone + Send + Sync + 'static>(
    State(agent): State<Arc<Agent<P>>>,
    principal: Option<Extension<P>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(Extension(principal)) = principal else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    upgrade
        .protocols([SUBPROTOCOL])
        .on_upgrade(move |socket| serve_socket(socket, agent, principal))
        .into_response()
}

async fn upgrade_handler<P: Send + Sync + 'static>(
    State(state): State<Arc<WsState<P>>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let principal = match state.principals.extract(&headers) {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    upgrade
        .protocols([SUBPROTOCOL])
        .on_upgrade(move |socket| serve_socket(socket, Arc::clone(&state.agent), principal))
        .into_response()
}

/// A router serving the envelope stream with the principal produced by an
/// [`AsyncPrincipalExtractor`] — the mounting style for verifications that
/// leave the process (session-store reads, the §14.3 `verify_session`
/// webhook of the standalone service topology).
pub fn agent_router_async<P: Send + Sync + 'static>(
    path: &str,
    agent: Arc<Agent<P>>,
    principals: Arc<dyn AsyncPrincipalExtractor<P>>,
) -> Router {
    Router::new()
        .route(path, get(async_upgrade_handler::<P>))
        .with_state(Arc::new(AsyncWsState { agent, principals }))
}

async fn async_upgrade_handler<P: Send + Sync + 'static>(
    State(state): State<Arc<AsyncWsState<P>>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let principal = match state.principals.extract(&headers).await {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    upgrade
        .protocols([SUBPROTOCOL])
        .on_upgrade(move |socket| serve_socket(socket, Arc::clone(&state.agent), principal))
        .into_response()
}

async fn serve_socket<P: Send + Sync>(socket: WebSocket, agent: Arc<Agent<P>>, principal: P) {
    use futures_util::SinkExt;
    use futures_util::future::ready;

    let (mut sink, stream) = socket.split();
    // A read error ends the inbound stream; non-text frames (binary,
    // ping/pong, close) carry no envelope traffic. Reading on after a close
    // frame lets the WebSocket library send its answer.
    let inbound = stream
        .take_while(|frame| ready(frame.is_ok()))
        .filter_map(|frame| async {
            match frame {
                Ok(Message::Text(text)) => Some(text.to_string()),
                _ => None,
            }
        });
    tokio::pin!(inbound);
    let (tx, rx) = mpsc::channel::<String>(32);
    let writer = async {
        // Owns the receiver, so a failed write drops it and the session
        // sees the receiver gone instead of blocking on a full channel.
        let mut rx = rx;
        while let Some(json) = rx.recv().await {
            if sink.send(Message::Text(json.into())).await.is_err() {
                return false;
            }
        }
        true
    };
    let (exit, writable) =
        tokio::join!(run_session(agent, principal, inbound.as_mut(), tx), writer);
    let SessionExit::Ended(end) = exit else {
        return;
    };
    if !writable {
        return;
    }
    // Spec §6.3: the session is over. Every event is written (the writer
    // drained the channel); close with 1008 and the error code, then read
    // until the client answers so the close handshake completes.
    let close = Message::Close(Some(CloseFrame {
        code: end.close_code(),
        reason: end.close_reason().into(),
    }));
    if sink.send(close).await.is_err() {
        return;
    }
    let _ = tokio::time::timeout(CLOSE_HANDSHAKE_TIMEOUT, async {
        while inbound.next().await.is_some() {}
    })
    .await;
}
