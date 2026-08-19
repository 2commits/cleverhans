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
    ContextParamResolver, DryRunHandler, LlmProvider, SlotBuilder,
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
