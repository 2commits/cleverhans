//! Shared layer under the PyO3 and napi-rs bindings — write once, bind
//! twice. Everything language-specific (GIL, threadsafe functions) stays in
//! the binding crates; everything protocol-shaped lives here or is
//! re-exported from its single home:
//!
//! - [`FfiPrincipal`] — the concrete principal at the FFI boundary
//! - [`FramePump`]/[`FrameOutcome`] — the per-frame session state machine
//!   (re-exported from `cleverhans-ws-core`, the one implementation of the
//!   spec §6.1 wire behavior)
//! - [`LlmSpec`]/[`build_llm`] — declarative provider selection
//! - [`assemble_registry`] — schema + host handler maps, with coverage
//!   errors that name the missing callback
//! - [`parse_agent_config`] — the host-facing config document
//! - [`LlmItem`]/[`DeclarativeSlots`] — neutral encodings shared with the
//!   conformance fixtures (re-exported from `cleverhans-conformance`)

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;

use cleverhans_core::agent::AgentConfig;
use cleverhans_core::registry::{Registry, RegistryBuilder};
use cleverhans_core::schema::{RegistrySchema, SchemaError};
use cleverhans_core::seams::{ActionHandler, DryRunHandler, LlmProvider, SlotBuilder};

pub use cleverhans::llm::LlmSpec;
pub use cleverhans_conformance::fixture::{DeclarativeSlots, LlmItem, ScriptedLlm, SlotScript};
pub use cleverhans_ws_core::{EventSink, FrameOutcome, FramePump};

/// The one principal type at the FFI boundary: an app-defined JSON identity
/// blob (user id, org, roles, …). The framework never introspects it; seam
/// callbacks receive it back verbatim. Live state (DB pools, request
/// clients) belongs in the handler closures, not the principal.
pub type FfiPrincipal = serde_json::Value;

/// Construction-time errors surfaced to the host as thrown exceptions.
#[derive(Debug, thiserror::Error)]
pub enum FfiError {
    /// The registry document failed to parse or version-gate.
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// The registry document + handler maps are inconsistent.
    #[error("{0}")]
    Registry(String),
    /// The LLM spec is invalid.
    #[error("llm spec: {0}")]
    Llm(String),
}

/// Builds a provider from a spec (single home: `cleverhans::llm`). The
/// `Result` wrapper is kept for binding-crate call-site stability.
///
/// # Errors
///
/// None today; reserved for spec content validation.
pub fn build_llm(spec: LlmSpec) -> Result<Arc<dyn LlmProvider>, FfiError> {
    Ok(cleverhans::llm::build_llm(spec))
}

/// Parses the host-facing agent-config document (`app_instructions`,
/// `max_validation_retries`, `describe_context`), filling unset fields from
/// [`AgentConfig::default`]. One definition for every binding — a knob added
/// here is a knob added everywhere.
///
/// # Errors
///
/// [`FfiError::Registry`]-style parse error via `serde_json`.
pub fn parse_agent_config(json: &str) -> Result<AgentConfig, serde_json::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ConfigDoc {
        app_instructions: Option<String>,
        max_validation_retries: Option<u8>,
        describe_context: Option<bool>,
    }
    let doc: ConfigDoc = serde_json::from_str(json)?;
    let defaults = AgentConfig::default();
    Ok(AgentConfig {
        app_instructions: doc.app_instructions,
        max_validation_retries: doc
            .max_validation_retries
            .unwrap_or(defaults.max_validation_retries),
        describe_context: doc.describe_context.unwrap_or(defaults.describe_context),
    })
}

/// Generates a typed module from a registry document — the codegen CLI as a
/// host-language call, so npm/PyPI consumers with prebuilt binaries never
/// need a Rust toolchain to regenerate types. `target` is
/// `typescript`/`ts`, `python`/`py`, or `rust`/`rs`.
///
/// # Errors
///
/// A human-readable message for a malformed document or unknown target.
pub fn generate_types(schema_json: &str, target: &str) -> Result<String, String> {
    let schema = RegistrySchema::from_json(schema_json).map_err(|err| err.to_string())?;
    match target {
        "typescript" | "ts" => Ok(cleverhans_codegen::typescript_module(
            &schema.actions,
            &schema.blocks,
        )),
        "python" | "py" => Ok(cleverhans_codegen::python_module(
            &schema.actions,
            &schema.blocks,
        )),
        "rust" | "rs" => Ok(cleverhans_codegen::rust_module(
            &schema.actions,
            &schema.blocks,
        )),
        other => Err(format!(
            "unknown codegen target `{other}` (typescript | python | rust)"
        )),
    }
}

