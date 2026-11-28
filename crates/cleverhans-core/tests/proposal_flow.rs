//! End-to-end exercise of the spec Appendix A flow: context update → user
//! message → validated proposal with preview → confirm → execution under the
//! principal, plus the decline/reject/expiry branches.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use cleverhans_core::JsonMap;
use cleverhans_core::agent::{Agent, Session};
use cleverhans_core::envelope::{ClientEvent, Context, DryRunPreview, ServerEvent};
use cleverhans_core::error::{HandlerError, LlmError};
use cleverhans_core::proposal::ProposalState;
use cleverhans_core::registry::{
    ActionDef, BlockDef, ParamSource, ParamSpec, Registry, SlotSpec, ValueType,
};
use cleverhans_core::seams::{
    ActionHandler, AuthzDecision, AuthzResolver, CompletionItem, CompletionRequest,
    ContextParamResolver, DryRunHandler, LlmProvider, Mandate, MandateDecision, SlotBuilder,
};

#[derive(Clone)]
struct User {
    id: &'static str,
    can_remove_co_buyer: bool,
}

struct ScriptedLlm {
    responses: Mutex<VecDeque<Vec<CompletionItem>>>,
}

impl ScriptedLlm {
    fn returning(items: Vec<CompletionItem>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from([items])),
        }
    }
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn complete(&self, _request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        self.responses
            .lock()
            .expect("lock")
            .pop_front()
            .ok_or_else(|| LlmError::Provider("script exhausted".to_owned()))
    }
}

struct RecordingHandler {
    executions: Arc<Mutex<Vec<(String, JsonMap)>>>,
}

#[async_trait]
impl ActionHandler<User> for RecordingHandler {
    async fn execute(
        &self,
        params: &JsonMap,
        principal: &User,
    ) -> Result<serde_json::Value, HandlerError> {
        self.executions
            .lock()
            .expect("lock")
            .push((principal.id.to_owned(), params.clone()));
        Ok(json!({"removed": true}))
    }
}

struct CoBuyerDryRun;

#[async_trait]
impl DryRunHandler<User> for CoBuyerDryRun {
    async fn dry_run(
        &self,
        _params: &JsonMap,
        _principal: &User,
    ) -> Result<DryRunPreview, HandlerError> {
        Ok(DryRunPreview {
            affected_count: 1,
            sample_ids: vec!["cb_112".to_owned()],
            summary: Some("Remove co-buyer Jane Doe from TX-581".to_owned()),
            extensions: JsonMap::new(),
        })
    }
}

struct ConfirmSlots;

impl SlotBuilder for ConfirmSlots {
    fn build(&self, _params: &JsonMap, preview: Option<&DryRunPreview>) -> JsonMap {
        let mut slots = JsonMap::new();
        slots.insert("title".to_owned(), json!("Remove co-buyer"));
        if let Some(summary) = preview.and_then(|p| p.summary.as_deref()) {
            slots.insert("detail".to_owned(), json!(summary));
        }
        slots
    }
}

struct PermissionAuthz;

#[async_trait]
impl AuthzResolver<User> for PermissionAuthz {
    async fn authorize(
        &self,
        principal: &User,
        _action_id: &str,
        _params: &JsonMap,
    ) -> AuthzDecision {
        if principal.can_remove_co_buyer {
            AuthzDecision::Allow
        } else {
            AuthzDecision::Deny("cannot remove co-buyers".to_owned())
        }
    }
}

struct RouteResolver;

impl ContextParamResolver for RouteResolver {
    fn resolve(
        &self,
        _action_id: &str,
        param: &ParamSpec,
        context: &Context,
    ) -> Option<serde_json::Value> {
        (param.name == "transactionId")
            .then(|| context.selected_record_id.clone().map(Into::into))
            .flatten()
    }
}

