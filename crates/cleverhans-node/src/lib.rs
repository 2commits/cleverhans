//! napi-rs binding: host a CleverHans agent from Node.js.
//!
//! The generated binding is wrapped by `typescript/cleverhans-node/src/`,
//! which owns the ergonomic surface: it normalizes registry/config inputs to
//! JSON strings, wraps host callbacks so synchronous throws become
//! rejections, and converts the `Rejected` business-error class into the
//! sentinel object this crate recognizes (`{"__cleverhans_rejected": msg}` —
//! error-by-value survives the threadsafe-function boundary losslessly,
//! error-by-message does not).

use std::collections::HashMap;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use serde_json::Value;
use tokio::sync::Mutex;

use cleverhans_core::JsonMap;
use cleverhans_core::agent::{Agent as CoreAgent, AgentConfig};
use cleverhans_core::envelope::DryRunPreview;
use cleverhans_core::error::{HandlerError, LlmError};
use cleverhans_core::schema::RegistrySchema;
use cleverhans_core::seams::{
    ActionHandler, AuthzDecision, AuthzResolver, CompletionItem, CompletionRequest, DryRunHandler,
    LlmProvider,
};
use cleverhans_ffi::{
    DeclarativeSlots, FfiPrincipal, FramePump, LlmSpec, assemble_registry, build_llm,
};

/// A host callback receiving `(payload_a, payload_b)` JSON values and
/// resolving to a JSON value. The TS wrapper guarantees the callback is
/// async (sync throws become rejections there).
type HostFn = ThreadsafeFunction<
    FnArgs<(Value, Value)>,
    Promise<Value>,
    FnArgs<(Value, Value)>,
    Status,
    false,
>;
/// Authorization callback: `(principal, action_id, params)`.
type AuthzFn = ThreadsafeFunction<
    FnArgs<(Value, String, Value)>,
    Promise<Value>,
    FnArgs<(Value, String, Value)>,
    Status,
    false,
>;
/// Per-event emitter for one turn's outbound frames.
type EmitFn = ThreadsafeFunction<String, (), String, Status, false>;

const OK_KEY: &str = "__cleverhans_ok";
const REJECTED_KEY: &str = "__cleverhans_rejected";

/// Unwraps the wrapper's total result envelope: every host callback result
/// arrives as `{"__cleverhans_ok": value}` or
/// `{"__cleverhans_rejected": message}`. Because the encoding is total, a
/// legitimate handler result can never collide with the rejection marker.
fn handler_outcome(value: Value) -> std::result::Result<Value, HandlerError> {
    let Value::Object(mut map) = value else {
        return Err(HandlerError::Internal(
            "host wrapper protocol violation: result is not an envelope".to_owned(),
        ));
    };
    if map.len() != 1 {
        return Err(HandlerError::Internal(
            "host wrapper protocol violation: envelope must have exactly one key".to_owned(),
        ));
    }
    if let Some(ok) = map.remove(OK_KEY) {
        return Ok(ok);
    }
    match map.remove(REJECTED_KEY) {
        Some(Value::String(message)) => Err(HandlerError::Rejected(message)),
        _ => Err(HandlerError::Internal(
            "host wrapper protocol violation: unknown envelope key".to_owned(),
        )),
    }
}

async fn call_host(
    callable: &HostFn,
    a: Value,
    b: Value,
) -> std::result::Result<Value, HandlerError> {
    let promise = callable
        .call_async((a, b).into())
        .await
        .map_err(|err| HandlerError::Internal(err.to_string()))?;
    let value = promise
        .await
        .map_err(|err| HandlerError::Internal(err.to_string()))?;
    handler_outcome(value)
}

struct JsHandler(HostFn);

#[async_trait::async_trait]
impl ActionHandler<FfiPrincipal> for JsHandler {
    async fn execute(
        &self,
        params: &JsonMap,
        principal: &FfiPrincipal,
    ) -> std::result::Result<Value, HandlerError> {
        call_host(&self.0, Value::Object(params.clone()), principal.clone()).await
    }
}

struct JsDryRun(HostFn);

