//! Framework-neutral session loop for JSON-frame bindings of the CleverHans
//! envelope (spec §11): each frame is one envelope message in the serde JSON
//! encoding of `cleverhans-core` — no translation layer, the wire shapes
//! *are* the core types.
//!
//! This crate speaks no HTTP. [`FramePump`] is the single per-frame protocol
//! state machine (decode, init-first ordering per spec §6.1, malformed-frame
//! handling); [`run_session`] loops it over a stream of frames for
//! socket-shaped transports. FFI bindings drive the pump one frame at a
//! time instead — either way there is exactly one implementation of the
//! wire behavior.

use std::sync::Arc;

use futures_util::{Stream, StreamExt};
use tokio::sync::mpsc;

use cleverhans_core::agent::{Agent, Session};
use cleverhans_core::envelope::{ClientEvent, ServerEvent};

/// Encodes one outbound envelope event as a JSON text frame.
pub fn to_json(event: &ServerEvent) -> String {
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

/// Where outbound JSON frames go. `emit` returns whether the receiver is
/// still alive; `false` stops emission for the rest of the turn (the turn
/// itself completes). Implemented for bounded/unbounded tokio senders and
/// for plain `FnMut(String) -> bool` closures (synchronous collection).
pub trait EventSink: Send {
    /// Delivers one outbound JSON frame.
    fn emit(&mut self, json: String) -> impl Future<Output = bool> + Send;
}

impl EventSink for mpsc::Sender<String> {
    async fn emit(&mut self, json: String) -> bool {
        self.send(json).await.is_ok()
    }
}

impl EventSink for mpsc::UnboundedSender<String> {
    async fn emit(&mut self, json: String) -> bool {
        self.send(json).is_ok()
    }
}

impl<F> EventSink for F
where
    F: FnMut(String) -> bool + Send,
{
    async fn emit(&mut self, json: String) -> bool {
        self(json)
    }
}

/// What one frame did to the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    /// The session continues.
    Continue,
    /// The session is closed (init-first violation, spec §6.1). The host
    /// should close its transport; further frames emit nothing.
    Closed,
    /// The emit sink reported the receiver gone mid-turn. The turn ran to
    /// completion (side effects included); remaining events were dropped.
    /// Stream transports treat this as session end; per-turn sinks (FFI
    /// bindings) may continue the session with a fresh sink.
    ReceiverGone,
}

/// Per-frame state machine for one envelope session: decodes inbound JSON
/// frames, enforces init-first ordering, drives the agent, and hands each
/// outbound event to `emit` as JSON — live, so chat deltas stream while the
/// model is still generating.
pub struct FramePump<P> {
    session: Session<P>,
    initialized: bool,
    closed: bool,
}

impl<P: Send + Sync> FramePump<P> {
    /// Opens a session for an authenticated principal.
    #[must_use]
    pub fn new(principal: P) -> Self {
        Self {
            session: Session::new(principal),
            initialized: false,
            closed: false,
        }
    }

    /// Whether an init-first violation has closed the session.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Handles one inbound frame, delivering outbound frames to `sink`.
    pub async fn handle_frame(
        &mut self,
        agent: &Agent<P>,
        frame: &str,
        sink: &mut impl EventSink,
    ) -> FrameOutcome {
        if self.closed {
            return FrameOutcome::Closed;
        }
        let event: ClientEvent = match serde_json::from_str(frame) {
            Ok(event) => event,
            Err(err) => {
                tracing::warn!(error = %err, "malformed client frame");
                let alive = sink
                    .emit(error_json("malformed_event", &err.to_string(), true))
                    .await;
                return if alive {
                    FrameOutcome::Continue
                } else {
                    FrameOutcome::ReceiverGone
                };
            }
        };
        tracing::info!(event = event.kind(), "client event");
        if !self.initialized {
            if !matches!(event, ClientEvent::Init { .. }) {
                tracing::warn!(event = event.kind(), "first frame was not init; closing");
                self.closed = true;
                let _ = sink
                    .emit(error_json(
                        "init_required",
                        "first message on a stream must be `init` (spec §6.1)",
                        false,
                    ))
                    .await;
                return FrameOutcome::Closed;
            }
            self.initialized = true;
        }
        // Forward live so chat deltas reach the client while the model is
        // still generating, instead of after the whole turn completes.
        let (etx, mut erx) = mpsc::unbounded_channel();
        let drive = async {
            agent.handle_into(&mut self.session, event, &etx).await;
            drop(etx);
        };
        let forward = async {
            let mut receiver_alive = true;
            while let Some(server_event) = erx.recv().await {
                match &server_event {
                    // Streaming fragments are too chatty for info level.
                    ServerEvent::ChatMessage { done: false, .. } => {
                        tracing::debug!(event = "chat_message(delta)", "server event");
                    }
                    other => tracing::info!(event = other.kind(), "server event"),
                }
                // Keep draining so `drive` never blocks, but stop
                // forwarding — the receiver is gone.
                if receiver_alive && !sink.emit(to_json(&server_event)).await {
                    receiver_alive = false;
                }
            }
            receiver_alive
        };
        let ((), receiver_alive) = tokio::join!(drive, forward);
        if receiver_alive {
            FrameOutcome::Continue
        } else {
            FrameOutcome::ReceiverGone
        }
    }
}

/// Drives one session over a stream of JSON text frames — [`FramePump`] in
/// a loop, for transports where the frame stream and the session share a
/// lifetime (WebSocket, gRPC-style bidi).
pub async fn run_session<P, S>(
    agent: Arc<Agent<P>>,
    principal: P,
    mut inbound: S,
    mut tx: mpsc::Sender<String>,
) where
    P: Send + Sync,
    S: Stream<Item = String> + Unpin + Send,
{
    let session_started = std::time::Instant::now();
    tracing::info!(target: "cleverhans::telemetry::session", phase = "opened", "envelope session opened");
    let mut pump = FramePump::new(principal);
    while let Some(frame) = inbound.next().await {
        match pump.handle_frame(&agent, &frame, &mut tx).await {
            FrameOutcome::Continue => {}
            FrameOutcome::Closed => {
                tracing::info!(
                    target: "cleverhans::telemetry::session",
                    phase = "closed",
                    duration_ms = session_started.elapsed().as_millis() as u64,
                    "envelope session closed"
                );
                return;
            }
            FrameOutcome::ReceiverGone => {
                tracing::info!(
                    target: "cleverhans::telemetry::session",
                    phase = "closed",
                    duration_ms = session_started.elapsed().as_millis() as u64,
                    "client gone; envelope session closed"
                );
                return;
            }
        }
    }
    tracing::info!(
        target: "cleverhans::telemetry::session",
        phase = "closed",
        duration_ms = session_started.elapsed().as_millis() as u64,
        "envelope session closed"
    );
}
