//! Declarative registry document (spec §4, non-normative serialization).
//!
//! The wire-visible registry data — everything except handlers — can be
//! authored as a versioned JSON document. The app loads it, attaches
//! handlers by action ID ([`crate::registry::RegistryBuilder::attach`]),
//! and builds; the same document is the codegen input and the interchange
//! format for conformance fixtures and non-Rust registry authors.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::envelope::Context;
use crate::registry::{ActionDef, BlockDef, ParamSpec};
use crate::seams::ContextParamResolver;

/// A versioned, serializable registry document.
///
/// Deserializes with [`RegistrySchema::from_json`]; the inverse of
/// [`crate::registry::Registry::schema`] modulo handlers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrySchema {
    /// Spec version the document targets; checked against
    /// [`crate::SPEC_VERSION`] by major.minor prefix, mirroring the `Init`
    /// handshake (§13).
    pub spec_version: String,
    /// Registered block types.
    pub blocks: Vec<BlockDef>,
    /// Registered action definitions.
    pub actions: Vec<ActionDef>,
    /// Declarative context-param filling: param name → context path, one of
    /// `route`, `selected_record_id`, `view_type`, `params.<key>`, or
    /// `extensions.<key>`. The `<key>` part is a single **literal** map key
    /// (no nested traversal; a key may itself contain dots). Paths are
    /// validated at load ([`SchemaError::InvalidContextPath`]). Consumed by
    /// [`MappedContextResolver`]; apps with richer needs implement
    /// [`ContextParamResolver`] instead — in which case this mapping does
    /// not describe their behavior and should be left empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context_params: BTreeMap<String, String>,
}

/// Errors parsing a registry document.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    /// The document is not valid JSON for this schema.
    #[error("registry schema parse error: {0}")]
    Parse(#[from] serde_json::Error),
    /// The document targets a spec version this crate does not implement.
    #[error("registry schema speaks `{found}`, this crate implements `{supported}`")]
    UnsupportedVersion {
        /// Version named by the document.
        found: String,
        /// Version this crate implements.
        supported: &'static str,
    },
    /// A `context_params` entry names a path [`MappedContextResolver`] does
    /// not support — caught here so a typo'd mapping fails at load, not as
    /// an unresolvable param on every proposal.
    #[error(
        "context_params[`{param}`] = `{path}` is not a supported context path \
         (route | selected_record_id | view_type | params.<key> | extensions.<key>)"
    )]
    InvalidContextPath {
        /// The mapped parameter.
        param: String,
        /// The rejected path.
        path: String,
    },
    /// A context-sourced param has no `context_params` mapping — caught when
    /// the app opts into [`RegistrySchema::context_resolver`], so the gap
    /// surfaces at startup instead of as an unresolvable param on every
    /// proposal of that action.
    #[error(
        "action `{action}` param `{param}` is context-sourced but has no \
         context_params mapping"
    )]
    UnmappedContextParam {
        /// The action whose param cannot be filled.
        action: String,
        /// The unmapped context-sourced param.
        param: String,
    },
}

/// The path grammar [`MappedContextResolver`] resolves: one of the fixed
/// context fields, or `params.`/`extensions.` followed by a non-empty
/// **literal** key (the remainder is not traversed further — a key may
/// itself contain dots).
fn context_path_is_valid(path: &str) -> bool {
    match path.split_once('.') {
        None => matches!(path, "route" | "selected_record_id" | "view_type"),
        Some(("params" | "extensions", key)) => !key.is_empty(),
        Some(_) => false,
    }
}

impl RegistrySchema {
    /// Parses and version-gates a registry document. Takes a string, not a
    /// path — file I/O stays with the caller (CLI, `include_str!`, FFI).
    ///
    /// # Errors
    ///
    /// [`SchemaError::Parse`] on malformed JSON or unknown fields;
    /// [`SchemaError::UnsupportedVersion`] when `spec_version` does not
    /// match this crate's major.minor prefix.
    pub fn from_json(json: &str) -> Result<Self, SchemaError> {
        let schema: Self = serde_json::from_str(json)?;
        if !crate::spec_version_compatible(&schema.spec_version) {
            return Err(SchemaError::UnsupportedVersion {
                found: schema.spec_version,
                supported: crate::SPEC_VERSION,
            });
        }
        for (param, path) in &schema.context_params {
            if !context_path_is_valid(path) {
                return Err(SchemaError::InvalidContextPath {
                    param: param.clone(),
                    path: path.clone(),
                });
            }
        }
        Ok(schema)
    }

