//! The proposal lifecycle state machine (spec §7).
//!
//! `proposed` and `invalid` exist only inside the validation pipeline and are
//! never stored: a proposal enters the [`ProposalStore`] already `validated`,
//! which is also the first state the frontend observes. Runtime states (an
//! enum, not typestate) because proposals are heterogeneous session data whose
//! transitions depend on user input — the legality table is enforced at every
//! transition instead.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::JsonMap;
use crate::envelope::ActionProposal;
use crate::error::TransitionError;

/// Lifecycle states of a proposal (spec §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    /// Model emitted a tool call; envelope constructed (agent-internal).
    Proposed,
    /// Failed propose-time validation; never rendered (terminal).
    Invalid,
    /// Passed propose-time validation; emitted to the frontend.
    Validated,
    /// User confirmed; confirm-time revalidation in progress.
    Confirmed,
    /// Handler ran successfully (terminal).
    Executed,
    /// User declined (terminal).
    Rejected,
    /// Context moved on or revalidation failed (terminal).
    Expired,
    /// Handler returned an error (terminal).
    Failed,
}

impl ProposalState {
    /// Whether no further transition is possible.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Invalid | Self::Executed | Self::Rejected | Self::Expired | Self::Failed
        )
    }

    /// The legality table from spec §7.
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Proposed, Self::Invalid | Self::Validated)
                | (
                    Self::Validated,
                    Self::Confirmed | Self::Rejected | Self::Expired
                )
                | (
                    Self::Confirmed,
                    Self::Executed | Self::Failed | Self::Expired
                )
        )
    }
}

impl fmt::Display for ProposalState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Proposed => "proposed",
            Self::Invalid => "invalid",
            Self::Validated => "validated",
            Self::Confirmed => "confirmed",
            Self::Executed => "executed",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Failed => "failed",
        };
        f.write_str(name)
    }
}

/// A stored proposal plus the session-side bookkeeping the envelope omits.
#[derive(Debug, Clone)]
pub struct TrackedProposal {
    /// The emitted envelope message.
    pub proposal: ActionProposal,
    /// Current lifecycle state.
    pub state: ProposalState,
    /// The model's original utterance-sourced params, kept separately so
    /// confirm-time revalidation can re-run the full pipeline (spec §7.3).
    pub utterance_params: JsonMap,
}

/// Compile-time proof that one proposal crossed `validated -> confirmed`
/// through [`ProposalStore::confirm`]. Deliberately not `Clone` and with no
/// other constructor: holding one means the user confirmed, this session,
/// exactly once.
#[derive(Debug)]
pub struct ConfirmedProposal {
    proposal: ActionProposal,
    utterance_params: JsonMap,
}

impl ConfirmedProposal {
    /// The proposal as it was rendered to the user.
    #[must_use]
    pub fn proposal(&self) -> &ActionProposal {
        &self.proposal
    }

    /// The model's original utterance-sourced params, for confirm-time
    /// revalidation (spec §7.3).
    #[must_use]
    pub fn utterance_params(&self) -> &JsonMap {
        &self.utterance_params
    }
}

/// Per-session store of emitted proposals with enforced transitions.
#[derive(Debug, Default)]
pub struct ProposalStore {
    proposals: BTreeMap<String, TrackedProposal>,
    next_id: u64,
}

