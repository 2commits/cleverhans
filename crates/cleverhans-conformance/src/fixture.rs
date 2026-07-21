//! Serde model of fixture + vector files, and the scripted seams that turn
//! fixture data into a runnable [`Agent`].

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use cleverhans_core::JsonMap;
use cleverhans_core::agent::Agent;
use cleverhans_core::envelope::DryRunPreview;
use cleverhans_core::error::{HandlerError, LlmError};
use cleverhans_core::registry::RegistryBuilder;
use cleverhans_core::schema::RegistrySchema;
use cleverhans_core::seams::{
    ActionHandler, AuthzDecision, AuthzResolver, CompletionItem, CompletionRequest, DryRunHandler,
    LlmProvider, SlotBuilder,
};

/// The principal every vector runs as. Identity semantics are asserted via
/// the execution log; authorization is scripted, so no per-user modeling.
#[derive(Clone)]
pub struct VectorPrincipal;

/// A registry fixture: the declarative registry document plus per-action
/// seam scripts (which cannot live inside `ActionDef` — the schema rejects
/// unknown fields by design).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    /// Fixture name, referenced by vectors.
    pub name: String,
    /// The registry document, exactly the spec §4 serialization.
    pub registry: RegistrySchema,
    /// Seam scripts keyed by action ID. Every action MUST have an entry.
    pub scripts: BTreeMap<String, ActionScript>,
}

/// Scripted seam behaviors for one action.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionScript {
    /// Handler behavior.
    pub handler: HandlerScript,
    /// Dry-run behavior; required when the action mutates.
    #[serde(default)]
    pub dry_run: Option<DryRunScript>,
    /// Slot builder script; absent → empty slots.
    #[serde(default)]
    pub slots: Option<BTreeMap<String, SlotScript>>,
}

/// `{"return": <json>}` or `{"fail": "<message>"}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandlerScript {
    /// Succeed with this result.
    Return(Value),
    /// Fail with `HandlerError::Rejected`.
    Fail(String),
}

/// One dry-run behavior.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DryRunBehavior {
    /// Succeed with this preview.
    Preview(DryRunPreview),
    /// Fail with `HandlerError::Rejected`.
    Fail(String),
}

/// A single behavior, or a per-call sequence (propose-time call, then
/// confirm-time call, …) with `then` as the tail.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum DryRunScript {
    /// Same behavior on every call.
    One(DryRunBehavior),
    /// Indexed by call count, falling back to `then`.
    Sequence {
        /// Behavior for call *n*.
        sequence: Vec<DryRunBehavior>,
        /// Behavior after the sequence is exhausted.
        then: DryRunBehavior,
    },
}

/// One slot value source.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotScript {
    /// A fixed value.
    Const(Value),
    /// Copy a filled param.
    Param(String),
    /// Copy a dry-run preview field (`"summary"` in v1); omitted when the
    /// preview or field is absent.
    Preview(String),
}

/// `"allow"` or `{"deny": "<reason>"}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthzBehavior {
    /// Permit.
    Allow,
    /// Deny with reason.
    Deny(String),
}

/// Authz script: a default for every call, or a per-call sequence.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AuthzScript {
    /// Same decision on every call.
    Default {
        /// The decision.
        default: AuthzBehavior,
    },
    /// Indexed by call count (propose-time, confirm-time, …), falling back
    /// to `then`.
    Sequence {
        /// Behavior for call *n*.
        sequence: Vec<AuthzBehavior>,
        /// Behavior after the sequence is exhausted.
        then: AuthzBehavior,
    },
}

impl Default for AuthzScript {
    fn default() -> Self {
        Self::Default {
            default: AuthzBehavior::Allow,
        }
    }
}

/// One scripted model-output item — the neutral encoding shared by vectors,
/// the FFI scripted provider, and host LLM callbacks.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmItem {
    /// Assistant prose.
    Text(String),
    /// A tool call.
    ToolCall {
        /// Action ID.
        name: String,
        /// Utterance arguments.
        arguments: JsonMap,
    },
}

impl From<LlmItem> for CompletionItem {
    fn from(item: LlmItem) -> Self {
        match item {
            LlmItem::Text(text) => Self::Text(text),
            LlmItem::ToolCall { name, arguments } => Self::ToolCall { name, arguments },
        }
    }
}