    /// Canonical pretty-printed JSON (round-trip tests, freshness checks).
    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        let mut out = serde_json::to_string_pretty(self).unwrap_or_else(|_| String::new());
        out.push('\n');
        out
    }

    /// A [`ContextParamResolver`] over this document's `context_params`
    /// mapping, checked for coverage: every context-sourced param of every
    /// action must have a mapping, since the mapped resolver is the only
    /// thing that will ever fill them. Apps with richer needs implement
    /// [`ContextParamResolver`] themselves and never call this.
    ///
    /// # Errors
    ///
    /// [`SchemaError::UnmappedContextParam`] naming the first uncovered
    /// param.
    pub fn context_resolver(&self) -> Result<MappedContextResolver, SchemaError> {
        for action in &self.actions {
            for param in &action.params {
                if param.source == crate::registry::ParamSource::Context
                    && !self.context_params.contains_key(&param.name)
                {
                    return Err(SchemaError::UnmappedContextParam {
                        action: action.id.clone(),
                        param: param.name.clone(),
                    });
                }
            }
        }
        Ok(MappedContextResolver::new(self.context_params.clone()))
    }
}

/// Fills context-sourced params from a declarative param-name → context-path
/// table — the common case (spec §9.5) with zero app code. Supported paths
/// are exactly the [`RegistrySchema::context_params`] grammar; unmapped
/// params resolve to `None`, which validation reports as unresolvable.
pub struct MappedContextResolver {
    paths: BTreeMap<String, String>,
}

impl MappedContextResolver {
    /// Builds a resolver from a param → dotted-path table.
    #[must_use]
    pub fn new(paths: BTreeMap<String, String>) -> Self {
        Self { paths }
    }
}