impl ProposalStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates the next session-unique proposal ID.
    pub fn allocate_id(&mut self) -> String {
        self.next_id += 1;
        format!("prop-{}", self.next_id)
    }

    /// Tracks a proposal that passed propose-time validation.
    pub fn insert_validated(&mut self, proposal: ActionProposal, utterance_params: JsonMap) {
        self.proposals.insert(
            proposal.proposal_id.clone(),
            TrackedProposal {
                proposal,
                state: ProposalState::Validated,
                utterance_params,
            },
        );
    }

    /// Looks up a tracked proposal.
    #[must_use]
    pub fn get(&self, proposal_id: &str) -> Option<&TrackedProposal> {
        self.proposals.get(proposal_id)
    }

    /// The `validated -> confirmed` edge (spec §7.3) — and the **only**
    /// source of a [`ConfirmedProposal`] witness. The framework's execution
    /// path takes the witness as a parameter, so "execute without a
    /// confirmed proposal" is a compile error, not a review invariant.
    ///
    /// # Errors
    ///
    /// [`TransitionError::UnknownProposal`] if the ID is not tracked;
    /// [`TransitionError::Illegal`] unless the proposal is pending
    /// (`validated`).
    pub fn confirm(&mut self, proposal_id: &str) -> Result<ConfirmedProposal, TransitionError> {
        let tracked = self
            .proposals
            .get_mut(proposal_id)
            .ok_or_else(|| TransitionError::UnknownProposal(proposal_id.to_owned()))?;
        if !tracked.state.can_transition_to(ProposalState::Confirmed) {
            return Err(TransitionError::Illegal {
                from: tracked.state,
                to: ProposalState::Confirmed,
            });
        }
        tracked.state = ProposalState::Confirmed;
        Ok(ConfirmedProposal {
            proposal: tracked.proposal.clone(),
            utterance_params: tracked.utterance_params.clone(),
        })
    }

    /// Applies a transition, enforcing the spec §7 legality table.
    /// Crate-private: the lifecycle is framework-owned — external code
    /// observes states, it never drives them (`confirm` is the one public
    /// mutation, and it returns a witness).
    ///
    /// # Errors
    ///
    /// [`TransitionError::UnknownProposal`] if the ID is not tracked;
    /// [`TransitionError::Illegal`] if the table forbids the move.
    pub(crate) fn transition(
        &mut self,
        proposal_id: &str,
        to: ProposalState,
    ) -> Result<ProposalState, TransitionError> {
        let tracked = self
            .proposals
            .get_mut(proposal_id)
            .ok_or_else(|| TransitionError::UnknownProposal(proposal_id.to_owned()))?;
        if !tracked.state.can_transition_to(to) {
            return Err(TransitionError::Illegal {
                from: tracked.state,
                to,
            });
        }
        tracked.state = to;
        Ok(to)
    }

    /// Expires every pending (`validated`) proposal, returning the affected
    /// IDs so the caller can emit `ProposalStateChanged` for each.
    pub fn expire_pending(&mut self) -> Vec<String> {
        self.proposals
            .values_mut()
            .filter(|tracked| tracked.state == ProposalState::Validated)
            .map(|tracked| {
                tracked.state = ProposalState::Expired;
                tracked.proposal.proposal_id.clone()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(id: &str) -> ActionProposal {
        ActionProposal {
            proposal_id: id.to_owned(),
            action_id: "a.b".to_owned(),
            params: JsonMap::new(),
            block_type: "confirm".to_owned(),
            slots: JsonMap::new(),
            preview: None,
            context_seq: 0,
            turn_msg_id: None,
        }
    }

    mod can_transition_to {
        use super::super::ProposalState::*;

        #[test]
        fn validated_can_be_confirmed() {
            assert!(Validated.can_transition_to(Confirmed));
        }

        #[test]
        fn confirmed_can_execute() {
            assert!(Confirmed.can_transition_to(Executed));
        }

        #[test]
        fn executed_is_terminal() {
            assert!(Executed.is_terminal());
        }

        #[test]
        fn validated_cannot_execute_directly() {
            assert!(!Validated.can_transition_to(Executed));
        }

        #[test]
        fn rejected_cannot_be_confirmed() {
            assert!(!Rejected.can_transition_to(Confirmed));
        }
    }

    mod transition {
        use super::*;

        #[test]
        fn applies_legal_transition() {
            let mut store = ProposalStore::new();
            store.insert_validated(proposal("prop-1"), JsonMap::new());

            let state = store.transition("prop-1", ProposalState::Confirmed);

            assert_eq!(state, Ok(ProposalState::Confirmed));
        }

        #[test]
        fn rejects_illegal_transition() {
            let mut store = ProposalStore::new();
            store.insert_validated(proposal("prop-1"), JsonMap::new());

            let result = store.transition("prop-1", ProposalState::Executed);

            assert_eq!(
                result,
                Err(TransitionError::Illegal {
                    from: ProposalState::Validated,
                    to: ProposalState::Executed,
                })
            );
        }

        #[test]
        fn rejects_unknown_proposal() {
            let mut store = ProposalStore::new();

            let result = store.transition("nope", ProposalState::Confirmed);

            assert_eq!(
                result,
                Err(TransitionError::UnknownProposal("nope".to_owned()))
            );
        }
    }

    mod confirm {
        use super::*;

        #[test]
        fn returns_the_witness_and_marks_the_proposal_confirmed() {
            let mut store = ProposalStore::new();
            store.insert_validated(proposal("prop-1"), JsonMap::new());

            let confirmed = store.confirm("prop-1").expect("pending proposal");

            assert_eq!(confirmed.proposal().proposal_id, "prop-1");
            assert_eq!(
                store.get("prop-1").map(|t| t.state),
                Some(ProposalState::Confirmed)
            );
        }

        #[test]
        fn refuses_a_rejected_proposal() {
            let mut store = ProposalStore::new();
            store.insert_validated(proposal("prop-1"), JsonMap::new());
            store
                .transition("prop-1", ProposalState::Rejected)
                .expect("legal transition");

            let result = store.confirm("prop-1");

            assert!(matches!(
                result,
                Err(TransitionError::Illegal {
                    from: ProposalState::Rejected,
                    to: ProposalState::Confirmed,
                })
            ));
        }

        #[test]
        fn refuses_an_unknown_proposal() {
            let mut store = ProposalStore::new();

            let result = store.confirm("nope");

            assert!(matches!(result, Err(TransitionError::UnknownProposal(id)) if id == "nope"));
        }
    }

    mod expire_pending {
        use super::*;

        #[test]
        fn expires_only_validated_proposals() {
            let mut store = ProposalStore::new();
            store.insert_validated(proposal("prop-1"), JsonMap::new());
            store.insert_validated(proposal("prop-2"), JsonMap::new());
            store
                .transition("prop-2", ProposalState::Rejected)
                .expect("legal transition");

            let expired = store.expire_pending();

            assert_eq!(expired, vec!["prop-1".to_owned()]);
        }
    }
}
