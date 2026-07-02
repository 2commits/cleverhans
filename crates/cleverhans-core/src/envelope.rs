//! The transport-agnostic envelope (spec §6).
//!
//! These types define message *shapes* only — never actions. `action_id` is a
//! plain string and `params`/`slots` are generic JSON maps, which is what
//! lets the envelope stay stable while the registry evolves freely.

use serde::{Deserialize, Serialize};

use crate::JsonMap;
use crate::proposal::ProposalState;

/// A context snapshot, app → agent (spec §6.2). Context flows one way and is
/// the only channel through which context-sourced params are filled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Context {
    /// Current app route/location.
    pub route: String,
    /// Route or view parameters.
    #[serde(default)]
    pub params: JsonMap,
    /// Current selection, if any.
    #[serde(default)]
    pub selected_record_id: Option<String>,
    /// View kind, e.g. `"detail"` or `"list"`.
    #[serde(default)]
    pub view_type: Option<String>,
    /// App-defined additions.
    #[serde(default)]
    pub extensions: JsonMap,
}

/// Messages from the app frontend to the agent (spec §6.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    /// Opens the session; must be the first message on a stream.
    Init {
        /// Protocol version the client speaks (§13).
        spec_version: String,
        /// Initial context snapshot.
        context: Context,
    },
    /// Replaces the current context snapshot.
    ContextUpdate {
        /// The new snapshot.
        context: Context,
        /// Client-monotonic sequence number; proposals record the snapshot
        /// they were built against.
        context_seq: u64,
    },
    /// A user chat turn.
    UserMessage {
        /// The utterance.
        text: String,
        /// Client-side correlation ID.
        client_msg_id: String,
    },
    /// The user confirms a proposal — triggers confirm-time revalidation and
    /// execution (spec §7.3).
    ConfirmAction {
        /// The proposal being confirmed.
        proposal_id: String,
    },
    /// The user declines a proposal. Terminal for that proposal.
    RejectAction {
        /// The proposal being rejected.
        proposal_id: String,
        /// Optional reason, fed back to the model as conversation context.
        #[serde(default)]
        reason: Option<String>,
    },
}

impl ClientEvent {
    /// The wire tag of this event, for logging and metrics.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Init { .. } => "init",
            Self::ContextUpdate { .. } => "context_update",
            Self::UserMessage { .. } => "user_message",
            Self::ConfirmAction { .. } => "confirm_action",
            Self::RejectAction { .. } => "reject_action",
        }
    }
}

/// Permission-correct preview of what a mutating action would do (spec §6.4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DryRunPreview {
    /// How many records the action would touch.
    pub affected_count: u64,
    /// A bounded sample of affected record identifiers.
    #[serde(default)]
    pub sample_ids: Vec<String>,
    /// Optional human-readable one-liner.
    #[serde(default)]
    pub summary: Option<String>,
    /// App-defined preview payload (e.g. a diff).
    #[serde(default)]
    pub extensions: JsonMap,
}

/// A validated proposal, ready to render (spec §6.4). Immutable once emitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionProposal {
    /// Agent-generated, opaque, unique per session.
    pub proposal_id: String,
    /// A registered action ID.
    pub action_id: String,
    /// Fully filled params: context-sourced + validated utterance-sourced.
    pub params: JsonMap,
    /// A registered block type (normally the action's).
    pub block_type: String,
    /// Typed slot values for that block.
    pub slots: JsonMap,
    /// Required for mutating actions.
    #[serde(default)]
    pub preview: Option<DryRunPreview>,
    /// The context snapshot this proposal was built against.
    pub context_seq: u64,
    /// Correlates to the chat turn that produced it.
    #[serde(default)]
    pub turn_msg_id: Option<String>,
}

/// Messages from the agent to the app frontend (spec §6.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// Assistant prose.
    ChatMessage {
        /// Agent-generated message ID.
        msg_id: String,
        /// Message content.
        text: String,
        /// Marks turn completion (bindings may stream deltas).
        done: bool,
    },
    /// A validated proposal, ready to render.
    ActionProposal(ActionProposal),
    /// Lifecycle transition after emission (spec §7).
    ProposalStateChanged {
        /// The proposal that changed.
        proposal_id: String,
        /// New state.
        state: ProposalState,
        /// Optional explanation (expiry cause, handler error, …).
        #[serde(default)]
        reason: Option<String>,
        /// Handler result for `executed`.
        #[serde(default)]
        result: Option<serde_json::Value>,
    },
    /// Stream- or turn-level errors that are not proposal state changes.
    Error {
        /// Machine-readable code.
        code: String,
        /// Human-readable message.
        message: String,
        /// Whether the session can continue.
        recoverable: bool,
    },
}

impl ServerEvent {
    /// The wire tag of this event, for logging and metrics.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ChatMessage { .. } => "chat_message",
            Self::ActionProposal(_) => "action_proposal",
            Self::ProposalStateChanged { .. } => "proposal_state_changed",
            Self::Error { .. } => "error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod serde_round_trip {
        use super::*;

        #[test]
        fn client_event_is_internally_tagged() {
            let event = ClientEvent::ConfirmAction {
                proposal_id: "prop-1".to_owned(),
            };

            let json = serde_json::to_value(&event).expect("serialize");

            assert_eq!(
                json,
                serde_json::json!({"type": "confirm_action", "proposal_id": "prop-1"})
            );
        }

        #[test]
        fn server_event_proposal_round_trips() {
            let event = ServerEvent::ActionProposal(ActionProposal {
                proposal_id: "prop-1".to_owned(),
                action_id: "transaction.coBuyer.remove".to_owned(),
                params: JsonMap::new(),
                block_type: "confirm".to_owned(),
                slots: JsonMap::new(),
                preview: None,
                context_seq: 7,
                turn_msg_id: None,
            });

            let json = serde_json::to_string(&event).expect("serialize");
            let back: ServerEvent = serde_json::from_str(&json).expect("deserialize");

            assert_eq!(back, event);
        }
    }
}
