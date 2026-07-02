//! WebSocket + JSON binding of the CleverHans envelope (spec §11), built on
//! axum. This is the binding `@cleverhans/react`'s `createWebSocketTransport`
//! speaks: each frame is one envelope message in the serde JSON encoding of
//! `cleverhans-core` — no translation layer, the wire shapes *are* the core
//! types.
//!
//! Same transport rules as the gRPC binding: the stream is authenticated
//! before any envelope traffic (principal from HTTP headers at upgrade), and
//! the first client message must be `init` (spec §6.1).

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{Stream, StreamExt};
use tokio::sync::mpsc;

use cleverhans_core::agent::{Agent, Session};
use cleverhans_core::envelope::{ClientEvent, ServerEvent};

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

struct WsState<P> {
    agent: Arc<Agent<P>>,
    principals: Arc<dyn PrincipalExtractor<P>>,
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
        .on_upgrade(move |socket| serve_socket(socket, Arc::clone(&state.agent), principal))
        .into_response()
}

async fn serve_socket<P: Send + Sync>(socket: WebSocket, agent: Arc<Agent<P>>, principal: P) {
    use futures_util::SinkExt;

    let (mut sink, stream) = socket.split();
    // Non-text frames (binary, ping/pong, close) carry no envelope traffic.
    let inbound = stream.filter_map(|frame| async {
        match frame {
            Ok(Message::Text(text)) => Some(text.to_string()),
            _ => None,
        }
    });
    tokio::pin!(inbound);
    let (tx, mut rx) = mpsc::channel::<String>(32);
    let writer = async {
        while let Some(json) = rx.recv().await {
            if sink.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    };
    tokio::join!(run_session(agent, principal, inbound, tx), writer);
}

fn to_json(event: &ServerEvent) -> String {
    serde_json::to_string(event).unwrap_or_else(|_| {
        // Envelope types serialize infallibly; keep a defensive fallback.
        r#"{"type":"error","code":"encode_failed","message":"","recoverable":false}"#.to_owned()
    })
}

fn error_json(code: &str, message: &str, recoverable: bool) -> String {
    to_json(&ServerEvent::Error {
        code: code.to_owned(),
        message: message.to_owned(),
        recoverable,
    })
}

/// Drives one session over JSON text frames: decodes inbound events,
/// enforces init-first ordering, feeds the agent, encodes outbound events.
/// Factored out of the axum handler so it is testable against any stream.
pub async fn run_session<P, S>(
    agent: Arc<Agent<P>>,
    principal: P,
    mut inbound: S,
    tx: mpsc::Sender<String>,
) where
    P: Send + Sync,
    S: Stream<Item = String> + Unpin + Send,
{
    let mut session = Session::new(principal);
    let mut initialized = false;
    while let Some(frame) = inbound.next().await {
        let event: ClientEvent = match serde_json::from_str(&frame) {
            Ok(event) => event,
            Err(err) => {
                let malformed = error_json("malformed_event", &err.to_string(), true);
                if tx.send(malformed).await.is_err() {
                    return;
                }
                continue;
            }
        };
        if !initialized {
            if !matches!(event, ClientEvent::Init { .. }) {
                let _ = tx
                    .send(error_json(
                        "init_required",
                        "first message on a stream must be `init` (spec §6.1)",
                        false,
                    ))
                    .await;
                return;
            }
            initialized = true;
        }
        for server_event in agent.handle(&mut session, event).await {
            if tx.send(to_json(&server_event)).await.is_err() {
                return;
            }
        }
    }
}
