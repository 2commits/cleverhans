//! The agent loop: consumes [`ClientEvent`]s, produces [`ServerEvent`]s
//! (spec §6–§7). One [`Session`] per authenticated stream; the [`Agent`]
//! itself is stateless and shared.

use std::sync::Arc;

use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::JsonMap;
use crate::envelope::{ActionProposal, ClientEvent, Context, ServerEvent};
use crate::error::ValidationFailure;
use crate::proposal::{ProposalState, ProposalStore, TrackedProposal};
use crate::registry::Registry;
use crate::seams::{
    AuthzResolver, ChatRole, ChatTurn, CompletionChunk, CompletionRequest, ContextParamResolver,
    LlmProvider,
};
use crate::validation::{CandidateAction, ValidatedAction, Validator};

/// The framework's propose-only ground rules, always the start of the system
/// turn. Every [`crate::seams::LlmProvider`] call is guaranteed to receive a
/// `System` turn as `messages[0]`: this text, plus the app's
/// [`AgentConfig::app_instructions`] when set.
pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are an in-app assistant that can only PROPOSE actions, never perform them. \
Actions you may reference are exactly the provided tools; you cannot invent \
others. A tool call creates a proposal that the user must explicitly confirm \
before the application executes it under the user's own permissions. Never \
claim an action has happened until you are told it executed. Provide only the \
parameters the tool schema asks for; the application supplies everything it \
already knows from context. When the user's intent is ambiguous or matches no \
tool, ask a clarifying question or say what you cannot do instead of guessing.";

/// Loop configuration supplied by the app.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// App-specific instructions appended to [`DEFAULT_SYSTEM_PROMPT`]
    /// (persona, domain vocabulary, tone). The propose-only preamble is not
    /// replaceable — it states the contract the framework enforces anyway.
    pub app_instructions: Option<String>,
    /// How many times a model-fixable validation failure is fed back to the
    /// model for another attempt within one user turn, before declining.
    /// Non-fixable failures (authz denial, app-side errors) never retry.
    pub max_validation_retries: u8,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            app_instructions: None,
            max_validation_retries: 2,
        }
    }
}

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
    config: AgentConfig,
}

impl<P: Send + Sync> Agent<P> {
    /// Assembles an agent over the app's seams with the default
    /// [`AgentConfig`].
    pub fn new(
        registry: Arc<Registry<P>>,
        llm: Arc<dyn LlmProvider>,
        authz: Arc<dyn AuthzResolver<P>>,
        context_params: Arc<dyn ContextParamResolver>,
    ) -> Self {
        Self::with_config(registry, llm, authz, context_params, AgentConfig::default())
    }

    /// Assembles an agent with explicit loop configuration.
    pub fn with_config(
        registry: Arc<Registry<P>>,
        llm: Arc<dyn LlmProvider>,
        authz: Arc<dyn AuthzResolver<P>>,
        context_params: Arc<dyn ContextParamResolver>,
        config: AgentConfig,
    ) -> Self {
        Self {
            registry,
            llm,
            authz,
            context_params,
            config,
        }
    }

    fn system_turn(&self) -> ChatTurn {
        let content = self.config.app_instructions.as_ref().map_or_else(
            || DEFAULT_SYSTEM_PROMPT.to_owned(),
            |extra| format!("{DEFAULT_SYSTEM_PROMPT}\n\n{extra}"),
        );
        ChatTurn {
            role: ChatRole::System,
            content,
        }
    }

    fn completion_request(&self, session: &Session<P>) -> CompletionRequest {
        let mut messages = Vec::with_capacity(session.history.len() + 1);
        messages.push(self.system_turn());
        messages.extend(session.history.iter().cloned());
        CompletionRequest {
            messages,
            tools: self.registry.tool_defs(),
        }
    }

