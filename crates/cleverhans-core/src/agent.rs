//! The agent loop: consumes [`ClientEvent`]s, produces [`ServerEvent`]s
//! (spec §6–§7). One [`Session`] per authenticated stream; the [`Agent`]
//! itself is stateless and shared.

use std::sync::Arc;

use crate::JsonMap;
use crate::envelope::{ActionProposal, ClientEvent, Context, ServerEvent};
use crate::proposal::{ProposalState, ProposalStore, TrackedProposal};
use crate::registry::Registry;
use crate::seams::{
    AuthzResolver, ChatRole, ChatTurn, CompletionItem, CompletionRequest, ContextParamResolver,
    LlmProvider,
};
use crate::validation::{CandidateAction, ValidatedAction, Validator};

/// Per-stream state, bound to one principal for its whole lifetime.
pub struct Session<P> {
    /// The authenticated principal; the framework never constructs one.
    pub principal: P,
    context: Context,
    context_seq: u64,
    proposals: ProposalStore,
    history: Vec<ChatTurn>,
    msg_counter: u64,
}

impl<P> Session<P> {
    /// Opens a session for an authenticated principal.
    pub fn new(principal: P) -> Self {
        Self {
            principal,
            context: Context::default(),
            context_seq: 0,
            proposals: ProposalStore::new(),
            history: Vec::new(),
            msg_counter: 0,
        }
    }

    fn next_msg_id(&mut self) -> String {
        self.msg_counter += 1;
        format!("msg-{}", self.msg_counter)
    }
}

/// The framework's agent: registry + seams, no credentials, no state.
pub struct Agent<P> {
    registry: Arc<Registry<P>>,
    llm: Arc<dyn LlmProvider>,
    authz: Arc<dyn AuthzResolver<P>>,
    context_params: Arc<dyn ContextParamResolver>,
}

impl<P: Send + Sync> Agent<P> {
    /// Assembles an agent over the app's seams.
    pub fn new(
        registry: Arc<Registry<P>>,
        llm: Arc<dyn LlmProvider>,
        authz: Arc<dyn AuthzResolver<P>>,
        context_params: Arc<dyn ContextParamResolver>,
    ) -> Self {
        Self {
            registry,
            llm,
            authz,
            context_params,
        }
    }

    /// Processes one client event. Infallible by design: failures surface as
    /// [`ServerEvent::Error`] or proposal state changes, never as a broken
    /// stream.
    pub async fn handle(&self, session: &mut Session<P>, event: ClientEvent) -> Vec<ServerEvent> {
        match event {
            ClientEvent::Init {
                spec_version,
                context,
            } => Self::handle_init(session, &spec_version, context),
            ClientEvent::ContextUpdate {
                context,
                context_seq,
            } => Self::handle_context_update(session, context, context_seq),
            ClientEvent::UserMessage { text, .. } => self.handle_user_message(session, text).await,
            ClientEvent::ConfirmAction { proposal_id } => {
                self.handle_confirm(session, &proposal_id).await
            }
            ClientEvent::RejectAction {
                proposal_id,
                reason,
            } => Self::handle_reject(session, &proposal_id, reason),
        }
    }

    fn handle_init(
        session: &mut Session<P>,
        spec_version: &str,
        context: Context,
    ) -> Vec<ServerEvent> {
        if !spec_version.starts_with(crate::SPEC_VERSION) {
            return vec![ServerEvent::Error {
                code: "unsupported_spec_version".to_owned(),
                message: format!(
                    "client speaks `{spec_version}`, server implements `{}`",
                    crate::SPEC_VERSION
                ),
                recoverable: false,
            }];
        }
        session.context = context;
        session.context_seq = 0;
        Vec::new()
    }

    fn handle_context_update(
        session: &mut Session<P>,
        context: Context,
        context_seq: u64,
    ) -> Vec<ServerEvent> {
        // Route changes invalidate pending proposals (spec §7.4 SHOULD);
        // confirm-time revalidation remains the safety backstop either way.
        let route_changed = session.context.route != context.route;
        session.context = context;
        session.context_seq = context_seq;
        if !route_changed {
            return Vec::new();
        }
        session
            .proposals
            .expire_pending()
            .into_iter()
            .map(|proposal_id| ServerEvent::ProposalStateChanged {
                proposal_id,
                state: ProposalState::Expired,
                reason: Some("context changed".to_owned()),
                result: None,
            })
            .collect()
    }

    async fn handle_user_message(
        &self,
        session: &mut Session<P>,
        text: String,
    ) -> Vec<ServerEvent> {
        session.history.push(ChatTurn {
            role: ChatRole::User,
            content: text,
        });
        let request = CompletionRequest {
            messages: session.history.clone(),
            tools: self.registry.tool_defs(),
        };
        let items = match self.llm.complete(request).await {
            Ok(items) => items,
            Err(err) => {
                return vec![ServerEvent::Error {
                    code: "llm_error".to_owned(),
                    message: err.to_string(),
                    recoverable: true,
                }];
            }
        };

        let mut events = Vec::new();
        for item in items {
            match item {
                CompletionItem::Text(content) => {
                    session.history.push(ChatTurn {
                        role: ChatRole::Assistant,
                        content: content.clone(),
                    });
                    events.push(ServerEvent::ChatMessage {
                        msg_id: session.next_msg_id(),
                        text: content,
                        done: true,
                    });
                }
                CompletionItem::ToolCall { name, arguments } => {
                    events.push(self.propose(session, name, arguments).await);
                }
            }
        }
        events
    }