#[async_trait::async_trait]
impl DryRunHandler<FfiPrincipal> for JsDryRun {
    async fn dry_run(
        &self,
        params: &JsonMap,
        principal: &FfiPrincipal,
    ) -> std::result::Result<DryRunPreview, HandlerError> {
        let value = call_host(&self.0, Value::Object(params.clone()), principal.clone()).await?;
        serde_json::from_value(value)
            .map_err(|err| HandlerError::Internal(format!("dryRun returned a bad preview: {err}")))
    }
}

// Slot builders are synchronous in the core seam, and Node has no safe way
// to call back onto the JS thread synchronously from a tokio worker (Python
// can — the GIL makes that call safe). v1 therefore supports slot content
// only through the app-authored sources that need no callback: static slots
// are simply absent here, and the dry-run preview carries the human-readable
// text (spec §8 keeps slot content app-authored either way).

/// `null`/`true` → allow, string → deny with reason; rejection → deny
/// (fail closed). The TS wrapper converts thrown errors into rejections.
struct JsAuthz(AuthzFn);

#[async_trait::async_trait]
impl AuthzResolver<FfiPrincipal> for JsAuthz {
    async fn authorize(
        &self,
        principal: &FfiPrincipal,
        action_id: &str,
        params: &JsonMap,
    ) -> AuthzDecision {
        let outcome = async {
            self.0
                .call_async(
                    (
                        principal.clone(),
                        action_id.to_owned(),
                        Value::Object(params.clone()),
                    )
                        .into(),
                )
                .await?
                .await
        }
        .await;
        match outcome {
            Ok(Value::Null) | Ok(Value::Bool(true)) => AuthzDecision::Allow,
            Ok(Value::String(reason)) => AuthzDecision::Deny(reason),
            Ok(Value::Bool(false)) => AuthzDecision::Deny("denied".to_owned()),
            Ok(other) => AuthzDecision::Deny(format!("authorize returned a non-decision: {other}")),
            Err(err) => AuthzDecision::Deny(format!("authorize raised: {err}")),
        }
    }
}

struct AllowAll;

#[async_trait::async_trait]
impl AuthzResolver<FfiPrincipal> for AllowAll {
    async fn authorize(
        &self,
        _principal: &FfiPrincipal,
        _action_id: &str,
        _params: &JsonMap,
    ) -> AuthzDecision {
        AuthzDecision::Allow
    }
}

/// Custom LLM callback: `(request)` resolving to the neutral item list.
struct JsLlm(HostFn);

#[async_trait::async_trait]
impl LlmProvider for JsLlm {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> std::result::Result<Vec<CompletionItem>, LlmError> {
        let request =
            serde_json::to_value(&request).map_err(|err| LlmError::Provider(err.to_string()))?;
        let value = call_host(&self.0, request, Value::Null)
            .await
            .map_err(|err| LlmError::Provider(err.to_string()))?;
        let items: Vec<cleverhans_ffi::LlmItem> = serde_json::from_value(value)
            .map_err(|err| LlmError::Provider(format!("bad llm items: {err}")))?;
        Ok(items.into_iter().map(Into::into).collect())
    }
}

fn invalid(err: impl std::fmt::Display) -> Error {
    Error::new(Status::InvalidArg, err.to_string())
}

/// Generates a typed module (`typescript` | `python` | `rust`) from a
/// registry document — codegen without a Rust toolchain. The TS wrapper and
/// the `cleverhans-codegen` bin script front this.
#[napi]
pub fn generate_types(registry_json: String, target: String) -> Result<String> {
    cleverhans_ffi::generate_types(&registry_json, &target).map_err(invalid)
}

/// The agent, stateless and shared across sessions.
#[napi]
pub struct Agent {
    inner: Arc<CoreAgent<FfiPrincipal>>,
}

