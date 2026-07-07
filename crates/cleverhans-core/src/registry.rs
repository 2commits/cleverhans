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
#[serde(deny_unknown_fields)]
pub struct ParamSpec {
    /// Parameter name.
    pub name: String,
    /// Model-facing description (utterance params only).
    #[serde(default)]
    pub description: String,
    /// Value type.
    #[serde(rename = "type")]
    pub ty: ValueType,
    /// Where the value comes from.
    pub source: ParamSource,
    /// Whether the param must be present.
    pub required: bool,
}

/// One slot of a block type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotSpec {
    /// Slot name.
    pub name: String,
    /// Value type.
    #[serde(rename = "type")]
    pub ty: ValueType,
    /// Whether the slot must be filled.
    pub required: bool,
}

/// A registered UI block type with its slot schema (spec §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
    context_params: BTreeMap<String, String>,
}

impl<P> Registry<P> {
    /// Starts an empty builder.
    #[must_use]
    pub fn builder() -> RegistryBuilder<P> {
        RegistryBuilder {
            actions: Vec::new(),
            blocks: Vec::new(),
            pending: Vec::new(),
            attachments: Vec::new(),
            context_params: BTreeMap::new(),
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

    /// The wire-visible registry as a versioned document — ID-sorted,
    /// `spec_version` = [`crate::SPEC_VERSION`]. The inverse of
    /// [`RegistryBuilder::from_schema`] modulo handlers.
    #[must_use]
    pub fn schema(&self) -> crate::schema::RegistrySchema {
        crate::schema::RegistrySchema {
            spec_version: crate::SPEC_VERSION.to_owned(),
            blocks: self.blocks.values().cloned().collect(),
            actions: self.actions.values().map(|reg| reg.def.clone()).collect(),
            context_params: self.context_params.clone(),
        }
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
    pending: Vec<ActionDef>,
    attachments: Vec<Attachment<P>>,
    context_params: BTreeMap<String, String>,
}

struct Attachment<P> {
    id: String,
    handler: Arc<dyn ActionHandler<P>>,
    dry_run: Option<Arc<dyn DryRunHandler<P>>>,
    slot_builder: Option<Arc<dyn SlotBuilder>>,
}

impl<P> RegistryBuilder<P> {
    /// Seeds a builder from a declarative document (spec §4): blocks are
    /// registered, action defs stay pending until [`RegistryBuilder::attach`]
    /// binds their handlers. Mixing with programmatic
    /// [`RegistryBuilder::block`]/[`RegistryBuilder::action`] calls is fine.
    #[must_use]
    pub fn from_schema(schema: crate::schema::RegistrySchema) -> Self {
        let mut builder = Registry::builder();
        builder.blocks = schema.blocks;
        builder.pending = schema.actions;
        builder.context_params = schema.context_params;
        builder
    }

    /// Binds handlers to a pending schema def by action ID. Infallible here;
    /// unknown, duplicate, or never-attached IDs are reported at
    /// [`RegistryBuilder::build`].
    #[must_use]
    pub fn attach(
        mut self,
        id: impl Into<String>,
        handler: Arc<dyn ActionHandler<P>>,
        dry_run: Option<Arc<dyn DryRunHandler<P>>>,
        slot_builder: Option<Arc<dyn SlotBuilder>>,
    ) -> Self {
        self.attachments.push(Attachment {
            id: id.into(),
            handler,
            dry_run,
            slot_builder,
        });
        self
    }

    /// Declares a context-param mapping (param name → context path) for the
    /// exported document ([`Registry::schema`]) and for
    /// [`crate::schema::MappedContextResolver`]. Programmatically-built
    /// registries use this so their exported schema carries the same mapping
    /// a declarative author would write.
    #[must_use]
    pub fn context_param(mut self, param: impl Into<String>, path: impl Into<String>) -> Self {
        self.context_params.insert(param.into(), path.into());
        self
    }

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
    /// types, mutating actions without a dry-run handler, or schema defs
    /// left without handlers ([`RegistryBuilder::attach`]).
    pub fn build(mut self) -> Result<Registry<P>, RegistryError> {
        // Resolve attachments against pending schema defs first, so the
        // invariant checks below (incl. mutates ⇒ dry_run) cover both the
        // programmatic and the declarative path.
        for attachment in self.attachments {
            let Some(at) = self
                .pending
                .iter()
                .position(|def| def.id == attachment.id)
            else {
                let attached_before = self.actions.iter().any(|reg| reg.def.id == attachment.id);
                return Err(if attached_before {
                    RegistryError::DuplicateAttachment(attachment.id)
                } else {
                    RegistryError::UnknownAttachment(attachment.id)
                });
            };
            self.actions.push(ActionRegistration {
                def: self.pending.remove(at),
                handler: attachment.handler,
                dry_run: attachment.dry_run,
                slot_builder: attachment.slot_builder,
            });
        }
        if let Some(def) = self.pending.first() {
            return Err(RegistryError::UnattachedAction(def.id.clone()));
        }
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
            if !reg.def.mutates && reg.dry_run.is_some() {
                return Err(RegistryError::UnexpectedDryRun(id));
            }
            if actions.insert(id.clone(), reg).is_some() {
                return Err(RegistryError::DuplicateAction(id));
            }
        }
        Ok(Registry {
            actions,
            blocks,
            context_params: self.context_params,
        })
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

    mod schema_path {
        use super::*;
        use crate::schema::RegistrySchema;

        fn schema() -> RegistrySchema {
            RegistrySchema {
                spec_version: crate::SPEC_VERSION.to_owned(),
                blocks: vec![confirm_block()],
                actions: vec![action("a.b", true)],
                context_params: std::collections::BTreeMap::from([(
                    "recordId".to_owned(),
                    "selected_record_id".to_owned(),
                )]),
            }
        }

        struct NoopDryRun;

        #[async_trait]
        impl DryRunHandler<()> for NoopDryRun {
            async fn dry_run(
                &self,
                _params: &JsonMap,
                _principal: &(),
            ) -> Result<crate::envelope::DryRunPreview, HandlerError> {
                Ok(crate::envelope::DryRunPreview::default())
            }
        }

        #[test]
        fn attached_schema_builds_and_round_trips() {
            let registry = RegistryBuilder::from_schema(schema())
                .attach("a.b", Arc::new(NoopHandler), Some(Arc::new(NoopDryRun)), None)
                .build()
                .expect("valid registry");

            assert_eq!(registry.schema(), schema());
        }

        #[test]
        fn unattached_def_fails_build() {
            let result = RegistryBuilder::<()>::from_schema(schema()).build();

            assert!(matches!(result, Err(RegistryError::UnattachedAction(id)) if id == "a.b"));
        }

        #[test]
        fn attach_for_undeclared_action_fails_build() {
            let result = RegistryBuilder::from_schema(schema())
                .attach("a.b", Arc::new(NoopHandler), Some(Arc::new(NoopDryRun)), None)
                .attach("no.such", Arc::new(NoopHandler), None, None)
                .build();

            assert!(matches!(result, Err(RegistryError::UnknownAttachment(id)) if id == "no.such"));
        }

        #[test]
        fn double_attach_fails_build() {
            let result = RegistryBuilder::from_schema(schema())
                .attach("a.b", Arc::new(NoopHandler), Some(Arc::new(NoopDryRun)), None)
                .attach("a.b", Arc::new(NoopHandler), Some(Arc::new(NoopDryRun)), None)
                .build();

            assert!(matches!(result, Err(RegistryError::DuplicateAttachment(id)) if id == "a.b"));
        }

        #[test]
        fn mutates_invariant_covers_schema_path() {
            let result = RegistryBuilder::from_schema(schema())
                .attach("a.b", Arc::new(NoopHandler), None, None)
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
