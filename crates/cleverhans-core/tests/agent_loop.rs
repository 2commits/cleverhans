//! Exercises the tightened LlmProvider contract: guaranteed system turn as
//! `messages[0]`, and the bounded validation-retry loop.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use cleverhans_core::JsonMap;
use cleverhans_core::agent::{Agent, AgentConfig, DEFAULT_SYSTEM_PROMPT, Session};
use cleverhans_core::envelope::{ClientEvent, Context, ServerEvent};
use cleverhans_core::error::{HandlerError, LlmError};
use cleverhans_core::registry::{ActionDef, BlockDef, ParamSource, ParamSpec, Registry, ValueType};
use cleverhans_core::seams::{
    ActionHandler, AuthzDecision, AuthzResolver, ChatRole, CompletionChunk, CompletionItem,
    CompletionRequest, CompletionStream, ContextParamResolver, LlmProvider,
};
use cleverhans_core::test_util::ScriptedLlm;

#[derive(Clone)]
struct User {
    allowed: bool,
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
                display: None,
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
) -> (Agent<User>, Arc<ScriptedLlm>) {
    let llm = Arc::new(ScriptedLlm::new(responses));
    (
        Agent::with_config(
            Arc::new(registry()),
            llm.clone(),
            Arc::new(FlagAuthz),
            Arc::new(NoContextParams),
            config,
        ),
        llm,
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

/// Provider with a native streaming implementation emitting fixed chunks.
struct StreamingLlm {
    chunks: Vec<CompletionChunk>,
}

#[async_trait]
impl LlmProvider for StreamingLlm {
    async fn complete(&self, _request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        unreachable!("agent must use the streaming path");
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionStream, LlmError> {
        let chunks: Vec<Result<CompletionChunk, LlmError>> =
            self.chunks.clone().into_iter().map(Ok).collect();
        Ok(Box::pin(futures_util::stream::iter(chunks)))
    }
}

#[tokio::test]
async fn streaming_provider_yields_deltas_then_authoritative_message() {
    let llm = StreamingLlm {
        chunks: vec![
            CompletionChunk::TextDelta("Hel".to_owned()),
            CompletionChunk::TextDelta("lo.".to_owned()),
            CompletionChunk::TextDone,
        ],
    };
    let agent = Agent::new(
        Arc::new(registry()),
        Arc::new(llm),
        Arc::new(FlagAuthz),
        Arc::new(NoContextParams),
    );
    let mut session = Session::new(User { allowed: true });

    let events = send_message(&agent, &mut session).await;

    let [
        ServerEvent::ChatMessage {
            msg_id: id_a,
            text: text_a,
            done: false,
        },
        ServerEvent::ChatMessage {
            msg_id: id_b,
            text: text_b,
            done: false,
        },
        ServerEvent::ChatMessage {
            msg_id: id_c,
            text: full,
            done: true,
        },
    ] = &events[..]
    else {
        panic!("expected two deltas then the final message, got {events:?}");
    };
    assert!(
        id_b == id_a && id_c == id_a,
        "all fragments must share one msg_id: {id_a} {id_b} {id_c}"
    );
    assert_eq!((text_a.as_str(), text_b.as_str()), ("Hel", "lo."));
    assert_eq!(full, "Hello.", "done:true must carry the full text");
}

#[tokio::test]
async fn every_completion_starts_with_the_system_turn() {
    let (agent, requests) = agent(vec![good_call()], AgentConfig::default());
    let mut session = Session::new(User { allowed: true });

    send_message(&agent, &mut session).await;

    let recorded = requests.requests();
    let first = &recorded[0].messages[0];
    assert!(
        first.role == ChatRole::System && first.content == DEFAULT_SYSTEM_PROMPT,
        "messages[0] must be the framework system turn, got {first:?}"
    );
}

#[tokio::test]
async fn context_note_is_the_last_message_and_names_no_ids() {
    let (agent, requests) = agent(vec![good_call()], AgentConfig::default());
    let mut session = Session::new(User { allowed: true });
    agent
        .handle(
            &mut session,
            ClientEvent::Init {
                spec_version: "0.1.0-draft".to_owned(),
                context: Context {
                    route: "/notes/note_7".to_owned(),
                    selected_record_id: Some("note_7".to_owned()),
                    view_type: Some("detail".to_owned()),
                    ..Context::default()
                },
            },
        )
        .await;

    send_message(&agent, &mut session).await;

    let recorded = requests.requests();
    let last = recorded[0].messages.last().expect("non-empty");
    assert!(
        last.role == ChatRole::System
            && last.content.contains("route `/notes/note_7`")
            && last.content.contains("viewing a detail")
            && last.content.contains("a record is selected"),
        "context note wrong: {last:?}"
    );
}

#[tokio::test]
async fn context_note_can_be_disabled() {
    let config = AgentConfig {
        describe_context: false,
        ..AgentConfig::default()
    };
    let (agent, requests) = agent(vec![good_call()], config);
    let mut session = Session::new(User { allowed: true });

    send_message(&agent, &mut session).await;

    let recorded = requests.requests();
    assert!(
        !recorded[0]
            .messages
            .iter()
            .any(|turn| turn.content.contains("Current app context")),
        "context note must be absent when disabled"
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

    let recorded = requests.requests();
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
    let recorded = requests.requests();
    assert_eq!(recorded.len(), 2, "one initial call plus one retry");
}

#[tokio::test]
async fn retry_request_carries_the_validation_failure() {
    let (agent, requests) = agent(vec![bad_call(), good_call()], AgentConfig::default());
    let mut session = Session::new(User { allowed: true });

    send_message(&agent, &mut session).await;

    let recorded = requests.requests();
    let last_tool_turn = recorded[1]
        .messages
        .iter()
        .rev()
        .find(|turn| turn.role == ChatRole::Tool)
        .expect("retry request has a tool turn");
    assert!(
        last_tool_turn.content.contains("unknown action `made.up`"),
        "retry must see the failure as a tool turn, got {last_tool_turn:?}"
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
    let recorded = requests.requests();
    assert_eq!(recorded.len(), 2, "retry budget of 1 means exactly 2 calls");
}

/// Registry with one action whose only param is context-sourced — paired
/// with [`NoContextParams`], every proposal fails resolution.
fn context_param_registry() -> Registry<User> {
    Registry::builder()
        .block(BlockDef {
            block_type: "confirm".to_owned(),
            slots: vec![],
        })
        .action(
            ActionDef {
                id: "doc.touch".to_owned(),
                description: "Touch the selected document".to_owned(),
                params: vec![ParamSpec {
                    name: "documentId".to_owned(),
                    description: String::new(),
                    ty: ValueType::String,
                    source: ParamSource::Context,
                    required: true,
                }],
                block_type: "confirm".to_owned(),
                mutates: false,
                authz_key: "doc.touch".to_owned(),
                display: None,
            },
            Arc::new(OkHandler),
            None,
            None,
        )
        .build()
        .expect("valid registry")
}

#[tokio::test]
async fn unresolved_context_param_feedback_frames_the_failure_as_momentary() {
    let llm = Arc::new(ScriptedLlm::new(vec![
        vec![CompletionItem::ToolCall {
            name: "doc.touch".to_owned(),
            arguments: JsonMap::new(),
        }],
        vec![CompletionItem::Text("understood".to_owned())],
    ]));
    let agent = Agent::with_config(
        Arc::new(context_param_registry()),
        llm.clone(),
        Arc::new(FlagAuthz),
        Arc::new(NoContextParams),
        AgentConfig::default(),
    );
    let mut session = Session::new(User { allowed: true });

    // Turn 1: the resolution miss declines without spending retries.
    let events = send_message(&agent, &mut session).await;
    assert!(
        matches!(&events[..], [ServerEvent::ChatMessage { text, .. }] if text.contains("documentId")),
        "unresolved context param must decline, got {events:?}"
    );

    // Turn 2: the model must see guidance that the miss was momentary, not
    // the generic fix-your-arguments advice.
    send_message(&agent, &mut session).await;
    let recorded = llm.requests();
    let tool_turn = recorded[1]
        .messages
        .iter()
        .rev()
        .find(|turn| turn.role == ChatRole::Tool)
        .expect("second request carries the failure tool turn");
    assert!(
        tool_turn.content.contains("once the user navigates"),
        "guidance must frame the failure as momentary, got {tool_turn:?}"
    );
    assert!(
        !tool_turn
            .content
            .contains("arguments matching their schema"),
        "context misses must not get schema advice, got {tool_turn:?}"
    );
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
    let recorded = requests.requests();
    assert_eq!(
        recorded.len(),
        1,
        "non-fixable failures must not spend the retry budget"
    );
}