/// One agent-layer step.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// A client event to send (may contain `$ref` directives).
    Send(Value),
    /// Expected server events since the last `send`, after normalization.
    Expect(Vec<Value>),
}

/// One binding-layer inbound frame.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Frame {
    /// A JSON object, serialized by the runner.
    Json(Value),
    /// A verbatim string (malformed-frame vectors).
    Raw(String),
}

/// An expected handler invocation.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionExpectation {
    /// The executed action.
    pub action_id: String,
    /// The exact params the handler received.
    pub params: JsonMap,
}

/// Which loop drives the vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Envelope events through `Agent::handle`, stepped send/expect.
    Agent,
    /// Raw text frames through the JSON-frame session loop.
    Binding,
}

/// One conformance vector.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vector {
    /// Vector name (file stem by convention).
    pub name: String,
    /// What the vector asserts.
    #[serde(default)]
    pub description: String,
    /// Spec section references.
    #[serde(default)]
    pub spec: Vec<String>,
    /// Driving layer.
    pub layer: Layer,
    /// Fixture name (resolved against `spec/vectors/fixtures/`).
    pub fixture: String,
    /// Scripted model output: one item list per LLM invocation.
    #[serde(default)]
    pub llm: Vec<Vec<LlmItem>>,
    /// Scripted authorization.
    #[serde(default)]
    pub authz: AuthzScript,
    /// Agent-layer steps.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Binding-layer frames.
    #[serde(default)]
    pub frames: Vec<Frame>,
    /// Binding-layer expected events (flat, after normalization).
    #[serde(default)]
    pub expect: Vec<Value>,
    /// Binding layer: no events may follow the matched list.
    #[serde(default)]
    pub expect_close: bool,
    /// Exact ordered handler-invocation log; `[]` asserts nothing executed.
    #[serde(default)]
    pub executions: Option<Vec<ExecutionExpectation>>,
    /// Keep `done: false` chat deltas instead of dropping them.
    #[serde(default)]
    pub keep_deltas: bool,
    /// Event types filtered out entirely before matching.
    #[serde(default)]
    pub ignore_types: Vec<String>,
    /// Per-binding directives (e.g. `{"grpc": "skip"}`), for adapters.
    #[serde(default)]
    pub bindings: BTreeMap<String, String>,
}

/// Recorded handler invocations, shared with the runner for the
/// `executions` assertion.
pub type ExecutionLog = Arc<Mutex<Vec<ExecutionExpectation>>>;

/// Scripted [`LlmProvider`]: one item list per invocation, in order.
/// Exhaustion is a loud scripting error, not a silent decline. Also the
/// FFI bindings' `{"provider": "scripted"}` implementation.
pub struct ScriptedLlm(Mutex<VecDeque<Vec<CompletionItem>>>);

impl ScriptedLlm {
    /// Builds a provider from scripted turns.
    #[must_use]
    pub fn new(turns: &[Vec<LlmItem>]) -> Self {
        let turns = turns
            .iter()
            .map(|items| items.iter().map(|item| item.clone().into()).collect())
            .collect();
        Self(Mutex::new(turns))
    }
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn complete(&self, _request: CompletionRequest) -> Result<Vec<CompletionItem>, LlmError> {
        self.0
            .lock()
            .expect("llm script lock")
            .pop_front()
            .ok_or_else(|| {
                LlmError::Provider(
                    "scripted llm exhausted — the script has fewer turns than the \
                     implementation requests (scripting error)"
                        .to_owned(),
                )
            })
    }
}

struct ScriptedAuthz {
    script: AuthzScript,
    calls: AtomicUsize,
}

#[async_trait]
impl AuthzResolver<VectorPrincipal> for ScriptedAuthz {
    async fn authorize(
        &self,
        _principal: &VectorPrincipal,
        _action_id: &str,
        _params: &JsonMap,
    ) -> AuthzDecision {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let behavior = match &self.script {
            AuthzScript::Default { default } => default,
            AuthzScript::Sequence { sequence, then } => sequence.get(call).unwrap_or(then),
        };
        match behavior {
            AuthzBehavior::Allow => AuthzDecision::Allow,
            AuthzBehavior::Deny(reason) => AuthzDecision::Deny(reason.clone()),
        }
    }
}

