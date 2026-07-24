//! Neutral declarative encodings for seams that hosts configure as data
//! rather than code: conformance fixtures, the FFI bindings (Node hosts
//! cannot register synchronous slot callbacks), and standalone-service
//! configuration files.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::JsonMap;
use crate::envelope::DryRunPreview;
use crate::seams::{CompletionItem, SlotBuilder};

/// One declarative slot source: `{"const": <json>}`, `{"param": "<name>"}`,
/// or `{"preview": "summary"}`.
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

/// A [`SlotBuilder`] over a declarative slot → source table.
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

/// One scripted model-output item — the neutral encoding shared by
/// conformance vectors, the FFI scripted provider, and declarative service
/// configuration.
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