/// Assembles a registry from a declarative document plus host-language seam
/// callbacks, validating coverage with errors that name the offending
/// callback before the builder's own invariants run.
///
/// # Errors
///
/// [`FfiError::Registry`] naming the offending action or map key.
pub fn assemble_registry(
    schema: RegistrySchema,
    mut handlers: HashMap<String, Arc<dyn ActionHandler<FfiPrincipal>>>,
    mut dry_runs: HashMap<String, Arc<dyn DryRunHandler<FfiPrincipal>>>,
    mut slot_builders: HashMap<String, Arc<dyn SlotBuilder>>,
) -> Result<Registry<FfiPrincipal>, FfiError> {
    let mut builder = RegistryBuilder::from_schema(schema.clone());
    for def in &schema.actions {
        let handler = handlers.remove(&def.id).ok_or_else(|| {
            FfiError::Registry(format!("no handler registered for action `{}`", def.id))
        })?;
        if !def.mutates && dry_runs.contains_key(&def.id) {
            return Err(FfiError::Registry(format!(
                "action `{}` does not mutate; its dry_run would never be called \
                 (set mutates: true or remove the dry_run)",
                def.id
            )));
        }
        let dry_run = dry_runs.remove(&def.id);
        if def.mutates && dry_run.is_none() {
            return Err(FfiError::Registry(format!(
                "action `{}` mutates but has no dry_run registered",
                def.id
            )));
        }
        builder = builder.attach(
            def.id.clone(),
            handler,
            dry_run,
            slot_builders.remove(&def.id),
        );
    }
    for (maps, name) in [
        (handlers.keys().next(), "handlers"),
        (dry_runs.keys().next(), "dry_runs"),
        (slot_builders.keys().next(), "slot_builders"),
    ] {
        if let Some(unknown) = maps {
            return Err(FfiError::Registry(format!(
                "{name} entry `{unknown}` matches no action in the registry document"
            )));
        }
    }
    builder
        .build()
        .map_err(|err| FfiError::Registry(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleverhans_core::JsonMap;
    use cleverhans_core::agent::Agent;
    use cleverhans_core::error::HandlerError;
    use cleverhans_core::seams::{AuthzDecision, AuthzResolver};
    use serde_json::json;

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

    struct Ok200;

    #[async_trait::async_trait]
    impl ActionHandler<FfiPrincipal> for Ok200 {
        async fn execute(
            &self,
            _params: &JsonMap,
            principal: &FfiPrincipal,
        ) -> Result<serde_json::Value, HandlerError> {
            Ok(json!({"as": principal["user_id"]}))
        }
    }

    struct OnePreview;

    #[async_trait::async_trait]
    impl DryRunHandler<FfiPrincipal> for OnePreview {
        async fn dry_run(
            &self,
            _params: &JsonMap,
            _principal: &FfiPrincipal,
        ) -> Result<cleverhans_core::envelope::DryRunPreview, HandlerError> {
            Ok(cleverhans_core::envelope::DryRunPreview {
                affected_count: 1,
                ..Default::default()
            })
        }
    }

    fn schema_json(mutates: bool) -> String {
        json!({
            "spec_version": "0.1",
            "blocks": [{"block_type": "confirm",
                        "slots": [{"name": "title", "type": "string", "required": false}]}],
            "actions": [{
                "id": "record.touch",
                "description": "Touch the selected record",
                "params": [{"name": "recordId", "type": "string",
                            "source": "context", "required": true}],
                "block_type": "confirm",
                "mutates": mutates,
                "authz_key": "record.touch"
            }],
            "context_params": {"recordId": "selected_record_id"}
        })
        .to_string()
    }

    fn schema() -> RegistrySchema {
        RegistrySchema::from_json(&schema_json(true)).expect("valid schema")
    }

    fn agent() -> Agent<FfiPrincipal> {
        let registry = assemble_registry(
            schema(),
            HashMap::from([(
                "record.touch".to_owned(),
                Arc::new(Ok200) as Arc<dyn ActionHandler<FfiPrincipal>>,
            )]),
            HashMap::from([(
                "record.touch".to_owned(),
                Arc::new(OnePreview) as Arc<dyn DryRunHandler<FfiPrincipal>>,
            )]),
            HashMap::new(),
        )
        .expect("registry assembles");
        let llm = build_llm(LlmSpec::Scripted {
            script: vec![vec![LlmItem::ToolCall {
                name: "record.touch".to_owned(),
                arguments: JsonMap::new(),
            }]],
        })
        .expect("scripted llm");
        Agent::new(
            Arc::new(registry),
            llm,
            Arc::new(AllowAll),
            Arc::new(schema().context_resolver().expect("mapped context params")),
        )
    }

    async fn pump_frames(frames: &[serde_json::Value]) -> Vec<serde_json::Value> {
        let agent = agent();
        let mut pump = FramePump::new(json!({"user_id": "alex"}));
        let mut out = Vec::new();
        for frame in frames {
            pump.handle_frame(&agent, &frame.to_string(), &mut |json: String| {
                out.push(serde_json::from_str(&json).expect("outbound JSON"));
                true
            })
            .await;
        }
        out
    }

    #[tokio::test]
    async fn full_flow_executes_under_the_json_principal() {
        let events = pump_frames(&[
            json!({"type": "init", "spec_version": "0.1.0-draft",
                   "context": {"route": "/records/r-1", "selected_record_id": "r-1"}}),
            json!({"type": "user_message", "text": "touch it", "client_msg_id": "c-1"}),
            json!({"type": "confirm_action", "proposal_id": "prop-1"}),
        ])
        .await;

        assert_eq!(events[0]["type"], "action_proposal", "got {events:?}");
        assert_eq!(events[1]["state"], "executed");
        assert_eq!(
            events[1]["result"],
            json!({"as": "alex"}),
            "handler saw the JSON principal"
        );
    }

    #[tokio::test]
    async fn non_init_first_frame_closes_the_pump() {
        let agent = agent();
        let mut pump = FramePump::new(json!({}));
        let mut out: Vec<String> = Vec::new();

        let outcome = pump
            .handle_frame(
                &agent,
                &json!({"type": "user_message", "text": "hi", "client_msg_id": "c-1"}).to_string(),
                &mut |json: String| {
                    out.push(json);
                    true
                },
            )
            .await;

        assert_eq!(outcome, FrameOutcome::Closed);
        assert!(pump.is_closed());
        assert!(out[0].contains("init_required"), "got {out:?}");

        // Post-close frames emit nothing (the host should have closed).
        let post = pump
            .handle_frame(&agent, "{}", &mut |json: String| {
                out.push(json);
                true
            })
            .await;
        assert_eq!(post, FrameOutcome::Closed);
        assert_eq!(out.len(), 1, "no events after close: {out:?}");
    }

    #[tokio::test]
    async fn malformed_frame_is_recoverable() {
        let agent = agent();
        let mut pump = FramePump::new(json!({}));
        let mut out: Vec<String> = Vec::new();

        let outcome = pump
            .handle_frame(&agent, "not json", &mut |json: String| {
                out.push(json);
                true
            })
            .await;

        assert_eq!(outcome, FrameOutcome::Continue);
        assert!(out[0].contains("malformed_event"));
    }

    #[test]
    fn assemble_registry_names_missing_callbacks() {
        let missing_handler =
            assemble_registry(schema(), HashMap::new(), HashMap::new(), HashMap::new())
                .err()
                .expect("missing handler must fail");
        assert!(
            missing_handler.to_string().contains("record.touch"),
            "got {missing_handler}"
        );

        let missing_dry_run = assemble_registry(
            schema(),
            HashMap::from([(
                "record.touch".to_owned(),
                Arc::new(Ok200) as Arc<dyn ActionHandler<FfiPrincipal>>,
            )]),
            HashMap::new(),
            HashMap::new(),
        )
        .err()
        .expect("missing dry_run must fail");
        assert!(
            missing_dry_run.to_string().contains("dry_run"),
            "got {missing_dry_run}"
        );
    }

    #[test]
    fn dry_run_for_non_mutating_action_is_rejected() {
        let result = assemble_registry(
            RegistrySchema::from_json(&schema_json(false)).expect("valid schema"),
            HashMap::from([(
                "record.touch".to_owned(),
                Arc::new(Ok200) as Arc<dyn ActionHandler<FfiPrincipal>>,
            )]),
            HashMap::from([(
                "record.touch".to_owned(),
                Arc::new(OnePreview) as Arc<dyn DryRunHandler<FfiPrincipal>>,
            )]),
            HashMap::new(),
        )
        .err()
        .expect("dry_run on a non-mutating action must fail loudly");

        assert!(
            result.to_string().contains("does not mutate"),
            "got {result}"
        );
    }
}
