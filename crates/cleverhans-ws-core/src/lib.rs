//! Framework-neutral session loop for JSON-frame bindings of the CleverHans
//! envelope (spec §11): each frame is one envelope message in the serde JSON
//! encoding of `cleverhans-core` — no translation layer, the wire shapes
//! *are* the core types.
//!
//! This crate speaks no HTTP. [`FramePump`] is the single per-frame protocol
//! state machine (decode, unknown message types, init-first ordering per
//! spec §6.1, malformed-frame handling, the end of a session after a
//! non-recoverable error per spec §6.3); [`run_session`] loops it over a
//! stream of frames for socket-shaped transports. FFI bindings drive the
//! pump one frame at a time instead — either way there is exactly one
//! implementation of the wire behavior.
//!
//! The pump never closes a transport itself: it reports *that* the session
//! ended and *why* ([`SessionEnd`]), and the binding closes its connection
//! (the axum adapter sends a WebSocket close frame with
//! [`SessionEnd::close_code`] and [`SessionEnd::close_reason`]).

use std::sync::Arc;

use futures_util::{Stream, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;

use cleverhans_core::agent::{Agent, Session};
use cleverhans_core::envelope::{ClientEvent, ServerEvent};
use cleverhans_core::telemetry::SessionSpan;

/// The WebSocket subprotocol name of this binding (the wire version,
/// spec §13). A server selects it whenever the client offers it; a client
/// that offers subprotocols offers it first. Other offered values (for
/// example a token) are the host's to read from the upgrade request.
pub const SUBPROTOCOL: &str = "cleverhans.v0.1";

/// WebSocket close code a binding sends when the server ends a session
/// after a non-recoverable error: `1008`, policy violation (RFC 6455
/// §7.4.1).
pub const CLOSE_POLICY_VIOLATION: u16 = 1008;

/// What an outbound event that cannot be serialized is replaced with. The
/// envelope types serialize infallibly; this is a defensive fallback.
const ENCODE_FAILED_CODE: &str = "encode_failed";
const ENCODE_FAILED_JSON: &str =
    r#"{"type":"error","code":"encode_failed","message":"","recoverable":false}"#;

/// The longest close reason a WebSocket close frame carries (RFC 6455
/// §5.5: a control frame payload is at most 125 bytes, two of them the
/// code).
const MAX_CLOSE_REASON_BYTES: usize = 123;

/// Encodes one outbound envelope event as a JSON text frame.
pub fn to_json(event: &ServerEvent) -> String {
    serde_json::to_string(event).unwrap_or_else(|_| ENCODE_FAILED_JSON.to_owned())
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

/// Why the server ended a session: it sent an `Error` with
/// `recoverable: false` (spec §6.3), and no further inbound message is
/// processed. The binding closes its connection — a WebSocket binding with
/// a close frame carrying [`Self::close_code`] and [`Self::close_reason`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEnd {
    code: String,
}

impl SessionEnd {
    fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }

    /// The `code` of the non-recoverable `Error` that ended the session
    /// (`init_required`, `unsupported_spec_version`, …).
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The WebSocket close code for this end: always
    /// [`CLOSE_POLICY_VIOLATION`] (`1008`).
    #[must_use]
    pub const fn close_code(&self) -> u16 {
        CLOSE_POLICY_VIOLATION
    }

    /// The WebSocket close reason for this end: the error's `code`,
    /// shortened to the 123 bytes a close frame can carry.
    #[must_use]
    pub fn close_reason(&self) -> &str {
        let mut end = self.code.len().min(MAX_CLOSE_REASON_BYTES);
        while !self.code.is_char_boundary(end) {
            end -= 1;
        }
        &self.code[..end]
    }

    /// The `reason` reported on the `cleverhans::telemetry::session`
    /// `closed` event (a static string, as the telemetry contract needs).
    fn telemetry_reason(&self) -> &'static str {
        match self.code.as_str() {
            "init_required" => "init_required",
            "unsupported_spec_version" => "unsupported_spec_version",
            ENCODE_FAILED_CODE => ENCODE_FAILED_CODE,
            _ => "unrecoverable_error",
        }
    }
}

/// What one frame did to the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameOutcome {
    /// The session continues.
    Continue,
    /// The session is over: the server sent an `Error` with
    /// `recoverable: false` (spec §6.3) — on this frame or an earlier one.
    /// The host should close its transport (a WebSocket host with
    /// [`SessionEnd::close_code`] and [`SessionEnd::close_reason`]);
    /// further frames emit nothing.
    Closed(SessionEnd),
    /// The emit sink reported the receiver gone mid-turn. The turn ran to
    /// completion (side effects included); remaining events were dropped.
    /// Stream transports treat this as session end; per-turn sinks (FFI
    /// bindings) may continue the session with a fresh sink.
    ReceiverGone,
}