fn registry(executions: Arc<Mutex<Vec<(String, JsonMap)>>>) -> Registry<User> {
    Registry::builder()
        .block(BlockDef {
            block_type: "confirm".to_owned(),
            slots: vec![
                SlotSpec {
                    name: "title".to_owned(),
                    ty: ValueType::String,
                    required: true,
                },
                SlotSpec {
                    name: "detail".to_owned(),
                    ty: ValueType::String,
                    required: false,
                },
            ],
        })
        .action(
            ActionDef {
                id: "transaction.coBuyer.remove".to_owned(),
                description: "Remove the co-buyer from the current transaction".to_owned(),
                params: vec![ParamSpec {
                    name: "transactionId".to_owned(),
                    description: String::new(),
                    ty: ValueType::String,
                    source: ParamSource::Context,
                    required: true,
                }],
                block_type: "confirm".to_owned(),
                mutates: true,
                authz_key: "transaction.coBuyer.remove".to_owned(),
                display: None,
            },
            Arc::new(RecordingHandler { executions }),
            Some(Arc::new(CoBuyerDryRun)),
            Some(Arc::new(ConfirmSlots)),
        )
        .build()
        .expect("valid registry")
}

fn agent_with(llm: ScriptedLlm, executions: Arc<Mutex<Vec<(String, JsonMap)>>>) -> Agent<User> {
    // Every agent in this binary is built here, so the telemetry collector
    // is in place before any test can emit (see `telemetry::install`).
    telemetry::install();
    Agent::new(
        Arc::new(registry(executions)),
        Arc::new(llm),
        Arc::new(PermissionAuthz),
        Arc::new(RouteResolver),
    )
}

fn tool_call() -> Vec<CompletionItem> {
    vec![CompletionItem::ToolCall {
        name: "transaction.coBuyer.remove".to_owned(),
        arguments: JsonMap::new(),
    }]
}

fn tx_context() -> Context {
    Context {
        route: "/transactions/tx_581".to_owned(),
        selected_record_id: Some("tx_581".to_owned()),
        view_type: Some("detail".to_owned()),
        ..Context::default()
    }
}

async fn open_session(agent: &Agent<User>, session: &mut Session<User>) {
    let events = agent
        .handle(
            session,
            ClientEvent::Init {
                spec_version: "0.1.0-draft".to_owned(),
                context: tx_context(),
            },
        )
        .await;
    assert!(events.is_empty(), "init should be silent: {events:?}");
}

fn user_message(text: &str) -> ClientEvent {
    ClientEvent::UserMessage {
        text: text.to_owned(),
        client_msg_id: "c-1".to_owned(),
    }
}

async fn propose(agent: &Agent<User>, session: &mut Session<User>) -> String {
    let events = agent
        .handle(session, user_message("remove the co-buyer"))
        .await;
    match &events[..] {
        [ServerEvent::ActionProposal(p)] => p.proposal_id.clone(),
        other => panic!("expected one proposal, got {other:?}"),
    }
}

#[tokio::test]
async fn user_message_yields_validated_proposal_with_preview() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let agent = agent_with(ScriptedLlm::returning(tool_call()), Arc::clone(&executions));
    let mut session = Session::new(User {
        id: "alex",
        can_remove_co_buyer: true,
    });
    open_session(&agent, &mut session).await;

    let events = agent
        .handle(&mut session, user_message("remove the co-buyer"))
        .await;

    let [ServerEvent::ActionProposal(proposal)] = &events[..] else {
        panic!("expected one proposal, got {events:?}");
    };
    assert_eq!(
        (
            proposal.action_id.as_str(),
            proposal.params["transactionId"].as_str(),
            proposal.block_type.as_str(),
            proposal.slots["title"].as_str(),
            proposal.preview.as_ref().map(|p| p.affected_count),
        ),
        (
            "transaction.coBuyer.remove",
            Some("tx_581"),
            "confirm",
            Some("Remove co-buyer"),
            Some(1),
        )
    );
}

