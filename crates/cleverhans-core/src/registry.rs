//! The action/block registry — the closed, app-owned contract (spec §4).
//!
//! Both sides reference the registry; neither owns the other. Handlers and
//! dry-runs live here backend-side but never cross the envelope.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::JsonMap;
use crate::error::{RegistryError, ValidationFailure};
use crate::seams::{ActionHandler, DryRunHandler, SlotBuilder, ToolDef};

/// Where a parameter's value comes from (spec §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamSource {
    /// Filled by the framework from the app context snapshot; never
    /// writable by the model.
    Context,
    /// Emitted by the model and validated against the schema.
    Utterance,
}

/// Value types for params and slots. Deliberately JSON-schema-lite; apps
/// needing richer validation do it in their handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    /// UTF-8 string.
    String,
    /// Whole number.
    Integer,
    /// Any JSON number.
    Number,
    /// Boolean.
    Boolean,
    /// One of a fixed set of strings.
    StringEnum(Vec<String>),
    /// Any JSON value; the app validates shape in its handler.
    Json,
}

impl ValueType {
    /// Checks a JSON value against this type.
    ///
    /// # Errors
    ///
    /// A human-readable mismatch description.
    pub fn check(&self, value: &serde_json::Value) -> Result<(), String> {
        let ok = match self {
            Self::String => value.is_string(),
            Self::Integer => value.is_i64() || value.is_u64(),
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::StringEnum(allowed) => value
                .as_str()
                .is_some_and(|s| allowed.iter().any(|a| a == s)),
            Self::Json => true,
        };
        if ok {
            Ok(())
        } else {
            Err(format!("expected {self:?}, got `{value}`"))
        }
    }

    fn json_schema(&self) -> serde_json::Value {
        match self {
            Self::String => json!({"type": "string"}),
            Self::Integer => json!({"type": "integer"}),
            Self::Number => json!({"type": "number"}),
            Self::Boolean => json!({"type": "boolean"}),
            Self::StringEnum(allowed) => json!({"type": "string", "enum": allowed}),
            Self::Json => json!({}),
        }
    }
}

/// One parameter of an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamSpec {
    /// Parameter name.
    pub name: String,
    /// Model-facing description (utterance params only).
    pub description: String,
    /// Value type.
    pub ty: ValueType,
    /// Where the value comes from.
    pub source: ParamSource,
    /// Whether the param must be present.
    pub required: bool,
}

/// One slot of a block type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotSpec {
    /// Slot name.
    pub name: String,
    /// Value type.
    pub ty: ValueType,
    /// Whether the slot must be filled.
    pub required: bool,
}

/// A registered UI block type with its slot schema (spec §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockDef {
    /// The closed-enum identifier the frontend routes on.
    pub block_type: String,
    /// Slot schema.
    pub slots: Vec<SlotSpec>,
}

impl BlockDef {
    /// Validates a slot map against this block's schema.
    ///
    /// # Errors
    ///
    /// [`ValidationFailure::InvalidSlot`] for missing required, unknown, or
    /// mistyped slots.
    pub fn check_slots(&self, slots: &JsonMap) -> Result<(), ValidationFailure> {
        for spec in &self.slots {
            match slots.get(&spec.name) {
                Some(value) => {
                    spec.ty
                        .check(value)
                        .map_err(|reason| ValidationFailure::InvalidSlot {
                            slot: spec.name.clone(),
                            reason,
                        })?;
                }
                None if spec.required => {
                    return Err(ValidationFailure::InvalidSlot {
                        slot: spec.name.clone(),
                        reason: "missing required slot".to_owned(),
                    });
                }
                None => {}
            }
        }
        if let Some(unknown) = slots
            .keys()
            .find(|k| !self.slots.iter().any(|s| &s.name == *k))
        {
            return Err(ValidationFailure::InvalidSlot {
                slot: unknown.clone(),
                reason: "unknown slot".to_owned(),
            });
        }
        Ok(())
    }
}

/// The wire-visible metadata of one action (spec §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionDef {
    /// Inert, hand-authored, opaque key (spec §3).
    pub id: String,
    /// Intent-matching surface presented to the model.
    pub description: String,
    /// Parameter schema.
    pub params: Vec<ParamSpec>,
    /// Which registered block renders proposals for this action.
    pub block_type: String,
    /// Forces dry-run + explicit confirmation.
    pub mutates: bool,
    /// Opaque permission key handed to the app's authz resolver.
    pub authz_key: String,
}