/// How a frame decodes (spec §6, §13).
enum Decoded {
    /// A message this binding knows.
    Event(ClientEvent),
    /// A JSON object whose `type` is a string this binding does not know:
    /// ignored without an answer (spec §13).
    Unknown(String),
    /// Anything else that fails to decode: answered with `malformed_event`.
    Malformed(String),
}

fn decode(frame: &str) -> Decoded {
    let value: Value = match serde_json::from_str(frame) {
        Ok(value) => value,
        Err(err) => return Decoded::Malformed(err.to_string()),
    };
    let Some(object) = value.as_object() else {
        return Decoded::Malformed("a client message must be a JSON object".to_owned());
    };
    let kind = match object.get("type") {
        Some(Value::String(kind)) => kind,
        Some(_) => return Decoded::Malformed("`type` must be a string".to_owned()),
        None => return Decoded::Malformed("missing field `type`".to_owned()),
    };
    if !ClientEvent::KINDS.contains(&kind.as_str()) {
        return Decoded::Unknown(kind.clone());
    }
    match serde_json::from_value(value) {
        Ok(event) => Decoded::Event(event),
        Err(err) => Decoded::Malformed(err.to_string()),
    }
}

/// Per-frame state machine for one envelope session: decodes inbound JSON
/// frames, ignores message types it does not know, enforces init-first
/// ordering, drives the agent, hands each outbound event to `emit` as JSON —
/// live, so chat deltas stream while the model is still generating — and
/// ends the session after a non-recoverable error.
pub struct FramePump<P> {
    session: Session<P>,
    initialized: bool,
    end: Option<SessionEnd>,
}

impl<P: Send + Sync> FramePump<P> {
    /// Opens a session for an authenticated principal.
    #[must_use]
    pub fn new(principal: P) -> Self {
        Self {
            session: Session::new(principal),
            initialized: false,
            end: None,
        }
    }

    /// Whether the session is over (the server sent a non-recoverable
    /// `Error`).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.end.is_some()
    }

    /// Why the session ended, once it has.
    #[must_use]
    pub fn end(&self) -> Option<&SessionEnd> {
        self.end.as_ref()
    }

    /// Records the end of the session and reports it.
    fn close(&mut self, end: SessionEnd) -> FrameOutcome {
        self.end = Some(end.clone());
        FrameOutcome::Closed(end)
    }

    /// Handles one inbound frame, delivering outbound frames to `sink`.
    pub async fn handle_frame(
        &mut self,
        agent: &Agent<P>,
        frame: &str,
        sink: &mut impl EventSink,
    ) -> FrameOutcome {
        if let Some(end) = &self.end {
            return FrameOutcome::Closed(end.clone());
        }
        let event = match decode(frame) {
            Decoded::Event(event) => event,
            Decoded::Unknown(kind) => {
                // Spec §13: the envelope grows by new message types, so a
                // receiver ignores one it does not know. Not an answer, not
                // the first message.
                tracing::debug!(event = kind.as_str(), "ignored unknown client event type");
                return FrameOutcome::Continue;
            }
            Decoded::Malformed(message) => {
                tracing::warn!(error = message.as_str(), "malformed client frame");
                let alive = sink
                    .emit(error_json("malformed_event", &message, true))
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
                let _ = sink
                    .emit(error_json(
                        "init_required",
                        "first message on a stream must be `init` (spec §6.1)",
                        false,
                    ))
                    .await;
                return self.close(SessionEnd::new("init_required"));
            }
            self.initialized = true;
        }
        // Forward live so chat deltas reach the client while the model is
        // still generating, instead of after the whole turn completes.
        let (etx, mut erx) = mpsc::unbounded_channel();
        let session = &mut self.session;
        let drive = async {
            agent.handle_into(session, event, &etx).await;
            drop(etx);
        };
        let forward = async {
            let mut receiver_alive = true;
            let mut end: Option<SessionEnd> = None;
            while let Some(server_event) = erx.recv().await {
                // Keep draining so `drive` never blocks, but forward
                // nothing after the receiver is gone or the session ended.
                if !receiver_alive || end.is_some() {
                    continue;
                }
                match &server_event {
                    // Streaming fragments are too chatty for info level.
                    ServerEvent::ChatMessage { done: false, .. } => {
                        tracing::debug!(event = "chat_message(delta)", "server event");
                    }
                    other => tracing::info!(event = other.kind(), "server event"),
                }
                let json = match serde_json::to_string(&server_event) {
                    Ok(json) => {
                        // Spec §6.3: a non-recoverable error ends the
                        // session; it is the last event the client gets.
                        if let ServerEvent::Error {
                            code,
                            recoverable: false,
                            ..
                        } = &server_event
                        {
                            tracing::warn!(code = code.as_str(), "non-recoverable error; closing");
                            end = Some(SessionEnd::new(code.clone()));
                        }
                        json
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "outbound event failed to encode; closing");
                        end = Some(SessionEnd::new(ENCODE_FAILED_CODE));
                        ENCODE_FAILED_JSON.to_owned()
                    }
                };
                if !sink.emit(json).await {
                    receiver_alive = false;
                }
            }
            (receiver_alive, end)
        };
        let ((), (receiver_alive, end)) = tokio::join!(drive, forward);
        if let Some(end) = end {
            return self.close(end);
        }
        if receiver_alive {
            FrameOutcome::Continue
        } else {
            FrameOutcome::ReceiverGone
        }
    }
}

