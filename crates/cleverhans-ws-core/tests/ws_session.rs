//! Drives the WebSocket binding's session loop over raw JSON frames — the
//! exact bytes `@cleverhans/react`'s WebSocket transport produces.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use cleverhans_core::JsonMap;
use cleverhans_core::agent::Agent;
use cleverhans_core::envelope::Context;
use cleverhans_core::error::{HandlerError, LlmError};
use cleverhans_core::registry::{
    ActionDef, BlockDef, ParamSource, ParamSpec, Registry, SlotSpec, ValueType,
};
use cleverhans_core::seams::{
    ActionHandler, AuthzDecision, AuthzResolver, CompletionItem, CompletionRequest,
    ContextParamResolver, DryRunHandler, LlmProvider, SlotBuilder,
};
use cleverhans_ws_core::run_session;

#[derive(Clone)]
struct User;

struct ScriptedLlm(Mutex<VecDeque<Vec<CompletionItem>>>);

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn complete(&self, _request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        self.0
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
    ) -> Result<cleverhans_core::envelope::DryRunPreview, HandlerError> {
        Ok(cleverhans_core::envelope::DryRunPreview {
            affected_count: 1,
            ..Default::default()
        })
    }
}

struct TitleSlot;

impl SlotBuilder for TitleSlot {
    fn build(
        &self,
        _params: &JsonMap,
        _preview: Option<&cleverhans_core::envelope::DryRunPreview>,
    ) -> JsonMap {
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
                authz_key: "tx".to_owned(),
            },
            Arc::new(OkHandler),
            Some(Arc::new(OnePreview)),
            Some(Arc::new(TitleSlot)),
        )
        .build()
        .expect("valid registry");
    let llm = ScriptedLlm(Mutex::new(VecDeque::from([vec![
        CompletionItem::ToolCall {
            name: "transaction.coBuyer.remove".to_owned(),
            arguments: JsonMap::new(),
        },
    ]])));
    Arc::new(Agent::new(
        Arc::new(registry),
        Arc::new(llm),
        Arc::new(AllowAll),
        Arc::new(SelectionResolver),
    ))
}

async fn drive(frames: Vec<String>) -> Vec<Value> {
    let (tx, mut rx) = mpsc::channel(16);
    run_session(agent(), User, stream::iter(frames), tx).await;
    let mut received = Vec::new();
    while let Some(json) = rx.recv().await {
        received.push(serde_json::from_str(&json).expect("valid outbound JSON"));
    }
    received
}

fn init_frame() -> String {
    json!({
        "type": "init",
        "spec_version": "0.1.0-draft",
        "context": {"route": "/transactions/tx_581", "selected_record_id": "tx_581"},
    })
    .to_string()
}

fn message_frame() -> String {
    json!({"type": "user_message", "text": "remove the co-buyer", "client_msg_id": "c-1"})
        .to_string()
}

#[tokio::test]
async fn full_flow_over_json_frames_executes_on_confirm() {
    let confirm = json!({"type": "confirm_action", "proposal_id": "prop-1"}).to_string();

    let events = drive(vec![init_frame(), message_frame(), confirm]).await;

    assert_eq!(
        events
            .iter()
            .map(|e| e["type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["action_proposal", "proposal_state_changed"],
        "got {events:?}"
    );
    assert_eq!(events[1]["state"], json!("executed"));
    assert_eq!(events[1]["result"], json!({"removed": true}));
}

#[tokio::test]
async fn proposal_frame_matches_frontend_wire_shape() {
    let events = drive(vec![init_frame(), message_frame()]).await;

    let proposal = &events[0];
    assert_eq!(
        (
            proposal["proposal_id"].as_str(),
            proposal["action_id"].as_str(),
            proposal["params"]["transactionId"].as_str(),
            proposal["slots"]["title"].as_str(),
            proposal["preview"]["affected_count"].as_u64(),
        ),
        (
            Some("prop-1"),
            Some("transaction.coBuyer.remove"),
            Some("tx_581"),
            Some("Remove co-buyer"),
            Some(1),
        ),
        "got {proposal:?}"
    );
}

#[tokio::test]
async fn non_init_first_frame_closes_unrecoverably() {
    let events = drive(vec![message_frame(), init_frame()]).await;

    assert_eq!(events.len(), 1, "stream must close after the error");
    assert_eq!(
        (
            events[0]["code"].as_str(),
            events[0]["recoverable"].as_bool()
        ),
        (Some("init_required"), Some(false))
    );
}

#[tokio::test]
async fn malformed_frame_is_recoverable() {
    let events = drive(vec![init_frame(), "not json".to_owned(), message_frame()]).await;

    assert_eq!(
        events
            .iter()
            .map(|e| e["type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["error", "action_proposal"],
        "malformed frame must not end the session: {events:?}"
    );
}