/// A registered action: wire-visible definition plus backend-private
/// handlers that never cross the envelope.
pub struct ActionRegistration<P> {
    /// Wire-visible metadata.
    pub def: ActionDef,
    /// The app's execution path.
    pub handler: Arc<dyn ActionHandler<P>>,
    /// Preview computation; present iff `def.mutates` (enforced at build).
    pub dry_run: Option<Arc<dyn DryRunHandler<P>>>,
    /// Slot content builder; `None` renders empty slots. Fixed cards use
    /// [`static_slots`](crate::seams::static_slots); param-aware cards pass
    /// a closure (see [`SlotBuilder`]).
    pub slot_builder: Option<Arc<dyn SlotBuilder>>,
}

/// The closed action/block enumeration. Built once via [`RegistryBuilder`],
/// then read-only. `BTreeMap` keeps tool-list order deterministic for evals.
pub struct Registry<P> {
    actions: BTreeMap<String, ActionRegistration<P>>,
    blocks: BTreeMap<String, BlockDef>,
}

impl<P> Registry<P> {
    /// Starts an empty builder.
    #[must_use]
    pub fn builder() -> RegistryBuilder<P> {
        RegistryBuilder {
            actions: Vec::new(),
            blocks: Vec::new(),
        }
    }

    /// Looks up an action registration.
    #[must_use]
    pub fn action(&self, id: &str) -> Option<&ActionRegistration<P>> {
        self.actions.get(id)
    }

    /// Looks up a block definition.
    #[must_use]
    pub fn block(&self, block_type: &str) -> Option<&BlockDef> {
        self.blocks.get(block_type)
    }

    /// All registered action definitions, in deterministic (ID) order.
    /// Wire-visible metadata only — handlers stay private. This is the
    /// codegen input (spec §9: one source, three consumers).
    pub fn action_defs(&self) -> impl Iterator<Item = &ActionDef> {
        self.actions.values().map(|reg| &reg.def)
    }

    /// All registered block definitions, in deterministic order.
    pub fn block_defs(&self) -> impl Iterator<Item = &BlockDef> {
        self.blocks.values()
    }

    /// The registry as model-facing tool definitions. Only utterance-sourced
    /// params are exposed — the model never sees context-sourced ones
    /// (spec §4.1).
    #[must_use]
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        self.actions
            .values()
            .map(|reg| {
                let mut properties = JsonMap::new();
                let mut required = Vec::new();
                for param in reg
                    .def
                    .params
                    .iter()
                    .filter(|p| p.source == ParamSource::Utterance)
                {
                    let mut schema = param.ty.json_schema();
                    if let Some(obj) = schema.as_object_mut() {
                        obj.insert("description".to_owned(), json!(param.description));
                    }
                    properties.insert(param.name.clone(), schema);
                    if param.required {
                        required.push(param.name.clone());
                    }
                }
                ToolDef {
                    name: reg.def.id.clone(),
                    description: reg.def.description.clone(),
                    parameters: json!({
                        "type": "object",
                        "properties": properties,
                        "required": required,
                    }),
                }
            })
            .collect()
    }
}

/// Accumulates registrations, then validates the whole contract at
/// [`RegistryBuilder::build`].
pub struct RegistryBuilder<P> {
    actions: Vec<ActionRegistration<P>>,
    blocks: Vec<BlockDef>,
}

impl<P> RegistryBuilder<P> {
    /// Registers a block type.
    #[must_use]
    pub fn block(mut self, def: BlockDef) -> Self {
        self.blocks.push(def);
        self
    }

    /// Registers an action with its handlers.
    #[must_use]
    pub fn action(
        mut self,
        def: ActionDef,
        handler: Arc<dyn ActionHandler<P>>,
        dry_run: Option<Arc<dyn DryRunHandler<P>>>,
        slot_builder: Option<Arc<dyn SlotBuilder>>,
    ) -> Self {
        self.actions.push(ActionRegistration {
            def,
            handler,
            dry_run,
            slot_builder,
        });
        self
    }

