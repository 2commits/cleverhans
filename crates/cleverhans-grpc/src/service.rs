//! The tonic service hosting the agent loop behind a bidirectional stream.
//!
//! Transport-level rules enforced here, not in core: the stream must be
//! authenticated before any envelope traffic (principal extraction from
//! request metadata), and the first client message must be `Init`
//! (spec §6.1).

use std::pin::Pin;
use std::sync::Arc;

use cleverhans_core::agent::{Agent, Session};
use cleverhans_core::envelope::ClientEvent;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status, Streaming};

use crate::convert;
use crate::pb;

/// Authenticates a stream: maps transport metadata (e.g. an OAuth bearer
/// token) onto the app's principal type. The app implements this; the
/// framework never constructs a principal (spec §10).
pub trait PrincipalExtractor<P>: Send + Sync {
    /// Extracts the principal, or refuses the stream.
    ///
    /// # Errors
    ///
    /// A [`Status`] (typically `unauthenticated`) that aborts the RPC before
    /// any envelope traffic is processed.
    fn extract(&self, metadata: &MetadataMap) -> Result<P, Status>;
}

/// Tonic implementation of `cleverhans.v1.AgentService`: one [`Session`] per
/// stream, bound to the extracted principal for its whole lifetime.
pub struct AgentStreamService<P> {
    agent: Arc<Agent<P>>,
    principals: Arc<dyn PrincipalExtractor<P>>,
}

impl<P> AgentStreamService<P> {
    /// Assembles the service over a shared agent and the app's authenticator.
    pub fn new(agent: Arc<Agent<P>>, principals: Arc<dyn PrincipalExtractor<P>>) -> Self {
        Self { agent, principals }
    }
}

fn error_event(code: &str, message: String, recoverable: bool) -> pb::ServerEvent {
    convert::server_event(cleverhans_core::envelope::ServerEvent::Error {
        code: code.to_owned(),
        message,
        recoverable,
    })
}

/// Drives one session: decodes inbound events, enforces init-first ordering,
/// feeds the agent, encodes outbound events. Factored out of the tonic trait
/// so it is testable against any stream, not just [`Streaming`].
pub async fn run_session<P, S>(
    agent: Arc<Agent<P>>,
    principal: P,
    mut inbound: S,
    tx: mpsc::Sender<Result<pb::ServerEvent, Status>>,
) where
    P: Send + Sync,
    S: Stream<Item = Result<pb::ClientEvent, Status>> + Unpin + Send,
{
    let mut session = Session::new(principal);
    let mut initialized = false;
    while let Some(next) = inbound.next().await {
        let Ok(pb_event) = next else {
            // Client-side transport error: nothing sensible left to do.
            return;
        };
        let event = match convert::client_event(pb_event) {
            Ok(event) => event,
            Err(err) => {
                let malformed = error_event("malformed_event", err.to_string(), true);
                if tx.send(Ok(malformed)).await.is_err() {
                    return;
                }
                continue;
            }
        };
        if !initialized {
            if !matches!(event, ClientEvent::Init { .. }) {
                let not_init = error_event(
                    "init_required",
                    "first message on a stream must be `init` (spec §6.1)".to_owned(),
                    false,
                );
                let _ = tx.send(Ok(not_init)).await;
                return;
            }
            initialized = true;
        }
        // Forward live so chat deltas reach the client while the model is
        // still generating, instead of after the whole turn completes.
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
        let drive = async {
            agent.handle_into(&mut session, event, &etx).await;
            drop(etx);
        };
        let forward = async {
            let mut receiver_alive = true;
            while let Some(server_event) = erx.recv().await {
                if receiver_alive
                    && tx
                        .send(Ok(convert::server_event(server_event)))
                        .await
                        .is_err()
                {
                    receiver_alive = false;
                }
            }
            receiver_alive
        };
        let ((), receiver_alive) = tokio::join!(drive, forward);
        if !receiver_alive {
            return;
        }
    }
}

#[tonic::async_trait]
impl<P: Send + Sync + 'static> pb::agent_service_server::AgentService for AgentStreamService<P> {
    type StreamStream = Pin<Box<dyn Stream<Item = Result<pb::ServerEvent, Status>> + Send>>;

    async fn stream(
        &self,
        request: Request<Streaming<pb::ClientEvent>>,
    ) -> Result<Response<Self::StreamStream>, Status> {
        let principal = self.principals.extract(request.metadata())?;
        let inbound = request.into_inner();
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(run_session(Arc::clone(&self.agent), principal, inbound, tx));
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}