/// How [`run_session`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionExit {
    /// The server ended the session after a non-recoverable error. The
    /// transport should close the connection — a WebSocket with a close
    /// frame carrying [`SessionEnd::close_code`] and
    /// [`SessionEnd::close_reason`].
    Ended(SessionEnd),
    /// The inbound stream ended: the client closed the connection, or the
    /// transport failed.
    InboundClosed,
    /// The outbound receiver is gone; the connection can no longer be
    /// written to.
    ReceiverGone,
}

/// Drives one session over a stream of JSON text frames — [`FramePump`] in
/// a loop, for transports where the frame stream and the session share a
/// lifetime (WebSocket, gRPC-style bidi). Returns why the session ended, so
/// the transport can close its connection accordingly.
pub async fn run_session<P, S>(
    agent: Arc<Agent<P>>,
    principal: P,
    mut inbound: S,
    mut tx: mpsc::Sender<String>,
) -> SessionExit
where
    P: Send + Sync,
    S: Stream<Item = String> + Unpin + Send,
{
    // RAII: the paired `closed` event fires on every exit, including a
    // dropped task, so the active-session gauge cannot drift upward.
    let mut span = SessionSpan::open();
    let mut pump = FramePump::new(principal);
    while let Some(frame) = inbound.next().await {
        match pump.handle_frame(&agent, &frame, &mut tx).await {
            FrameOutcome::Continue => {}
            FrameOutcome::Closed(end) => {
                span.set_reason(end.telemetry_reason());
                return SessionExit::Ended(end);
            }
            FrameOutcome::ReceiverGone => {
                span.set_reason("client_gone");
                return SessionExit::ReceiverGone;
            }
        }
    }
    SessionExit::InboundClosed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_reason_is_the_error_code() {
        let end = SessionEnd::new("init_required");

        assert_eq!(
            (end.close_code(), end.close_reason()),
            (1008, "init_required")
        );
    }

    #[test]
    fn close_reason_fits_a_close_frame_on_a_char_boundary() {
        let end = SessionEnd::new("é".repeat(100));

        let reason = end.close_reason();

        assert!(
            reason.len() <= MAX_CLOSE_REASON_BYTES,
            "{} bytes",
            reason.len()
        );
        assert_eq!(reason, "é".repeat(61));
    }

    #[test]
    fn decode_tells_unknown_types_from_malformed_frames() {
        let kinds = [
            r#"{"type":"frobnicate"}"#,
            r#"{"type":"frobnicate","context_seq":"x"}"#,
            "not json",
            "[]",
            r#"{"text":"hi"}"#,
            r#"{"type":7}"#,
            r#"{"type":"user_message"}"#,
            r#"{"type":"context_update","context":{"route":"/"},"context_seq":"1"}"#,
            r#"{"type":"user_message","text":"hi","client_msg_id":"c-1"}"#,
        ]
        .map(|frame| match decode(frame) {
            Decoded::Event(_) => "event",
            Decoded::Unknown(_) => "unknown",
            Decoded::Malformed(_) => "malformed",
        });

        assert_eq!(
            kinds,
            [
                "unknown",
                "unknown",
                "malformed",
                "malformed",
                "malformed",
                "malformed",
                "malformed",
                "malformed",
                "event",
            ]
        );
    }
}