    /// Processes one client event, collecting every emitted server event.
    /// Convenience over [`Agent::handle_into`] for callers that don't need
    /// live streaming.
    pub async fn handle(&self, session: &mut Session<P>, event: ClientEvent) -> Vec<ServerEvent> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.handle_into(session, event, &tx).await;
        drop(tx);
        let mut events = Vec::new();
        while let Ok(server_event) = rx.try_recv() {
            events.push(server_event);
        }
        events
    }

    /// Processes one client event, emitting server events as they become
    /// available — chat text arrives as `done: false` deltas followed by an
    /// authoritative `done: true` message. Infallible by design: failures
    /// surface as [`ServerEvent::Error`] or proposal state changes, never as
    /// a broken stream. Stops early (silently) if the receiver is dropped.
    pub async fn handle_into(
        &self,
        session: &mut Session<P>,
        event: ClientEvent,
        emit: &mpsc::UnboundedSender<ServerEvent>,
    ) {
        let events = match event {
            ClientEvent::Init {
                spec_version,
                context,
            } => Self::handle_init(session, &spec_version, context),
            ClientEvent::ContextUpdate {
                context,
                context_seq,
            } => Self::handle_context_update(session, context, context_seq),
            ClientEvent::UserMessage { text, .. } => {
                return self.handle_user_message(session, text, emit).await;
            }
            ClientEvent::ConfirmAction { proposal_id } => {
                self.handle_confirm(session, &proposal_id).await
            }
            ClientEvent::RejectAction {
                proposal_id,
                reason,
            } => Self::handle_reject(session, &proposal_id, reason),
        };
        for server_event in events {
            if emit.send(server_event).is_err() {
                return;
            }
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
        emit: &mpsc::UnboundedSender<ServerEvent>,
    ) {
        session.history.push(ChatTurn {
            role: ChatRole::User,
            content: text,
        });
        let mut retries_left = self.config.max_validation_retries;
        // Set while a fixable validation failure is awaiting its retry
        // completion: a provider error on that retry declines the failure
        // instead of surfacing a stream error for an optional attempt.
        let mut pending_failure: Option<ValidationFailure> = None;
        loop {
            let mut stream = match self
                .llm
                .complete_stream(self.completion_request(session))
                .await
            {
                Ok(stream) => stream,
                Err(err) => {
                    let event = match pending_failure {
                        Some(failure) => self.decline(session, &failure),
                        None => ServerEvent::Error {
                            code: "llm_error".to_owned(),
                            message: err.to_string(),
                            recoverable: true,
                        },
                    };
                    let _ = emit.send(event);
                    return;
                }
            };

            // One open text segment at a time: deltas share a msg_id, the
            // closing `done: true` message carries the authoritative full
            // text (clients that ignore deltas stay correct).
            let mut segment: Option<(String, String)> = None;
            let mut retry_failure = None;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(CompletionChunk::TextDelta(delta)) => {
                        let (msg_id, buffer) =
                            segment.get_or_insert_with(|| (session.next_msg_id(), String::new()));
                        buffer.push_str(&delta);
                        let event = ServerEvent::ChatMessage {
                            msg_id: msg_id.clone(),
                            text: delta,
                            done: false,
                        };
                        if emit.send(event).is_err() {
                            return;
                        }
                    }
                    Ok(CompletionChunk::TextDone) => {
                        if Self::flush_segment(session, &mut segment, emit).is_err() {
                            return;
                        }
                    }
                    Ok(CompletionChunk::ToolCall { name, arguments }) => {
                        if Self::flush_segment(session, &mut segment, emit).is_err() {
                            return;
                        }
                        match self.propose(session, name, arguments).await {
                            Ok(proposal) => {
                                if emit.send(proposal).is_err() {
                                    return;
                                }
                            }
                            Err(failure) if failure.is_model_fixable() && retries_left > 0 => {
                                // Feed the failure back for another attempt;
                                // the rest of this response is dropped — it
                                // may build on the bad call.
                                retries_left -= 1;
                                retry_failure = Some(failure);
                                break;
                            }
                            Err(failure) => {
                                if emit.send(self.decline(session, &failure)).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Err(err) => {
                        let _ = Self::flush_segment(session, &mut segment, emit);
                        let _ = emit.send(ServerEvent::Error {
                            code: "llm_error".to_owned(),
                            message: err.to_string(),
                            recoverable: true,
                        });
                        return;
                    }
                }
            }
            drop(stream);
            // Providers should close segments with TextDone; flush
            // defensively if the stream ended mid-segment.
            if Self::flush_segment(session, &mut segment, emit).is_err() {
                return;
            }
            let Some(failure) = retry_failure else {
                return;
            };
            // Loop continues into a fresh completion carrying the failure
            // note pushed by `propose`.
            pending_failure = Some(failure);
        }
    }

    /// Closes the open text segment: records the full text in history and
    /// emits the authoritative `done: true` message.
    ///
    /// # Errors
    ///
    /// `Err(())` when the receiver is gone and the caller should stop.
    fn flush_segment(
        session: &mut Session<P>,
        segment: &mut Option<(String, String)>,
        emit: &mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<(), ()> {
        let Some((msg_id, text)) = segment.take() else {
            return Ok(());
        };
        session.history.push(ChatTurn {
            role: ChatRole::Assistant,
            content: text.clone(),
        });
        emit.send(ServerEvent::ChatMessage {
            msg_id,
            text,
            done: true,
        })
        .map_err(|_| ())
    }

    fn decline(&self, session: &mut Session<P>, failure: &ValidationFailure) -> ServerEvent {
        ServerEvent::ChatMessage {
            msg_id: session.next_msg_id(),
            text: format!("I can't propose that action: {failure}."),
            done: true,
        }
    }

    /// Validates one model tool call into an emitted proposal.
    ///
    /// # Errors
    ///
    /// The `proposed -> invalid` edge (spec §7.1): nothing is rendered, the
    /// failure is recorded in history as a `Tool` turn, and the caller
    /// decides between a bounded retry and a conversational decline.
    async fn propose(
        &self,
        session: &mut Session<P>,
        action_id: String,
        arguments: JsonMap,
    ) -> Result<ServerEvent, ValidationFailure> {
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
                session.history.push(ChatTurn {
                    role: ChatRole::Tool,
                    content: format!(
                        "proposal rejected by validation: {failure}. Use only the \
                         provided tools with arguments matching their schema, or \
                         reply in text if the request cannot be served."
                    ),
                });
                return Err(failure);
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
        Ok(ServerEvent::ActionProposal(proposal))
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