#[tokio::test]
async fn confirm_executes_handler_under_principal() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let agent = agent_with(ScriptedLlm::returning(tool_call()), Arc::clone(&executions));
    let mut session = Session::new(User {
        id: "alex",
        can_remove_co_buyer: true,
    });
    open_session(&agent, &mut session).await;
    let proposal_id = propose(&agent, &mut session).await;

    let events = agent
        .handle(&mut session, ClientEvent::ConfirmAction { proposal_id })
        .await;

    let [ServerEvent::ProposalStateChanged { state, result, .. }] = &events[..] else {
        panic!("expected one state change, got {events:?}");
    };
    assert_eq!(*state, ProposalState::Executed);
    assert_eq!(result, &Some(json!({"removed": true})));
    let recorded = executions.lock().expect("lock");
    assert_eq!(
        recorded
            .iter()
            .map(|(who, params)| (who.as_str(), params["transactionId"].as_str()))
            .collect::<Vec<_>>(),
        vec![("alex", Some("tx_581"))],
        "handler must run exactly once, as the confirming principal"
    );
}

#[tokio::test]
async fn unauthorized_principal_gets_decline_not_proposal() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let agent = agent_with(ScriptedLlm::returning(tool_call()), Arc::clone(&executions));
    let mut session = Session::new(User {
        id: "mallory",
        can_remove_co_buyer: false,
    });
    open_session(&agent, &mut session).await;

    let events = agent
        .handle(&mut session, user_message("remove the co-buyer"))
        .await;

    assert!(
        matches!(&events[..], [ServerEvent::ChatMessage { .. }]),
        "unauthorized action must decline conversationally, got {events:?}"
    );
}

#[tokio::test]
async fn reject_is_terminal() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let agent = agent_with(ScriptedLlm::returning(tool_call()), Arc::clone(&executions));
    let mut session = Session::new(User {
        id: "alex",
        can_remove_co_buyer: true,
    });
    open_session(&agent, &mut session).await;
    let proposal_id = propose(&agent, &mut session).await;

    agent
        .handle(
            &mut session,
            ClientEvent::RejectAction {
                proposal_id: proposal_id.clone(),
                reason: Some("changed my mind".to_owned()),
            },
        )
        .await;
    let events = agent
        .handle(&mut session, ClientEvent::ConfirmAction { proposal_id })
        .await;

    let [ServerEvent::ProposalStateChanged { state, .. }] = &events[..] else {
        panic!("expected state echo, got {events:?}");
    };
    assert_eq!(
        *state,
        ProposalState::Rejected,
        "confirming a rejected proposal must not execute"
    );
    assert!(executions.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn route_change_expires_pending_proposal() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let agent = agent_with(ScriptedLlm::returning(tool_call()), Arc::clone(&executions));
    let mut session = Session::new(User {
        id: "alex",
        can_remove_co_buyer: true,
    });
    open_session(&agent, &mut session).await;
    let proposal_id = propose(&agent, &mut session).await;

    let events = agent
        .handle(
            &mut session,
            ClientEvent::ContextUpdate {
                context: Context {
                    route: "/transactions".to_owned(),
                    ..Context::default()
                },
                context_seq: 1,
            },
        )
        .await;

    let [
        ServerEvent::ProposalStateChanged {
            proposal_id: expired_id,
            state,
            ..
        },
    ] = &events[..]
    else {
        panic!("expected expiry event, got {events:?}");
    };
    assert_eq!(
        (expired_id.as_str(), *state),
        (proposal_id.as_str(), ProposalState::Expired)
    );
}

fn context_update(selected: &str, context_seq: u64) -> ClientEvent {
    ClientEvent::ContextUpdate {
        context: Context {
            selected_record_id: Some(selected.to_owned()),
            ..tx_context()
        },
        context_seq,
    }
}

fn is_stale_context(events: &[ServerEvent]) -> bool {
    matches!(
        events,
        [ServerEvent::Error { code, recoverable: true, .. }] if code == "stale_context"
    )
}

