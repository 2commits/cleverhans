//! Error types for registry construction, validation, execution, and the
//! proposal state machine.

use crate::proposal::ProposalState;

/// Errors raised while building a [`crate::registry::Registry`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    /// Two actions were registered under the same ID.
    #[error("duplicate action id `{0}`")]
    DuplicateAction(String),
    /// Two block types were registered under the same name.
    #[error("duplicate block type `{0}`")]
    DuplicateBlock(String),
    /// An action references a block type that was never registered.
    #[error("action `{action_id}` references unregistered block type `{block_type}`")]
    UnknownBlockType {
        /// The offending action.
        action_id: String,
        /// The unregistered block type it referenced.
        block_type: String,
    },
    /// A mutating action was registered without a dry-run handler (spec §4).
    #[error("mutating action `{0}` must register a dry-run handler")]
    MissingDryRun(String),
    /// A non-mutating action registered a dry-run handler, which validation
    /// would never invoke — almost certainly a mismatch between the action
    /// definition and the handler wiring.
    #[error("action `{0}` does not mutate; its dry-run handler would never run")]
    UnexpectedDryRun(String),
    /// A schema-declared action was never given handlers via
    /// [`crate::registry::RegistryBuilder::attach`].
    #[error("action `{0}` from schema has no handler attached")]
    UnattachedAction(String),
    /// An `attach` call named an action the schema does not declare.
    #[error("attach for `{0}` matches no schema action")]
    UnknownAttachment(String),
    /// The same action was attached more than once.
    #[error("action `{0}` attached twice")]
    DuplicateAttachment(String),
    /// A [`crate::registry::RegistryBuilder::bind`] never set a handler.
    #[error("binding for `{0}` sets no handler")]
    MissingHandler(String),
    /// A binding set both [`crate::seams::SlotBuilder`] and
    /// [`crate::seams::AsyncSlotBuilder`]; an action has exactly one slot
    /// source.
    #[error("binding for `{0}` sets both slots and async_slots")]
    ConflictingSlotBuilders(String),
}

/// A propose-time or confirm-time validation failure (spec §7.1).
///
/// A candidate that fails validation becomes `invalid` (propose time) or
/// `expired` (confirm time) and is never rendered / executed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationFailure {
    /// The model referenced an action ID that is not registered.
    #[error("unknown action `{0}`")]
    UnknownAction(String),
    /// A parameter value failed its type check.
    #[error("param `{param}`: {reason}")]
    InvalidParam {
        /// Parameter name.
        param: String,
        /// Human-readable type-check failure.
        reason: String,
    },
    /// A required parameter was not provided.
    #[error("missing required param `{0}`")]
    MissingParam(String),
    /// The model emitted a parameter the action does not declare as
    /// utterance-sourced — including attempts to set context-sourced params.
    #[error("unknown or non-utterance param `{0}`")]
    UnknownParam(String),
    /// A required context-sourced parameter could not be resolved from the
    /// current context snapshot.
    #[error("context param `{0}` could not be resolved from current context")]
    UnresolvedContextParam(String),
    /// The authorization seam denied the action for this principal.
    #[error("not authorized for `{action_id}`: {reason}")]
    Unauthorized {
        /// The denied action.
        action_id: String,
        /// Reason supplied by the app's authz resolver.
        reason: String,
    },
    /// The mandate seam denied the action (spec §9.8): the principal may
    /// well be authorized, but the user never delegated this action to the
    /// agent.
    #[error("outside the agent's mandate for `{action_id}`: {reason}")]
    OutOfMandate {
        /// The denied action.
        action_id: String,
        /// Reason supplied by the app's mandate.
        reason: String,
    },
    /// A slot value failed the block type's slot schema.
    #[error("slot `{slot}`: {reason}")]
    InvalidSlot {
        /// Slot name.
        slot: String,
        /// Human-readable schema failure.
        reason: String,
    },
    /// The dry-run handler failed; a mutating proposal without a
    /// permission-correct preview must not be rendered.
    #[error("dry-run failed: {0}")]
    DryRun(String),
    /// The async slot builder failed (§14.9 fail-closed): a card whose
    /// content could not be built must not be rendered.
    #[error("slot build failed: {0}")]
    SlotBuild(String),
}

impl ValidationFailure {
    /// Whether a different model output could fix this failure.
    ///
    /// Selection and argument mistakes are worth a bounded retry; denials
    /// and app-side failures (authz, mandate, unresolvable context, dry-run,
    /// slots) are not — no rephrasing makes an unauthorized action
    /// authorized or widens the agent's mandate.
    #[must_use]
    pub fn is_model_fixable(&self) -> bool {
        matches!(
            self,
            Self::UnknownAction(_)
                | Self::InvalidParam { .. }
                | Self::MissingParam(_)
                | Self::UnknownParam(_)
        )
    }
}

/// Errors returned by app-side handlers ([`crate::seams::ActionHandler`],
/// [`crate::seams::DryRunHandler`]).
#[derive(Debug, thiserror::Error)]
pub enum HandlerError {
    /// The app declined the operation (business rule, conflict, …).
    #[error("rejected: {0}")]
    Rejected(String),
    /// The app failed internally.
    #[error("internal: {0}")]
    Internal(String),
}

/// Errors returned by an [`crate::seams::LlmProvider`].
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// The provider call failed (network, auth, quota, malformed reply, …).
    #[error("llm provider error: {0}")]
    Provider(String),
}

/// Illegal operations on the proposal state machine.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitionError {
    /// No proposal with this ID is tracked.
    #[error("unknown proposal `{0}`")]
    UnknownProposal(String),
    /// The requested transition is not in the lifecycle table (spec §7).
    #[error("illegal transition {from} -> {to}")]
    Illegal {
        /// Current state.
        from: ProposalState,
        /// Requested state.
        to: ProposalState,
    },
}