    /// Validates and freezes the registry.
    ///
    /// # Errors
    ///
    /// [`RegistryError`] on duplicate IDs, references to unregistered block
    /// types, or mutating actions without a dry-run handler.
    pub fn build(self) -> Result<Registry<P>, RegistryError> {
        let mut blocks = BTreeMap::new();
        for def in self.blocks {
            let block_type = def.block_type.clone();
            if blocks.insert(block_type.clone(), def).is_some() {
                return Err(RegistryError::DuplicateBlock(block_type));
            }
        }
        let mut actions = BTreeMap::new();
        for reg in self.actions {
            let id = reg.def.id.clone();
            if !blocks.contains_key(&reg.def.block_type) {
                return Err(RegistryError::UnknownBlockType {
                    action_id: id,
                    block_type: reg.def.block_type.clone(),
                });
            }
            if reg.def.mutates && reg.dry_run.is_none() {
                return Err(RegistryError::MissingDryRun(id));
            }
            if actions.insert(id.clone(), reg).is_some() {
                return Err(RegistryError::DuplicateAction(id));
            }
        }
        Ok(Registry { actions, blocks })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::HandlerError;
    use async_trait::async_trait;

    struct NoopHandler;

    #[async_trait]
    impl ActionHandler<()> for NoopHandler {
        async fn execute(
            &self,
            _params: &JsonMap,
            _principal: &(),
        ) -> Result<serde_json::Value, HandlerError> {
            Ok(serde_json::Value::Null)
        }
    }

    fn confirm_block() -> BlockDef {
        BlockDef {
            block_type: "confirm".to_owned(),
            slots: vec![SlotSpec {
                name: "title".to_owned(),
                ty: ValueType::String,
                required: true,
            }],
        }
    }

    fn action(id: &str, mutates: bool) -> ActionDef {
        ActionDef {
            id: id.to_owned(),
            description: "test action".to_owned(),
            params: vec![
                ParamSpec {
                    name: "recordId".to_owned(),
                    description: String::new(),
                    ty: ValueType::String,
                    source: ParamSource::Context,
                    required: true,
                },
                ParamSpec {
                    name: "country".to_owned(),
                    description: "ISO country code".to_owned(),
                    ty: ValueType::String,
                    source: ParamSource::Utterance,
                    required: true,
                },
            ],
            block_type: "confirm".to_owned(),
            mutates,
            authz_key: "test".to_owned(),
        }
    }

    mod build {
        use super::*;

        #[test]
        fn rejects_duplicate_action_ids() {
            let result = Registry::<()>::builder()
                .block(confirm_block())
                .action(action("a.b", false), Arc::new(NoopHandler), None, None)
                .action(action("a.b", false), Arc::new(NoopHandler), None, None)
                .build();

            assert!(matches!(result, Err(RegistryError::DuplicateAction(id)) if id == "a.b"));
        }

        #[test]
        fn rejects_unregistered_block_type() {
            let result = Registry::<()>::builder()
                .action(action("a.b", false), Arc::new(NoopHandler), None, None)
                .build();

            assert!(matches!(
                result,
                Err(RegistryError::UnknownBlockType { .. })
            ));
        }

        #[test]
        fn rejects_mutating_action_without_dry_run() {
            let result = Registry::<()>::builder()
                .block(confirm_block())
                .action(action("a.b", true), Arc::new(NoopHandler), None, None)
                .build();

            assert!(matches!(result, Err(RegistryError::MissingDryRun(id)) if id == "a.b"));
        }
    }

    mod tool_defs {
        use super::*;

        #[test]
        fn exposes_only_utterance_params() {
            let registry = Registry::<()>::builder()
                .block(confirm_block())
                .action(action("a.b", false), Arc::new(NoopHandler), None, None)
                .build()
                .expect("valid registry");

            let tools = registry.tool_defs();

            let properties = tools[0].parameters["properties"]
                .as_object()
                .expect("object schema");
            assert!(
                properties.contains_key("country") && !properties.contains_key("recordId"),
                "context param leaked into tool schema: {properties:?}"
            );
        }
    }

    mod check_slots {
        use super::*;

        #[test]
        fn rejects_missing_required_slot() {
            let result = confirm_block().check_slots(&JsonMap::new());

            assert!(matches!(
                result,
                Err(ValidationFailure::InvalidSlot { slot, .. }) if slot == "title"
            ));
        }

        #[test]
        fn rejects_unknown_slot() {
            let mut slots = JsonMap::new();
            slots.insert("title".to_owned(), serde_json::json!("ok"));
            slots.insert("bogus".to_owned(), serde_json::json!(1));

            let result = confirm_block().check_slots(&slots);

            assert!(matches!(
                result,
                Err(ValidationFailure::InvalidSlot { slot, .. }) if slot == "bogus"
            ));
        }
    }
}