#[tokio::test]
async fn a_context_update_that_does_not_advance_the_sequence_is_refused_and_not_applied() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let agent = agent_with(ScriptedLlm::returning(tool_call()), Arc::clone(&executions));
    let mut session = Session::new(User {
        id: "alex",
        can_remove_co_buyer: true,
    });
    open_session(&agent, &mut session).await;
    let applied = agent
        .handle(&mut session, context_update("tx_582", 3))
        .await;
    assert!(
        applied.is_empty(),
        "a growing sequence applies: {applied:?}"
    );

    let equal = agent
        .handle(&mut session, context_update("tx_999", 3))
        .await;
    let lower = agent
        .handle(&mut session, context_update("tx_999", 2))
        .await;
    let events = agent
        .handle(&mut session, user_message("remove the co-buyer"))
        .await;

    assert!(is_stale_context(&equal), "equal sequence: {equal:?}");
    assert!(is_stale_context(&lower), "lower sequence: {lower:?}");
    let [ServerEvent::ActionProposal(proposal)] = &events[..] else {
        panic!("expected one proposal, got {events:?}");
    };
    assert_eq!(
        (
            proposal.params["transactionId"].as_str(),
            proposal.context_seq
        ),
        (Some("tx_582"), 3),
        "the refused snapshots must not have replaced the applied one"
    );
}

#[tokio::test]
async fn the_first_context_update_after_init_must_carry_a_sequence_above_zero() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let agent = agent_with(ScriptedLlm::returning(tool_call()), Arc::clone(&executions));
    let mut session = Session::new(User {
        id: "alex",
        can_remove_co_buyer: true,
    });
    open_session(&agent, &mut session).await;

    let zero = agent
        .handle(&mut session, context_update("tx_999", 0))
        .await;
    let one = agent
        .handle(&mut session, context_update("tx_582", 1))
        .await;

    assert!(is_stale_context(&zero), "Init's snapshot is 0: {zero:?}");
    assert!(one.is_empty(), "1 follows Init's 0: {one:?}");
}

#[tokio::test]
async fn a_later_init_resets_the_context_sequence() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let agent = agent_with(ScriptedLlm::returning(tool_call()), Arc::clone(&executions));
    let mut session = Session::new(User {
        id: "alex",
        can_remove_co_buyer: true,
    });
    open_session(&agent, &mut session).await;
    let applied = agent
        .handle(&mut session, context_update("tx_582", 5))
        .await;
    assert!(applied.is_empty(), "got {applied:?}");

    open_session(&agent, &mut session).await;
    let after_reinit = agent
        .handle(&mut session, context_update("tx_583", 1))
        .await;

    assert!(
        after_reinit.is_empty(),
        "a second Init resets the sequence to 0: {after_reinit:?}"
    );
}

#[tokio::test]
async fn confirm_of_unknown_proposal_is_an_error() {
    let executions = Arc::new(Mutex::new(Vec::new()));
    let agent = agent_with(ScriptedLlm::returning(tool_call()), Arc::clone(&executions));
    let mut session = Session::new(User {
        id: "alex",
        can_remove_co_buyer: true,
    });
    open_session(&agent, &mut session).await;

    let events = agent
        .handle(
            &mut session,
            ClientEvent::ConfirmAction {
                proposal_id: "prop-999".to_owned(),
            },
        )
        .await;

    assert!(
        matches!(&events[..], [ServerEvent::Error { code, .. }] if code == "unknown_proposal"),
        "got {events:?}"
    );
}

/// The lifecycle telemetry contract: one event per *accepted* transition,
/// every one of them keyed by action.
mod mandate {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A revocable mandate: allow while the flag is up, deny once lowered —
    /// the "user edits their delegation contract mid-session" shape.
    struct FlagMandate {
        delegated: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Mandate<User> for FlagMandate {
        async fn mandate(
            &self,
            _principal: &User,
            _action_id: &str,
            _params: &JsonMap,
            _preview: Option<&DryRunPreview>,
        ) -> MandateDecision {
            if self.delegated.load(Ordering::SeqCst) {
                MandateDecision::Allow
            } else {
                MandateDecision::Deny("co-buyer removal is no longer delegated".to_owned())
            }
        }
    }

