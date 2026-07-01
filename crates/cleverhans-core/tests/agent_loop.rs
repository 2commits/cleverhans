//! Exercises the tightened LlmProvider contract: guaranteed system turn as
//! `messages[0]`, and the bounded validation-retry loop.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use cleverhans_core::JsonMap;
use cleverhans_core::agent::{Agent, AgentConfig, DEFAULT_SYSTEM_PROMPT, Session};
use cleverhans_core::envelope::{ClientEvent, Context, ServerEvent};
use cleverhans_core::error::{HandlerError, LlmError};
use cleverhans_core::registry::{ActionDef, BlockDef, ParamSource, ParamSpec, Registry, ValueType};
use cleverhans_core::seams::{
    ActionHandler, AuthzDecision, AuthzResolver, ChatRole, CompletionItem, CompletionRequest,
    ContextParamResolver, LlmProvider,
};

#[derive(Clone)]
struct User {
    allowed: bool,
}

/// Scripted provider that also records every request it receives.
struct RecordingLlm {
    responses: Mutex<VecDeque<Vec<CompletionItem>>>,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

impl RecordingLlm {
    fn scripted(responses: Vec<Vec<CompletionItem>>) -> (Self, Arc<Mutex<Vec<CompletionRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                responses: Mutex::new(responses.into()),
                requests: Arc::clone(&requests),
            },
            requests,
        )
    }
}

#[async_trait]
impl LlmProvider for RecordingLlm {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        self.requests.lock().expect("lock").push(request);
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
        Ok(serde_json::Value::Null)
    }
}

struct FlagAuthz;

#[async_trait]
impl AuthzResolver<User> for FlagAuthz {
    async fn authorize(
        &self,
        principal: &User,
        _action_id: &str,
        _params: &JsonMap,
    ) -> AuthzDecision {
        if principal.allowed {
            AuthzDecision::Allow
        } else {
            AuthzDecision::Deny("not allowed".to_owned())
        }
    }
}

struct NoContextParams;

impl ContextParamResolver for NoContextParams {
    fn resolve(
        &self,
        _action_id: &str,
        _param: &ParamSpec,
        _context: &Context,
    ) -> Option<serde_json::Value> {
        None
    }
}

fn registry() -> Registry<User> {
    Registry::builder()
        .block(BlockDef {
            block_type: "confirm".to_owned(),
            slots: vec![],
        })
        .action(
            ActionDef {
                id: "note.create".to_owned(),
                description: "Create a note".to_owned(),
                params: vec![ParamSpec {
                    name: "text".to_owned(),
                    description: "Note body".to_owned(),
                    ty: ValueType::String,
                    source: ParamSource::Utterance,
                    required: true,
                }],
                block_type: "confirm".to_owned(),
                mutates: false,
                authz_key: "note.create".to_owned(),
            },
            Arc::new(OkHandler),
            None,
            None,
        )
        .build()
        .expect("valid registry")
}

fn agent(
    responses: Vec<Vec<CompletionItem>>,
    config: AgentConfig,
) -> (Agent<User>, Arc<Mutex<Vec<CompletionRequest>>>) {
    let (llm, requests) = RecordingLlm::scripted(responses);
    (
        Agent::with_config(
            Arc::new(registry()),
            Arc::new(llm),
            Arc::new(FlagAuthz),
            Arc::new(NoContextParams),
            config,
        ),
        requests,
    )
}

fn good_call() -> Vec<CompletionItem> {
    let mut arguments = JsonMap::new();
    arguments.insert("text".to_owned(), json!("buy milk"));
    vec![CompletionItem::ToolCall {
        name: "note.create".to_owned(),
        arguments,
    }]
}

fn bad_call() -> Vec<CompletionItem> {
    vec![CompletionItem::ToolCall {
        name: "made.up".to_owned(),
        arguments: JsonMap::new(),
    }]
}