struct ScriptedHandler {
    action_id: String,
    script: HandlerScript,
    log: ExecutionLog,
}

#[async_trait]
impl ActionHandler<VectorPrincipal> for ScriptedHandler {
    async fn execute(
        &self,
        params: &JsonMap,
        _principal: &VectorPrincipal,
    ) -> Result<Value, HandlerError> {
        self.log
            .lock()
            .expect("execution log lock")
            .push(ExecutionExpectation {
                action_id: self.action_id.clone(),
                params: params.clone(),
            });
        match &self.script {
            HandlerScript::Return(value) => Ok(value.clone()),
            HandlerScript::Fail(message) => Err(HandlerError::Rejected(message.clone())),
        }
    }
}

struct ScriptedDryRun {
    script: DryRunScript,
    calls: AtomicUsize,
}

#[async_trait]
impl DryRunHandler<VectorPrincipal> for ScriptedDryRun {
    async fn dry_run(
        &self,
        _params: &JsonMap,
        _principal: &VectorPrincipal,
    ) -> Result<DryRunPreview, HandlerError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let behavior = match &self.script {
            DryRunScript::One(behavior) => behavior,
            DryRunScript::Sequence { sequence, then } => sequence.get(call).unwrap_or(then),
        };
        match behavior {
            DryRunBehavior::Preview(preview) => Ok(preview.clone()),
            DryRunBehavior::Fail(message) => Err(HandlerError::Rejected(message.clone())),
        }
    }
}

/// A [`SlotBuilder`] over a declarative slot → source table — used by
/// fixtures here and re-exported through `cleverhans-ffi` for hosts that
/// cannot register synchronous callbacks (Node).
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct DeclarativeSlots(pub BTreeMap<String, SlotScript>);

impl SlotBuilder for DeclarativeSlots {
    fn build(&self, params: &JsonMap, preview: Option<&DryRunPreview>) -> JsonMap {
        let mut slots = JsonMap::new();
        for (name, script) in &self.0 {
            let value = match script {
                SlotScript::Const(value) => Some(value.clone()),
                SlotScript::Param(param) => params.get(param).cloned(),
                SlotScript::Preview(field) => match field.as_str() {
                    "summary" => preview.and_then(|p| p.summary.clone()).map(Value::String),
                    _ => None,
                },
            };
            if let Some(value) = value {
                slots.insert(name.clone(), value);
            }
        }
        slots
    }
}

/// Assembles a runnable agent from a fixture and a vector's seam scripts.
///
/// # Panics
///
/// On fixture-authoring errors (action without a script entry, invalid
/// registry) — loud failure is the point.
#[must_use]
pub fn build_agent(
    fixture: &Fixture,
    vector: &Vector,
) -> (Arc<Agent<VectorPrincipal>>, ExecutionLog) {
    let log: ExecutionLog = Arc::new(Mutex::new(Vec::new()));
    let mut builder = RegistryBuilder::from_schema(fixture.registry.clone());
    for def in &fixture.registry.actions {
        let script = fixture
            .scripts
            .get(&def.id)
            .unwrap_or_else(|| panic!("fixture `{}` has no script for `{}`", fixture.name, def.id));
        builder = builder.attach(
            def.id.clone(),
            Arc::new(ScriptedHandler {
                action_id: def.id.clone(),
                script: script.handler.clone(),
                log: Arc::clone(&log),
            }),
            script.dry_run.clone().map(|dry_run| {
                Arc::new(ScriptedDryRun {
                    script: dry_run,
                    calls: AtomicUsize::new(0),
                }) as Arc<dyn DryRunHandler<VectorPrincipal>>
            }),
            script
                .slots
                .clone()
                .map(|slots| Arc::new(DeclarativeSlots(slots)) as Arc<dyn SlotBuilder>),
        );
    }
    let registry = builder.build().expect("fixture registry is valid");
    let agent = Agent::new(
        Arc::new(registry),
        Arc::new(ScriptedLlm::new(&vector.llm)),
        Arc::new(ScriptedAuthz {
            script: vector.authz.clone(),
            calls: AtomicUsize::new(0),
        }),
        Arc::new(
            fixture
                .registry
                .context_resolver()
                .expect("fixture registries map every context param"),
        ),
    );
    (Arc::new(agent), log)
}