    fn mandated_agent(
        executions: Arc<Mutex<Vec<(String, JsonMap)>>>,
        delegated: Arc<AtomicBool>,
    ) -> Agent<User> {
        agent_with(ScriptedLlm::returning(tool_call()), executions)
            .with_mandate(Arc::new(FlagMandate { delegated }))
    }

    #[tokio::test]
    async fn out_of_mandate_proposal_never_renders() {
        let executions = Arc::new(Mutex::new(Vec::new()));
        let agent = mandated_agent(Arc::clone(&executions), Arc::new(AtomicBool::new(false)));
        let mut session = Session::new(User {
            id: "u_1",
            can_remove_co_buyer: true,
        });
        open_session(&agent, &mut session).await;

        let events = agent
            .handle(&mut session, user_message("remove the co-buyer"))
            .await;

        // The authorized-but-undelegated action declines in chat — no
        // proposal event, so no card can render and no confirm can follow.
        match &events[..] {
            [
                ServerEvent::ChatMessage {
                    text, done: true, ..
                },
            ] => {
                assert!(
                    text.contains("outside the agent's mandate"),
                    "decline should name the mandate: {text}"
                );
            }
            other => panic!("expected a decline, got {other:?}"),
        }
        assert!(executions.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn mandate_revoked_between_propose_and_confirm_expires() {
        let executions = Arc::new(Mutex::new(Vec::new()));
        let delegated = Arc::new(AtomicBool::new(true));
        let agent = mandated_agent(Arc::clone(&executions), Arc::clone(&delegated));
        let mut session = Session::new(User {
            id: "u_1",
            can_remove_co_buyer: true,
        });
        open_session(&agent, &mut session).await;
        let proposal_id = propose(&agent, &mut session).await;

        delegated.store(false, Ordering::SeqCst);
        let events = agent
            .handle(&mut session, ClientEvent::ConfirmAction { proposal_id })
            .await;

        // Confirm-time revalidation re-runs the mandate: the stale approval
        // expires instead of executing (§7.3 / §9.8).
        match &events[..] {
            [ServerEvent::ProposalStateChanged { state, .. }] => {
                assert_eq!(*state, ProposalState::Expired);
            }
            other => panic!("expected expiry, got {other:?}"),
        }
        assert!(
            executions.lock().expect("lock").is_empty(),
            "a revoked mandate must never execute"
        );
    }
}

mod telemetry {
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex, Once};

    use tracing::field::{Field, Visit};

    use super::{
        Agent, ClientEvent, ProposalState, ScriptedLlm, Session, User, agent_with, open_session,
        propose, tool_call,
    };

    /// `(state, action_id)` of every `cleverhans::telemetry::proposal` event.
    #[derive(Default)]
    struct Events {
        seen: Mutex<Vec<(String, String)>>,
    }

    thread_local! {
        /// The sink of the test running on this thread, while it records.
        static SINK: RefCell<Option<Arc<Events>>> = const { RefCell::new(None) };
    }

    /// This thread's sink, if any. `try_with`: a thread being torn down may
    /// still emit, and must not panic here.
    fn sink() -> Option<Arc<Events>> {
        SINK.try_with(|sink| sink.borrow().clone()).ok().flatten()
    }

    /// One collector for the whole test binary; each event goes to the sink
    /// of the test on the emitting thread, so tests stay parallel and
    /// independent.
    ///
    /// Not a per-test `set_default`: `tracing` caches a callsite's interest
    /// process-wide when the callsite is first hit, and while at most one
    /// dispatcher is registered, `tracing-core` asks only the default
    /// subscriber of the thread that hits it. A test without a subscriber
    /// hitting `proposal_state` first cached `never`, and the telemetry
    /// test's own collector then saw no event. As the global default,
    /// installed before any agent exists, this collector is every thread's
    /// view of every callsite, and it never answers `never`.
    struct Collector;

    /// Installs [`Collector`] as the global default, once per process.
    pub(super) fn install() {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            tracing::subscriber::set_global_default(Collector)
                .expect("no other global subscriber in this test binary");
        });
    }