#[napi]
impl Agent {
    /// Constructs an agent. Called by the TS wrapper, which normalizes the
    /// ergonomic surface down to these arguments.
    #[napi(constructor)]
    #[allow(clippy::too_many_arguments)] // flat FFI surface, normalized by the wrapper
    pub fn new(
        registry_json: String,
        handlers: HashMap<String, HostFn>,
        dry_runs: HashMap<String, HostFn>,
        slot_builders: HashMap<String, Value>,
        authorize: Option<AuthzFn>,
        llm_spec_json: Option<String>,
        llm_callable: Option<HostFn>,
        config_json: Option<String>,
    ) -> Result<Self> {
        let schema = RegistrySchema::from_json(&registry_json).map_err(invalid)?;

        let handlers = handlers
            .into_iter()
            .map(|(id, callable)| {
                (
                    id,
                    Arc::new(JsHandler(callable)) as Arc<dyn ActionHandler<FfiPrincipal>>,
                )
            })
            .collect();
        let dry_runs = dry_runs
            .into_iter()
            .map(|(id, callable)| {
                (
                    id,
                    Arc::new(JsDryRun(callable)) as Arc<dyn DryRunHandler<FfiPrincipal>>,
                )
            })
            .collect();

        let authz: Arc<dyn AuthzResolver<FfiPrincipal>> = match authorize {
            Some(callable) => Arc::new(JsAuthz(callable)),
            None => Arc::new(AllowAll),
        };
        let llm: Arc<dyn LlmProvider> = match (llm_spec_json, llm_callable) {
            (Some(spec), None) => {
                let spec: LlmSpec = serde_json::from_str(&spec).map_err(invalid)?;
                build_llm(spec).map_err(invalid)?
            }
            (None, Some(callable)) => Arc::new(JsLlm(callable)),
            _ => return Err(invalid("provide exactly one of llm spec or llm callable")),
        };
        let config: AgentConfig = match config_json {
            Some(json) => cleverhans_ffi::parse_agent_config(&json).map_err(invalid)?,
            None => AgentConfig::default(),
        };

        let slot_builders = slot_builders
            .into_iter()
            .map(|(id, table)| {
                let slots: DeclarativeSlots = serde_json::from_value(table)
                    .map_err(|err| invalid(format!("slotBuilders[{id}]: {err}")))?;
                Ok((
                    id,
                    Arc::new(slots) as Arc<dyn cleverhans_core::seams::SlotBuilder>,
                ))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        let context_resolver = schema
            .context_resolver()
            .map_err(|err| invalid(err.to_string()))?;
        let registry =
            assemble_registry(schema, handlers, dry_runs, slot_builders).map_err(invalid)?;
        Ok(Self {
            inner: Arc::new(CoreAgent::with_config(
                Arc::new(registry),
                llm,
                authz,
                Arc::new(context_resolver),
                config,
            )),
        })
    }

    /// Opens a session bound to a principal (any JSON identity blob).
    #[napi]
    pub fn session(&self, principal: Value) -> Session {
        Session {
            agent: Arc::clone(&self.inner),
            pump: Arc::new(Mutex::new(FramePump::new(principal))),
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

/// One envelope session; frames in, frames out.
#[napi]
pub struct Session {
    agent: Arc<CoreAgent<FfiPrincipal>>,
    pump: Arc<Mutex<FramePump<FfiPrincipal>>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

#[napi]
impl Session {
    /// Handles one inbound JSON frame; `onEvent` fires once per outbound
    /// JSON frame as it becomes available (chat deltas stream live). The
    /// returned promise resolves when the turn completes; it resolves `true`
    /// when the session is closed (init-first violation, spec §6.1), after
    /// which the host should close its transport.
    #[napi]
    pub async fn handle(&self, frame: String, on_event: EmitFn) -> Result<bool> {
        let agent = Arc::clone(&self.agent);
        let pump = Arc::clone(&self.pump);
        let mut pump = pump.lock().await;
        let outcome = pump
            .handle_frame(&agent, &frame, &mut |json: String| {
                on_event.call(json, ThreadsafeFunctionCallMode::NonBlocking);
                true
            })
            .await;
        if outcome == cleverhans_ffi::FrameOutcome::Closed {
            self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(pump.is_closed())
    }

    /// Whether an init-first violation has closed the session. Once true,
    /// further frames emit no events; close your transport.
    #[napi(getter)]
    pub fn closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }
}