    async fn propose(
        &self,
        session: &mut Session<P>,
        action_id: String,
        arguments: JsonMap,
    ) -> ServerEvent {
        let candidate = CandidateAction {
            action_id,
            utterance_params: arguments,
        };
        let validated = match self
            .validator()
            .validate(&candidate, &session.context, &session.principal)
            .await
        {
            Ok(validated) => validated,
            Err(failure) => {
                // `proposed -> invalid`: never rendered; decline
                // conversationally instead (spec §7.1).
                let text = format!("I can't propose that action: {failure}.");
                session.history.push(ChatTurn {
                    role: ChatRole::Tool,
                    content: format!("proposal rejected by validation: {failure}"),
                });
                return ServerEvent::ChatMessage {
                    msg_id: session.next_msg_id(),
                    text,
                    done: true,
                };
            }
        };

        let ValidatedAction {
            action_id,
            params,
            block_type,
            slots,
            preview,
        } = validated;
        let proposal_id = session.proposals.allocate_id();
        let proposal = ActionProposal {
            proposal_id: proposal_id.clone(),
            action_id: action_id.clone(),
            params,
            block_type,
            slots,
            preview,
            context_seq: session.context_seq,
            turn_msg_id: None,
        };
        session
            .proposals
            .insert_validated(proposal.clone(), candidate.utterance_params);
        session.history.push(ChatTurn {
            role: ChatRole::Tool,
            content: format!("proposed `{action_id}` as `{proposal_id}`, awaiting user decision"),
        });
        ServerEvent::ActionProposal(proposal)
    }

    async fn handle_confirm(
        &self,
        session: &mut Session<P>,
        proposal_id: &str,
    ) -> Vec<ServerEvent> {
        let Some(tracked) = session.proposals.get(proposal_id) else {
            return vec![ServerEvent::Error {
                code: "unknown_proposal".to_owned(),
                message: format!("no proposal `{proposal_id}` in this session"),
                recoverable: true,
            }];
        };
        // A confirm for an already-terminal proposal reports the actual
        // state and executes nothing (spec §7.3).
        if tracked.state != ProposalState::Validated {
            return vec![ServerEvent::ProposalStateChanged {
                proposal_id: proposal_id.to_owned(),
                state: tracked.state,
                reason: Some("proposal is not pending confirmation".to_owned()),
                result: None,
            }];
        }
        let TrackedProposal {
            proposal,
            utterance_params,
            ..
        } = tracked.clone();
        if session
            .proposals
            .transition(proposal_id, ProposalState::Confirmed)
            .is_err()
        {
            // Guarded by the state check above; fail closed if racing.
            return Vec::new();
        }

        // Confirm-time revalidation against *current* state (spec §7.3).
        let candidate = CandidateAction {
            action_id: proposal.action_id.clone(),
            utterance_params,
        };
        let revalidated = self
            .validator()
            .validate(&candidate, &session.context, &session.principal)
            .await;
        let expired = |reason: String, session: &mut Session<P>| {
            let _ = session
                .proposals
                .transition(proposal_id, ProposalState::Expired);
            vec![ServerEvent::ProposalStateChanged {
                proposal_id: proposal_id.to_owned(),
                state: ProposalState::Expired,
                reason: Some(reason),
                result: None,
            }]
        };
        match revalidated {
            Err(failure) => return expired(failure.to_string(), session),
            // Context drift: params no longer resolve to what the user saw.
            Ok(now) if now.params != proposal.params => {
                return expired("context changed since proposal".to_owned(), session);
            }
            Ok(_) => {}
        }

        let Some(registration) = self.registry.action(&proposal.action_id) else {
            return expired("action no longer registered".to_owned(), session);
        };
        let outcome = registration
            .handler
            .execute(&proposal.params, &session.principal)
            .await;
        let (state, reason, result) = match outcome {
            Ok(value) => (ProposalState::Executed, None, Some(value)),
            Err(err) => (ProposalState::Failed, Some(err.to_string()), None),
        };
        let _ = session.proposals.transition(proposal_id, state);
        session.history.push(ChatTurn {
            role: ChatRole::Tool,
            content: format!(
                "`{}` ({proposal_id}) {state}{}",
                proposal.action_id,
                reason
                    .as_ref()
                    .map_or_else(String::new, |r| format!(": {r}"))
            ),
        });
        vec![ServerEvent::ProposalStateChanged {
            proposal_id: proposal_id.to_owned(),
            state,
            reason,
            result,
        }]
    }

    fn handle_reject(
        session: &mut Session<P>,
        proposal_id: &str,
        reason: Option<String>,
    ) -> Vec<ServerEvent> {
        match session
            .proposals
            .transition(proposal_id, ProposalState::Rejected)
        {
            Ok(state) => {
                session.history.push(ChatTurn {
                    role: ChatRole::Tool,
                    content: format!(
                        "user rejected `{proposal_id}`{}",
                        reason
                            .as_ref()
                            .map_or_else(String::new, |r| format!(": {r}"))
                    ),
                });
                vec![ServerEvent::ProposalStateChanged {
                    proposal_id: proposal_id.to_owned(),
                    state,
                    reason,
                    result: None,
                }]
            }
            Err(err) => vec![ServerEvent::Error {
                code: "invalid_reject".to_owned(),
                message: err.to_string(),
                recoverable: true,
            }],
        }
    }

    fn validator(&self) -> Validator<'_, P> {
        Validator::new(
            &self.registry,
            self.authz.as_ref(),
            self.context_params.as_ref(),
        )
    }
}