    /// Routes this thread's telemetry to a sink until dropped.
    struct Recording;

    impl Drop for Recording {
        fn drop(&mut self) {
            // Ignore a thread already tearing down: its sink goes with it.
            let _ = SINK.try_with(|sink| sink.borrow_mut().take());
        }
    }

    #[derive(Default)]
    struct Captured {
        state: String,
        action_id: String,
    }

    impl Visit for Captured {
        fn record_str(&mut self, field: &Field, value: &str) {
            match field.name() {
                "state" => self.state = value.to_owned(),
                "action_id" => self.action_id = value.to_owned(),
                _ => {}
            }
        }

        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.record_str(field, format!("{value:?}").trim_matches('"'));
        }
    }

    impl tracing::Subscriber for Collector {
        fn register_callsite(&self, _: &tracing::Metadata<'_>) -> tracing::subscriber::Interest {
            // Decided per event in `enabled`: whether the emitting thread
            // records changes from test to test.
            tracing::subscriber::Interest::sometimes()
        }

        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            metadata.target() == "cleverhans::telemetry::proposal" && sink().is_some()
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let Some(events) = sink() else { return };
            let mut captured = Captured::default();
            event.record(&mut captured);
            events
                .seen
                .lock()
                .expect("lock")
                .push((captured.state, captured.action_id));
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// Records this thread's proposal telemetry until the guard drops. The
    /// session runs on this thread: `#[tokio::test]` is a current-thread
    /// runtime.
    fn collecting() -> (Arc<Events>, Recording) {
        install();
        let events = Arc::new(Events::default());
        SINK.with(|sink| *sink.borrow_mut() = Some(Arc::clone(&events)));
        (events, Recording)
    }

    async fn rejected_session() -> (Arc<Events>, Recording) {
        let (events, guard) = collecting();
        let executions = Arc::new(Mutex::new(Vec::new()));
        let agent: Agent<User> =
            agent_with(ScriptedLlm::returning(tool_call()), Arc::clone(&executions));
        let mut session = Session::new(User {
            id: "alex",
            can_remove_co_buyer: true,
        });
        open_session(&agent, &mut session).await;
        let proposal_id = propose(&agent, &mut session).await;
        agent
            .handle(
                &mut session,
                ClientEvent::RejectAction {
                    proposal_id: proposal_id.clone(),
                    reason: Some("changed my mind".to_owned()),
                },
            )
            .await;
        // A confirm after the rejection: the store refuses the edge, so it
        // must not show up as a second lifecycle event.
        agent
            .handle(&mut session, ClientEvent::ConfirmAction { proposal_id })
            .await;
        (events, guard)
    }

    #[tokio::test]
    async fn a_rejection_is_keyed_by_action() {
        let (recorded, _guard) = rejected_session().await;
        let events = recorded.seen.lock().expect("lock").clone();
        assert!(
            events.contains(&(
                ProposalState::Rejected.to_string(),
                "transaction.coBuyer.remove".to_owned()
            )),
            "rejection must carry action_id, got {events:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_transition_emits_nothing() {
        let (recorded, _guard) = rejected_session().await;
        let events = recorded.seen.lock().expect("lock").clone();
        let states: Vec<&str> = events.iter().map(|(state, _)| state.as_str()).collect();
        assert_eq!(
            states,
            vec![
                ProposalState::Validated.to_string(),
                ProposalState::Rejected.to_string()
            ],
            "only accepted transitions are lifecycle events"
        );
    }
}