async fn send_message(agent: &Agent<User>, session: &mut Session<User>) -> Vec<ServerEvent> {
    agent
        .handle(
            session,
            ClientEvent::UserMessage {
                text: "note: buy milk".to_owned(),
                client_msg_id: "c-1".to_owned(),
            },
        )
        .await
}

#[tokio::test]
async fn every_completion_starts_with_the_system_turn() {
    let (agent, requests) = agent(vec![good_call()], AgentConfig::default());
    let mut session = Session::new(User { allowed: true });

    send_message(&agent, &mut session).await;

    let recorded = requests.lock().expect("lock");
    let first = &recorded[0].messages[0];
    assert!(
        first.role == ChatRole::System && first.content == DEFAULT_SYSTEM_PROMPT,
        "messages[0] must be the framework system turn, got {first:?}"
    );
}

#[tokio::test]
async fn app_instructions_are_appended_to_the_system_turn() {
    let config = AgentConfig {
        app_instructions: Some("Answer in Danish.".to_owned()),
        ..AgentConfig::default()
    };
    let (agent, requests) = agent(vec![good_call()], config);
    let mut session = Session::new(User { allowed: true });

    send_message(&agent, &mut session).await;

    let recorded = requests.lock().expect("lock");
    let content = &recorded[0].messages[0].content;
    assert!(
        content.starts_with(DEFAULT_SYSTEM_PROMPT) && content.ends_with("Answer in Danish."),
        "app instructions must follow the preamble, got: {content}"
    );
}

#[tokio::test]
async fn fixable_failure_retries_and_recovers() {
    let (agent, requests) = agent(vec![bad_call(), good_call()], AgentConfig::default());
    let mut session = Session::new(User { allowed: true });

    let events = send_message(&agent, &mut session).await;

    assert!(
        matches!(&events[..], [ServerEvent::ActionProposal(p)] if p.action_id == "note.create"),
        "retry should end in a proposal, got {events:?}"
    );
    let recorded = requests.lock().expect("lock");
    assert_eq!(recorded.len(), 2, "one initial call plus one retry");
}

#[tokio::test]
async fn retry_request_carries_the_validation_failure() {
    let (agent, requests) = agent(vec![bad_call(), good_call()], AgentConfig::default());
    let mut session = Session::new(User { allowed: true });

    send_message(&agent, &mut session).await;

    let recorded = requests.lock().expect("lock");
    let last = recorded[1].messages.last().expect("non-empty");
    assert!(
        last.role == ChatRole::Tool && last.content.contains("unknown action `made.up`"),
        "retry must see the failure as a tool turn, got {last:?}"
    );
}

#[tokio::test]
async fn exhausted_retries_decline_conversationally() {
    let config = AgentConfig {
        max_validation_retries: 1,
        ..AgentConfig::default()
    };
    let (agent, requests) = agent(vec![bad_call(), bad_call()], config);
    let mut session = Session::new(User { allowed: true });

    let events = send_message(&agent, &mut session).await;

    assert!(
        matches!(&events[..], [ServerEvent::ChatMessage { text, .. }] if text.contains("made.up")),
        "exhaustion must decline, got {events:?}"
    );
    let recorded = requests.lock().expect("lock");
    assert_eq!(recorded.len(), 2, "retry budget of 1 means exactly 2 calls");
}

#[tokio::test]
async fn unauthorized_failure_never_retries() {
    let (agent, requests) = agent(vec![good_call()], AgentConfig::default());
    let mut session = Session::new(User { allowed: false });

    let events = send_message(&agent, &mut session).await;

    assert!(
        matches!(&events[..], [ServerEvent::ChatMessage { text, .. }] if text.contains("not authorized")),
        "authz denial must decline immediately, got {events:?}"
    );
    let recorded = requests.lock().expect("lock");
    assert_eq!(
        recorded.len(),
        1,
        "non-fixable failures must not spend the retry budget"
    );
}