impl ContextParamResolver for MappedContextResolver {
    fn resolve(
        &self,
        _action_id: &str,
        param: &ParamSpec,
        context: &Context,
    ) -> Option<serde_json::Value> {
        let path = self.paths.get(&param.name)?;
        match path.split_once('.') {
            None => match path.as_str() {
                "route" => Some(serde_json::Value::String(context.route.clone())),
                "selected_record_id" => context
                    .selected_record_id
                    .clone()
                    .map(serde_json::Value::String),
                "view_type" => context.view_type.clone().map(serde_json::Value::String),
                _ => None,
            },
            Some(("params", key)) => context.params.get(key).cloned(),
            Some(("extensions", key)) => context.extensions.get(key).cloned(),
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{ParamSource, ValueType};

    fn document() -> String {
        serde_json::json!({
            "spec_version": "0.1",
            "blocks": [
                {
                    "block_type": "confirm",
                    "slots": [
                        { "name": "title", "type": "string", "required": true }
                    ]
                }
            ],
            "actions": [
                {
                    "id": "record.remove",
                    "description": "Remove the selected record",
                    "params": [
                        { "name": "recordId", "type": "string",
                          "source": "context", "required": true }
                    ],
                    "block_type": "confirm",
                    "mutates": true,
                    "authz_key": "record.remove"
                }
            ],
            "context_params": { "recordId": "selected_record_id" }
        })
        .to_string()
    }

    #[test]
    fn round_trips_through_json() {
        let schema = RegistrySchema::from_json(&document()).expect("valid document");

        let reparsed =
            RegistrySchema::from_json(&schema.to_json_pretty()).expect("canonical JSON is valid");

        assert_eq!(schema, reparsed);
    }

    #[test]
    fn defaults_param_description_to_empty() {
        let schema = RegistrySchema::from_json(&document()).expect("valid document");

        assert_eq!(schema.actions[0].params[0].description, "");
    }

    #[test]
    fn rejects_unknown_fields() {
        let doc = document().replace("\"mutates\"", "\"mutatess\"");

        assert!(matches!(
            RegistrySchema::from_json(&doc),
            Err(SchemaError::Parse(_))
        ));
    }

    #[test]
    fn rejects_unsupported_version() {
        let doc = document().replace("\"0.1\"", "\"9.9\"");

        assert!(matches!(
            RegistrySchema::from_json(&doc),
            Err(SchemaError::UnsupportedVersion { found, .. }) if found == "9.9"
        ));
    }

    #[test]
    fn accepts_draft_suffixed_version() {
        let doc = document().replace("\"0.1\"", "\"0.1.0-draft\"");

        assert!(RegistrySchema::from_json(&doc).is_ok());
    }

    #[test]
    fn rejects_prefix_lookalike_versions() {
        for lookalike in ["0.10", "0.10.0", "0.1x"] {
            let doc = document().replace("\"0.1\"", &format!("\"{lookalike}\""));

            assert!(
                matches!(
                    RegistrySchema::from_json(&doc),
                    Err(SchemaError::UnsupportedVersion { .. })
                ),
                "`{lookalike}` must not pass the 0.1 gate"
            );
        }
    }

    #[test]
    fn rejects_unsupported_context_paths() {
        for bad in [
            "selected_record",
            "selected_record_id.foo",
            "params.",
            "para.x",
        ] {
            let doc = document().replace("\"selected_record_id\"", &format!("\"{bad}\""));

            assert!(
                matches!(
                    RegistrySchema::from_json(&doc),
                    Err(SchemaError::InvalidContextPath { .. })
                ),
                "path `{bad}` must be rejected at load"
            );
        }
    }

    mod checked_resolver {
        use super::*;

        #[test]
        fn covers_every_context_param() {
            let schema = RegistrySchema::from_json(&document()).expect("valid document");

            assert!(schema.context_resolver().is_ok());
        }

        #[test]
        fn reports_unmapped_context_param_at_startup() {
            let mut schema = RegistrySchema::from_json(&document()).expect("valid document");
            schema.context_params.clear();

            assert!(matches!(
                schema.context_resolver(),
                Err(SchemaError::UnmappedContextParam { action, param })
                    if action == "record.remove" && param == "recordId"
            ));
        }
    }

    mod mapped_resolver {
        use super::*;

        fn param(name: &str) -> ParamSpec {
            ParamSpec {
                name: name.to_owned(),
                description: String::new(),
                ty: ValueType::String,
                source: ParamSource::Context,
                required: true,
            }
        }

        fn resolver() -> MappedContextResolver {
            MappedContextResolver::new(BTreeMap::from([
                ("recordId".to_owned(), "selected_record_id".to_owned()),
                ("where".to_owned(), "route".to_owned()),
                ("filter".to_owned(), "params.filter".to_owned()),
                ("tenant".to_owned(), "extensions.tenant".to_owned()),
            ]))
        }

        fn context() -> Context {
            Context {
                route: "/records/r-1".to_owned(),
                params: crate::slots! { "filter": "active" },
                selected_record_id: Some("r-1".to_owned()),
                view_type: None,
                extensions: crate::slots! { "tenant": "acme" },
            }
        }

        #[test]
        fn resolves_each_path_kind() {
            let (resolver, context) = (resolver(), context());

            let get = |name: &str| resolver.resolve("a.b", &param(name), &context);

            assert_eq!(get("recordId"), Some("r-1".into()));
            assert_eq!(get("where"), Some("/records/r-1".into()));
            assert_eq!(get("filter"), Some("active".into()));
            assert_eq!(get("tenant"), Some("acme".into()));
        }

        #[test]
        fn unmapped_or_absent_params_resolve_to_none() {
            let resolver = resolver();
            let mut context = context();
            context.selected_record_id = None;

            assert_eq!(resolver.resolve("a.b", &param("recordId"), &context), None);
            assert_eq!(resolver.resolve("a.b", &param("unmapped"), &context), None);
        }
    }
}
