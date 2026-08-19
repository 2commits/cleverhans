//! Drives `run_session` over the wire types end to end: init → user message
//! → proposal → confirm → executed, plus the init-first transport rule.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use cleverhans_core::JsonMap;
use cleverhans_core::agent::Agent;
use cleverhans_core::envelope::{Context, DryRunPreview};
use cleverhans_core::error::{HandlerError, LlmError};
use cleverhans_core::registry::{
    ActionDef, BlockDef, ParamSource, ParamSpec, Registry, SlotSpec, ValueType,
};
use cleverhans_core::seams::{
    ActionHandler, AuthzDecision, AuthzResolver, CompletionItem, CompletionRequest,
    ContextParamResolver, DryRunHandler, LlmProvider, SlotBuilder,
};
use cleverhans_grpc::service::run_session;
use cleverhans_grpc::{convert, pb};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

#[derive(Clone)]
struct User;

struct ScriptedLlm {
    responses: Mutex<VecDeque<Vec<CompletionItem>>>,
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

struct OkHandler;

#[async_trait]
impl ActionHandler<User> for OkHandler {
    async fn execute(
        &self,
        _params: &JsonMap,
        _principal: &User,
    ) -> Result<serde_json::Value, HandlerError> {
        Ok(json!({"removed": true}))
    }
}

struct OnePreview;

#[async_trait]
impl DryRunHandler<User> for OnePreview {
    async fn dry_run(
        &self,
        _params: &JsonMap,
        _principal: &User,
    ) -> Result<DryRunPreview, HandlerError> {
        Ok(DryRunPreview {
            affected_count: 1,
            ..DryRunPreview::default()
        })
    }
}

struct TitleSlot;

impl SlotBuilder for TitleSlot {
    fn build(&self, _params: &JsonMap, _preview: Option<&DryRunPreview>) -> JsonMap {
        let mut slots = JsonMap::new();
        slots.insert("title".to_owned(), json!("Remove co-buyer"));
        slots
    }
}

struct AllowAll;

#[async_trait]
impl AuthzResolver<User> for AllowAll {
    async fn authorize(
        &self,
        _principal: &User,
        _action_id: &str,
        _params: &JsonMap,
    ) -> AuthzDecision {
        AuthzDecision::Allow
    }
}

struct SelectionResolver;

impl ContextParamResolver for SelectionResolver {
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

fn agent() -> Arc<Agent<User>> {
    let registry = Registry::builder()
        .block(BlockDef {
            block_type: "confirm".to_owned(),
            slots: vec![SlotSpec {
                name: "title".to_owned(),
                ty: ValueType::String,
                required: true,
            }],
        })
        .action(
            ActionDef {
                id: "transaction.coBuyer.remove".to_owned(),
                description: "Remove the co-buyer".to_owned(),
                params: vec![ParamSpec {
                    name: "transactionId".to_owned(),
                    description: String::new(),
                    ty: ValueType::String,
                    source: ParamSource::Context,
                    required: true,
                }],
                block_type: "confirm".to_owned(),
                mutates: true,
                authz_key: "tx.cobuyer.remove".to_owned(),
                display: None,
            },
            Arc::new(OkHandler),
            Some(Arc::new(OnePreview)),
            Some(Arc::new(TitleSlot)),
        )
        .build()
        .expect("valid registry");
    let llm = ScriptedLlm {
        responses: Mutex::new(VecDeque::from([vec![CompletionItem::ToolCall {
            name: "transaction.coBuyer.remove".to_owned(),
            arguments: JsonMap::new(),
        }]])),
    };
    Arc::new(Agent::new(
        Arc::new(registry),
        Arc::new(llm),
        Arc::new(AllowAll),
        Arc::new(SelectionResolver),
    ))
}

fn pb_init() -> pb::ClientEvent {
    pb::ClientEvent {
        event: Some(pb::client_event::Event::Init(pb::Init {
            spec_version: "0.1.0-draft".to_owned(),
            context: Some(pb::Context {
                route: "/transactions/tx_581".to_owned(),
                params: None,
                selected_record_id: Some("tx_581".to_owned()),
                view_type: Some("detail".to_owned()),
                extensions: None,
            }),
        })),
    }
}

fn pb_user_message(text: &str) -> pb::ClientEvent {
    pb::ClientEvent {
        event: Some(pb::client_event::Event::UserMessage(pb::UserMessage {
            text: text.to_owned(),
            client_msg_id: "c-1".to_owned(),
        })),
    }
}

fn pb_confirm(proposal_id: &str) -> pb::ClientEvent {
    pb::ClientEvent {
        event: Some(pb::client_event::Event::ConfirmAction(pb::ConfirmAction {
            proposal_id: proposal_id.to_owned(),
        })),
    }
}

/// Runs a scripted client conversation through `run_session`, collecting
/// everything the server streams back.
async fn drive(events: Vec<pb::ClientEvent>) -> Vec<pb::server_event::Event> {
    let (in_tx, in_rx) = mpsc::channel::<Result<pb::ClientEvent, Status>>(8);
    let (out_tx, mut out_rx) = mpsc::channel(8);
    for event in events {
        in_tx.send(Ok(event)).await.expect("send");
    }
    drop(in_tx);
    run_session(agent(), User, ReceiverStream::new(in_rx), out_tx).await;

    let mut received = Vec::new();
    while let Some(item) = out_rx.recv().await {
        received.push(item.expect("ok event").event.expect("payload"));
    }
    received
}

#[tokio::test]
async fn full_flow_over_wire_types_executes_on_confirm() {
    let events = drive(vec![
        pb_init(),
        pb_user_message("remove the co-buyer"),
        pb_confirm("prop-1"),
    ])
    .await;

    let [
        pb::server_event::Event::ActionProposal(proposal),
        pb::server_event::Event::ProposalStateChanged(changed),
    ] = &events[..]
    else {
        panic!("expected proposal then state change, got {events:?}");
    };
    assert_eq!(
        (
            proposal.proposal_id.as_str(),
            proposal.action_id.as_str(),
            proposal.preview.as_ref().map(|p| p.affected_count),
            changed.state.as_str(),
        ),
        ("prop-1", "transaction.coBuyer.remove", Some(1), "executed")
    );
}

#[tokio::test]
async fn proposal_params_survive_struct_encoding() {
    let events = drive(vec![pb_init(), pb_user_message("remove the co-buyer")]).await;

    let [pb::server_event::Event::ActionProposal(proposal)] = &events[..] else {
        panic!("expected one proposal, got {events:?}");
    };
    let params = convert::struct_to_map(proposal.params.clone().expect("params"));
    assert_eq!(params["transactionId"], json!("tx_581"));
}

#[tokio::test]
async fn non_init_first_message_closes_stream_unrecoverably() {
    let events = drive(vec![pb_user_message("hello"), pb_init()]).await;

    let [pb::server_event::Event::Error(error)] = &events[..] else {
        panic!("expected single error, got {events:?}");
    };
    assert_eq!(
        (error.code.as_str(), error.recoverable),
        ("init_required", false)
    );
}
