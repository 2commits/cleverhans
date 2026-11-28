//! Keeps the serde wire types and `spec/schemas/*.json` in step: the JSON
//! Schemas are the reference, and these tests fail when a Rust type changes
//! without its schema or a schema changes without its type.
//!
//! - Every schema is valid draft 2020-12 and named by its `$id`.
//! - Every `ClientEvent`/`ServerEvent` variant has a fully populated and a
//!   minimal example that validates; every serde variant has a schema
//!   definition and every definition a serde variant.
//! - Per variant and per nested shape: the serialized keys of the full
//!   example equal the schema's `properties`; the schema's `required` keys
//!   alone deserialize and dropping any one of them fails; a property
//!   accepts `null` in Rust exactly when the schema allows `null`.
//! - The same parity for the registry document (spec §4), every registry
//!   document in the repository validates, and tables of invalid and valid
//!   edge-case documents get the same verdict from the schema and from
//!   [`RegistrySchema::from_json`].
//! - Every complete envelope message in `spec/vectors/` validates.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use cleverhans_core::JsonMap;
use cleverhans_core::envelope::{ActionProposal, ClientEvent, Context, DryRunPreview, ServerEvent};
use cleverhans_core::proposal::ProposalState;
use cleverhans_core::registry::{
    ActionDef, BlockDef, DisplayDef, ParamSource, ParamSpec, SlotSpec, ValueType,
};
use cleverhans_core::schema::RegistrySchema;

const SCHEMA_FILES: [&str; 3] = ["client-event.json", "server-event.json", "registry.json"];
const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn read_json(relative: &str) -> Value {
    let path = repo_path(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn schema(file: &str) -> Value {
    read_json(&format!("spec/schemas/{file}"))
}

fn validator(schema: &Value) -> jsonschema::Validator {
    jsonschema::draft202012::new(schema).expect("schema compiles")
}

/// The schema narrowed to one JSON pointer inside it (a `$defs` entry or a
/// property), keeping `$defs` so internal `$ref`s still resolve.
fn narrowed(schema: &Value, pointer: &str) -> jsonschema::Validator {
    let mut narrowed = json!({ "$schema": DRAFT_2020_12, "$defs": schema["$defs"].clone() });
    // A pointer into the root's own `properties` is re-homed under `$defs`,
    // where those properties do not constrain the instance itself.
    let pointer = match pointer.strip_prefix("/properties/") {
        Some(rest) => {
            narrowed["$defs"]["__root"] = json!({ "properties": schema["properties"].clone() });
            format!("/$defs/__root/properties/{rest}")
        }
        None => pointer.to_owned(),
    };
    narrowed["$ref"] = json!(format!("#{pointer}"));
    validator(&narrowed)
}

fn assert_valid(validator: &jsonschema::Validator, instance: &Value, what: &str) {
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|err| format!("{err} (at `{}`)", err.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{what} does not validate:\n  {}\n{instance:#}",
        errors.join("\n  ")
    );
}

fn def<'a>(schema: &'a Value, name: &str) -> &'a Value {
    schema["$defs"]
        .get(name)
        .unwrap_or_else(|| panic!("schema has no $defs/{name}"))
}

fn property_keys(shape: &Value) -> BTreeSet<String> {
    shape["properties"]
        .as_object()
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default()
}

fn required_keys(shape: &Value) -> BTreeSet<String> {
    shape["required"]
        .as_array()
        .map(|keys| {
            keys.iter()
                .map(|key| key.as_str().expect("string key").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn object_keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("not an object: {value}"))
        .keys()
        .cloned()
        .collect()
}

fn to_value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).expect("serializes")
}

/// The variant names serde accepts for `T`, read from the error serde gives
/// for an unknown variant — the derive's own list, so it cannot drift.
fn serde_variants<T: DeserializeOwned + std::fmt::Debug>(probe: Value) -> BTreeSet<String> {
    let message = serde_json::from_value::<T>(probe)
        .expect_err("the probe names no variant")
        .to_string();
    // serde writes "expected `a`", "expected `a` or `b`" or
    // "expected one of `a`, `b`, `c`".
    let (_, list) = message
        .split_once(", expected ")
        .unwrap_or_else(|| panic!("unexpected serde message: {message}"));
    let list = list.strip_prefix("one of ").unwrap_or(list);
    list.split(", ")
        .flat_map(|part| part.split(" or "))
        .map(|name| name.trim_matches('`').to_owned())
        .collect()
}

/// The `type` tags of a tagged-union schema's `oneOf` definitions.
fn schema_tags(schema: &Value) -> BTreeSet<String> {
    schema["oneOf"]
        .as_array()
        .expect("schema has oneOf")
        .iter()
        .map(|branch| {
            let name = branch["$ref"]
                .as_str()
                .and_then(|r| r.strip_prefix("#/$defs/"))
                .expect("oneOf branch is a $defs ref");
            def(schema, name)["properties"]["type"]["const"]
                .as_str()
                .expect("definition has a const type")
                .to_owned()
        })
        .collect()
}

/// One shape whose Rust type and schema definition must agree: its fully
/// populated serialized form, the schema definition that describes it,
/// and how Rust reads it back.
struct Shape {
    /// Schema file the definition lives in.
    file: &'static str,
    /// JSON pointer of the definition inside that file.
    pointer: String,
    /// The fully populated example, serialized.
    full: Value,
    /// Whether Rust deserializes `value` into this shape's type.
    reads: fn(Value) -> bool,
}

fn reads<T: DeserializeOwned>(value: Value) -> bool {
    serde_json::from_value::<T>(value).is_ok()
}

/// Property parity, required parity and null parity for one shape.
fn assert_parity(shape: &Shape) {
    let schema = schema(shape.file);
    let definition = schema
        .pointer(&shape.pointer)
        .unwrap_or_else(|| panic!("{} has no {}", shape.file, shape.pointer));
    let what = format!("{}#{}", shape.file, shape.pointer);

    // Property parity: the full example writes exactly the schema's keys.
    assert_eq!(
        object_keys(&shape.full),
        property_keys(definition),
        "{what}: serialized keys of the full example differ from the schema's properties"
    );

    // Required parity: the required keys alone deserialize ...
    let required = required_keys(definition);
    let full = shape.full.as_object().expect("object");
    let only_required: JsonMap = full
        .iter()
        .filter(|(key, _)| required.contains(*key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    assert!(
        (shape.reads)(Value::Object(only_required.clone())),
        "{what}: an object with only the required keys {required:?} does not deserialize"
    );
    // ... and dropping any one of them fails.
    for key in &required {
        let mut missing = only_required.clone();
        missing.remove(key);
        assert!(
            !(shape.reads)(Value::Object(missing)),
            "{what}: deserializes without the required key `{key}` — make it optional in the schema"
        );
    }

    // Null parity: Rust reads `null` for a property exactly when the schema
    // allows it.
    for key in property_keys(definition) {
        let allows_null = narrowed(&schema, &format!("{}/properties/{key}", shape.pointer))
            .is_valid(&Value::Null);
        let mut with_null = full.clone();
        with_null.insert(key.clone(), Value::Null);
        assert_eq!(
            (shape.reads)(Value::Object(with_null)),
            allows_null,
            "{what}: `{key}: null` — Rust and the schema disagree on whether null is accepted"
        );
    }
}

mod schema_files {
    use super::*;

    #[test]
    fn every_schema_is_valid_draft_2020_12_and_named_by_its_id() {
        for file in SCHEMA_FILES {
            let schema = schema(file);

            assert_eq!(schema["$schema"], DRAFT_2020_12, "{file}: $schema");
            jsonschema::draft202012::meta::validate(&schema)
                .unwrap_or_else(|err| panic!("{file} is not valid draft 2020-12: {err}"));
            let id = schema["$id"].as_str().expect("$id is a string");
            assert!(
                id.starts_with("https://cleverhans.dev/spec/v0.1/")
                    && id.ends_with(&format!("/{file}")),
                "{file}: $id `{id}` must be https://cleverhans.dev/spec/v0.1/{file}"
            );
            assert!(schema["title"].is_string(), "{file}: title");
            assert!(schema["description"].is_string(), "{file}: description");
            validator(&schema);
        }
    }

    #[test]
    fn every_schema_file_in_the_folder_is_checked_here() {
        let on_disk: BTreeSet<String> = std::fs::read_dir(repo_path("spec/schemas"))
            .expect("read spec/schemas")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(".json"))
            .collect();

        assert_eq!(
            on_disk,
            SCHEMA_FILES.iter().map(ToString::to_string).collect(),
            "a schema in spec/schemas/ has no sync test"
        );
    }

    #[test]
    fn every_property_and_definition_is_described() {
        fn walk(node: &Value, path: &str, missing: &mut Vec<String>) {
            match node {
                Value::Object(map) => {
                    if let Some(Value::Object(props)) = map.get("properties") {
                        for (key, prop) in props {
                            if prop.get("description").is_none() {
                                missing.push(format!("{path}/properties/{key}"));
                            }
                        }
                    }
                    if let Some(Value::Object(defs)) = map.get("$defs") {
                        for (name, definition) in defs {
                            if definition.get("description").is_none() {
                                missing.push(format!("{path}/$defs/{name}"));
                            }
                        }
                    }
                    for (key, child) in map {
                        walk(child, &format!("{path}/{key}"), missing);
                    }
                }
                Value::Array(items) => {
                    for (i, child) in items.iter().enumerate() {
                        walk(child, &format!("{path}/{i}"), missing);
                    }
                }
                _ => {}
            }
        }

        for file in SCHEMA_FILES {
            let mut missing = Vec::new();
            walk(&schema(file), "", &mut missing);
            assert!(missing.is_empty(), "{file}: no description at {missing:?}");
        }
    }

    #[test]
    fn envelope_schemas_stay_open_for_additive_evolution() {
        // Spec §13: receivers ignore unknown fields, so no envelope shape
        // may close itself.
        for file in ["client-event.json", "server-event.json"] {
            let text = std::fs::read_to_string(repo_path(&format!("spec/schemas/{file}")))
                .expect("read schema");
            assert!(
                !text.contains("\"additionalProperties\": false"),
                "{file} closes a shape; the envelope evolves additively"
            );
        }
    }
}

mod envelope {
    use super::*;

    /// An example of one enum variant: its schema definition, a fully
    /// populated form (every optional field set) and a minimal form (every
    /// optional field empty).
    struct Example<T> {
        def: &'static str,
        full: T,
        minimal: T,
    }

    fn full_context() -> Context {
        Context {
            route: "/transactions/tx_581".to_owned(),
            params: cleverhans_core::slots! { "tab": "parties" },
            selected_record_id: Some("tx_581".to_owned()),
            view_type: Some("detail".to_owned()),
            extensions: cleverhans_core::slots! { "tenant": "acme" },
        }
    }

    fn minimal_context() -> Context {
        Context {
            route: "/".to_owned(),
            ..Context::default()
        }
    }

    fn full_preview() -> DryRunPreview {
        DryRunPreview {
            affected_count: 1,
            sample_ids: vec!["cb_1".to_owned()],
            summary: Some("Removes 1 co-buyer".to_owned()),
            extensions: cleverhans_core::slots! { "diff": "-cb_1" },
        }
    }

    fn full_proposal() -> ActionProposal {
        ActionProposal {
            proposal_id: "prop-1".to_owned(),
            action_id: "transaction.coBuyer.remove".to_owned(),
            params: cleverhans_core::slots! { "transactionId": "tx_581" },
            block_type: "confirm".to_owned(),
            slots: cleverhans_core::slots! { "title": "Remove co-buyer" },
            preview: Some(full_preview()),
            context_seq: 7,
            turn_msg_id: Some("m-1".to_owned()),
        }
    }

    fn full_client_events() -> Vec<ClientEvent> {
        vec![
            ClientEvent::Init {
                spec_version: "0.1.0-draft".to_owned(),
                context: full_context(),
            },
            ClientEvent::ContextUpdate {
                context: full_context(),
                context_seq: 7,
            },
            ClientEvent::UserMessage {
                text: "remove the co-buyer".to_owned(),
                client_msg_id: "c-1".to_owned(),
            },
            ClientEvent::ConfirmAction {
                proposal_id: "prop-1".to_owned(),
            },
            ClientEvent::RejectAction {
                proposal_id: "prop-1".to_owned(),
                reason: Some("wrong transaction".to_owned()),
            },
        ]
    }

    /// Exhaustive on purpose — no wildcard arm: a new `ClientEvent` variant
    /// does not compile until it names its schema definition and its
    /// minimal example, and `every_variant_has_an_example_and_a_definition`
    /// fails until it has a full example and a schema entry.
    fn client_example(full: ClientEvent) -> Example<ClientEvent> {
        let (def, minimal) = match &full {
            ClientEvent::Init { spec_version, .. } => (
                "Init",
                ClientEvent::Init {
                    spec_version: spec_version.clone(),
                    context: minimal_context(),
                },
            ),
            ClientEvent::ContextUpdate { context_seq, .. } => (
                "ContextUpdate",
                ClientEvent::ContextUpdate {
                    context: minimal_context(),
                    context_seq: *context_seq,
                },
            ),
            ClientEvent::UserMessage {
                text,
                client_msg_id,
            } => (
                "UserMessage",
                ClientEvent::UserMessage {
                    text: text.clone(),
                    client_msg_id: client_msg_id.clone(),
                },
            ),
            ClientEvent::ConfirmAction { proposal_id } => (
                "ConfirmAction",
                ClientEvent::ConfirmAction {
                    proposal_id: proposal_id.clone(),
                },
            ),
            ClientEvent::RejectAction { proposal_id, .. } => (
                "RejectAction",
                ClientEvent::RejectAction {
                    proposal_id: proposal_id.clone(),
                    reason: None,
                },
            ),
        };
        Example { def, full, minimal }
    }

    fn full_server_events() -> Vec<ServerEvent> {
        vec![
            ServerEvent::ChatMessage {
                msg_id: "m-1".to_owned(),
                text: "Here is the proposal.".to_owned(),
                done: true,
            },
            ServerEvent::ActionProposal(full_proposal()),
            ServerEvent::ProposalStateChanged {
                proposal_id: "prop-1".to_owned(),
                state: ProposalState::Executed,
                reason: Some("done".to_owned()),
                result: Some(json!({ "removed": true })),
            },
            ServerEvent::Error {
                code: "malformed_event".to_owned(),
                message: "frame is not a ClientEvent".to_owned(),
                recoverable: true,
            },
        ]
    }

    /// Exhaustive on purpose — see [`client_example`].
    fn server_example(full: ServerEvent) -> Example<ServerEvent> {
        let (def, minimal) = match &full {
            ServerEvent::ChatMessage { msg_id, text, done } => (
                "ChatMessage",
                ServerEvent::ChatMessage {
                    msg_id: msg_id.clone(),
                    text: text.clone(),
                    done: *done,
                },
            ),
            ServerEvent::ActionProposal(proposal) => (
                "ActionProposal",
                ServerEvent::ActionProposal(ActionProposal {
                    params: JsonMap::new(),
                    slots: JsonMap::new(),
                    preview: None,
                    turn_msg_id: None,
                    ..proposal.clone()
                }),
            ),
            ServerEvent::ProposalStateChanged {
                proposal_id, state, ..
            } => (
                "ProposalStateChanged",
                ServerEvent::ProposalStateChanged {
                    proposal_id: proposal_id.clone(),
                    state: *state,
                    reason: None,
                    result: None,
                },
            ),
            ServerEvent::Error {
                code,
                message,
                recoverable,
            } => (
                "Error",
                ServerEvent::Error {
                    code: code.clone(),
                    message: message.clone(),
                    recoverable: *recoverable,
                },
            ),
        };
        Example { def, full, minimal }
    }

    fn client_examples() -> Vec<Example<ClientEvent>> {
        full_client_events()
            .into_iter()
            .map(client_example)
            .collect()
    }

    fn server_examples() -> Vec<Example<ServerEvent>> {
        full_server_events()
            .into_iter()
            .map(server_example)
            .collect()
    }

    fn tag(value: &Value) -> String {
        value["type"].as_str().expect("tagged").to_owned()
    }

    #[test]
    fn every_variant_has_an_example_and_a_definition() {
        let client = schema("client-event.json");
        let server = schema("server-event.json");

        let client_tags: BTreeSet<String> = client_examples()
            .iter()
            .map(|example| tag(&to_value(&example.full)))
            .collect();
        let server_tags: BTreeSet<String> = server_examples()
            .iter()
            .map(|example| tag(&to_value(&example.full)))
            .collect();

        let client_variants = serde_variants::<ClientEvent>(json!({ "type": "?" }));
        let server_variants = serde_variants::<ServerEvent>(json!({ "type": "?" }));
        assert_eq!(
            client_tags, client_variants,
            "a ClientEvent variant has no example"
        );
        assert_eq!(
            server_tags, server_variants,
            "a ServerEvent variant has no example"
        );
        assert_eq!(
            schema_tags(&client),
            client_variants,
            "client-event.json oneOf"
        );
        assert_eq!(
            schema_tags(&server),
            server_variants,
            "server-event.json oneOf"
        );
        for example in client_examples() {
            assert_eq!(
                def(&client, example.def)["properties"]["type"]["const"],
                tag(&to_value(&example.full)),
                "{} names the wrong definition",
                example.def
            );
        }
        for example in server_examples() {
            assert_eq!(
                def(&server, example.def)["properties"]["type"]["const"],
                tag(&to_value(&example.full)),
                "{} names the wrong definition",
                example.def
            );
        }
    }

    #[test]
    fn every_example_validates_in_full_and_minimal_form() {
        let client = schema("client-event.json");
        let server = schema("server-event.json");
        let (client_all, server_all) = (validator(&client), validator(&server));

        for example in client_examples() {
            let own = narrowed(&client, &format!("/$defs/{}", example.def));
            for (form, event) in [("full", &example.full), ("minimal", &example.minimal)] {
                let value = to_value(event);
                assert_valid(&client_all, &value, &format!("{} ({form})", example.def));
                assert_valid(
                    &own,
                    &value,
                    &format!("{} ({form}) against its own definition", example.def),
                );
                assert_eq!(
                    &serde_json::from_value::<ClientEvent>(value).expect("reads back"),
                    event
                );
            }
        }
        for example in server_examples() {
            let own = narrowed(&server, &format!("/$defs/{}", example.def));
            for (form, event) in [("full", &example.full), ("minimal", &example.minimal)] {
                let value = to_value(event);
                assert_valid(&server_all, &value, &format!("{} ({form})", example.def));
                assert_valid(
                    &own,
                    &value,
                    &format!("{} ({form}) against its own definition", example.def),
                );
                assert_eq!(
                    &serde_json::from_value::<ServerEvent>(value).expect("reads back"),
                    event
                );
            }
        }
    }

    #[test]
    fn every_known_proposal_state_validates() {
        let server = schema("server-event.json");
        let server_all = validator(&server);
        let states = serde_variants::<ProposalState>(json!("?"));

        for state in &states {
            let event = json!({
                "type": "proposal_state_changed",
                "proposal_id": "prop-1",
                "state": state,
            });
            assert_valid(&server_all, &event, &format!("state `{state}`"));
            assert!(
                def(&server, "ProposalStateChanged")["properties"]["state"]["description"]
                    .as_str()
                    .is_some_and(|text| text.contains(&format!("`{state}`"))),
                "the state description does not name `{state}`"
            );
        }
    }

    fn client_shapes() -> Vec<Shape> {
        let mut shapes: Vec<Shape> = client_examples()
            .into_iter()
            .map(|example| Shape {
                file: "client-event.json",
                pointer: format!("/$defs/{}", example.def),
                full: to_value(&example.full),
                reads: reads::<ClientEvent>,
            })
            .collect();
        shapes.push(Shape {
            file: "client-event.json",
            pointer: "/$defs/Context".to_owned(),
            full: to_value(&full_context()),
            reads: reads::<Context>,
        });
        shapes
    }

    fn server_shapes() -> Vec<Shape> {
        let mut shapes: Vec<Shape> = server_examples()
            .into_iter()
            .map(|example| Shape {
                file: "server-event.json",
                pointer: format!("/$defs/{}", example.def),
                full: to_value(&example.full),
                reads: reads::<ServerEvent>,
            })
            .collect();
        shapes.push(Shape {
            file: "server-event.json",
            pointer: "/$defs/DryRunPreview".to_owned(),
            full: to_value(&full_preview()),
            reads: reads::<DryRunPreview>,
        });
        shapes
    }

    #[test]
    fn client_shapes_match_the_schema() {
        for shape in client_shapes() {
            assert_parity(&shape);
        }
    }

    #[test]
    fn server_shapes_match_the_schema() {
        for shape in server_shapes() {
            assert_parity(&shape);
        }
    }

    #[test]
    fn nested_shapes_appear_in_the_full_examples() {
        // The nested definitions checked above are the ones the variants
        // actually carry.
        let init = to_value(&full_client_events()[0]);
        assert_eq!(init["context"], to_value(&full_context()));
        let proposal = to_value(&ServerEvent::ActionProposal(full_proposal()));
        assert_eq!(proposal["preview"], to_value(&full_preview()));
    }

    #[test]
    fn every_definition_has_a_parity_check() {
        for (file, shapes) in [
            ("client-event.json", client_shapes()),
            ("server-event.json", server_shapes()),
        ] {
            let checked: BTreeSet<String> =
                shapes.iter().map(|shape| shape.pointer.clone()).collect();
            let defined: BTreeSet<String> = object_keys(&schema(file)["$defs"])
                .into_iter()
                .map(|name| format!("/$defs/{name}"))
                .collect();
            assert_eq!(checked, defined, "{file}: a definition has no parity check");
        }
    }

    #[test]
    fn an_empty_preview_is_valid() {
        let server = schema("server-event.json");
        let proposal = ServerEvent::ActionProposal(ActionProposal {
            preview: Some(DryRunPreview::default()),
            ..full_proposal()
        });

        assert_valid(
            &validator(&server),
            &to_value(&proposal),
            "an empty preview",
        );
        assert!(
            serde_json::from_value::<DryRunPreview>(json!({})).is_ok(),
            "`{{}}` is a valid empty preview"
        );
    }

    #[test]
    fn unknown_fields_are_accepted_by_both() {
        let client = schema("client-event.json");
        let server = schema("server-event.json");

        let mut event = to_value(&full_client_events()[0]);
        event["future_field"] = json!(1);
        event["context"]["future_field"] = json!(1);
        assert_valid(&validator(&client), &event, "init with unknown fields");
        assert!(serde_json::from_value::<ClientEvent>(event).is_ok());

        let mut event = to_value(&ServerEvent::ActionProposal(full_proposal()));
        event["future_field"] = json!(1);
        event["preview"]["future_field"] = json!(1);
        assert_valid(&validator(&server), &event, "proposal with unknown fields");
        assert!(serde_json::from_value::<ServerEvent>(event).is_ok());
    }

    #[test]
    fn an_unknown_type_is_rejected_by_both() {
        let event = json!({ "type": "teleport", "proposal_id": "prop-1" });

        assert!(!validator(&schema("client-event.json")).is_valid(&event));
        assert!(!validator(&schema("server-event.json")).is_valid(&event));
        assert!(serde_json::from_value::<ClientEvent>(event.clone()).is_err());
        assert!(serde_json::from_value::<ServerEvent>(event).is_err());
    }
}

mod registry {
    use super::*;

    fn full_display() -> DisplayDef {
        DisplayDef {
            title: "Remove co-buyer".to_owned(),
            description: "Takes the co-buyer off this transaction".to_owned(),
            keywords: vec!["co-buyer".to_owned()],
            group: Some("transactions".to_owned()),
            tags: vec!["ai".to_owned()],
        }
    }

    fn full_param() -> ParamSpec {
        ParamSpec {
            name: "country".to_owned(),
            description: "ISO country code".to_owned(),
            ty: ValueType::StringEnum(vec!["NO".to_owned(), "SE".to_owned()]),
            source: ParamSource::Utterance,
            required: true,
        }
    }

    fn full_slot() -> SlotSpec {
        SlotSpec {
            name: "title".to_owned(),
            ty: ValueType::String,
            required: true,
        }
    }

    fn full_block() -> BlockDef {
        BlockDef {
            block_type: "confirm".to_owned(),
            slots: vec![full_slot()],
        }
    }

    fn full_action() -> ActionDef {
        ActionDef {
            id: "transaction.setCountry".to_owned(),
            description: "Set the transaction country".to_owned(),
            params: vec![
                ParamSpec {
                    name: "transactionId".to_owned(),
                    description: String::new(),
                    ty: ValueType::String,
                    source: ParamSource::Context,
                    required: true,
                },
                full_param(),
            ],
            block_type: "confirm".to_owned(),
            mutates: true,
            authz_key: "transaction.setCountry".to_owned(),
            display: Some(full_display()),
        }
    }

    fn full_document() -> RegistrySchema {
        RegistrySchema {
            schema_uri: Some("https://cleverhans.dev/spec/v0.1/registry.json".to_owned()),
            spec_version: "0.1".to_owned(),
            blocks: vec![full_block()],
            actions: vec![full_action()],
            context_params: [("transactionId".to_owned(), "selected_record_id".to_owned())].into(),
        }
    }

    /// Every [`ValueType`] variant once. Exhaustive on purpose — no
    /// wildcard arm: a new variant does not compile until it is listed.
    fn value_type_examples() -> Vec<ValueType> {
        let all = vec![
            ValueType::String,
            ValueType::Integer,
            ValueType::Number,
            ValueType::Boolean,
            ValueType::StringEnum(vec!["a".to_owned(), "b".to_owned()]),
            ValueType::Json,
        ];
        for ty in &all {
            match ty {
                ValueType::String
                | ValueType::Integer
                | ValueType::Number
                | ValueType::Boolean
                | ValueType::StringEnum(_)
                | ValueType::Json => {}
            }
        }
        all
    }

    fn shapes() -> Vec<Shape> {
        let file = "registry.json";
        vec![
            Shape {
                file,
                pointer: String::new(),
                full: to_value(&full_document()),
                reads: reads::<RegistrySchema>,
            },
            Shape {
                file,
                pointer: "/$defs/BlockDef".to_owned(),
                full: to_value(&full_block()),
                reads: reads::<BlockDef>,
            },
            Shape {
                file,
                pointer: "/$defs/SlotSpec".to_owned(),
                full: to_value(&full_slot()),
                reads: reads::<SlotSpec>,
            },
            Shape {
                file,
                pointer: "/$defs/ActionDef".to_owned(),
                full: to_value(&full_action()),
                reads: reads::<ActionDef>,
            },
            Shape {
                file,
                pointer: "/$defs/ParamSpec".to_owned(),
                full: to_value(&full_param()),
                reads: reads::<ParamSpec>,
            },
            Shape {
                file,
                pointer: "/$defs/DisplayDef".to_owned(),
                full: to_value(&full_display()),
                reads: reads::<DisplayDef>,
            },
        ]
    }

    #[test]
    fn the_full_document_validates() {
        assert_valid(
            &validator(&schema("registry.json")),
            &to_value(&full_document()),
            "the full registry document",
        );
    }

    #[test]
    fn registry_shapes_match_the_schema() {
        for shape in shapes() {
            assert_parity(&shape);
        }
    }

    #[test]
    fn every_closed_rust_shape_is_closed_in_the_schema() {
        let schema = schema("registry.json");
        for shape in shapes() {
            let definition = if shape.pointer.is_empty() {
                &schema
            } else {
                schema.pointer(&shape.pointer).expect("definition")
            };
            assert_eq!(
                definition["additionalProperties"],
                json!(false),
                "registry.json#{}: the Rust type denies unknown fields",
                shape.pointer
            );
        }
    }

    #[test]
    fn every_value_type_validates_and_the_schema_lists_no_other() {
        let schema = schema("registry.json");
        let own = narrowed(&schema, "/$defs/ValueType");
        let mut written = BTreeSet::new();

        for ty in value_type_examples() {
            let value = to_value(&ty);
            assert_valid(&own, &value, &format!("{ty:?}"));
            assert_eq!(
                serde_json::from_value::<ValueType>(value.clone()).expect("reads back"),
                ty
            );
            written.insert(match &value {
                Value::String(name) => name.clone(),
                Value::Object(map) => map.keys().next().expect("one key").clone(),
                other => panic!("unexpected ValueType encoding {other}"),
            });
        }

        let variants = serde_variants::<ValueType>(json!("?"));
        assert_eq!(written, variants, "a ValueType variant has no example");
        let branches = def(&schema, "ValueType")["oneOf"]
            .as_array()
            .expect("ValueType is a oneOf");
        let mut in_schema: BTreeSet<String> = branches[0]["enum"]
            .as_array()
            .expect("unit variants")
            .iter()
            .map(|name| name.as_str().expect("string").to_owned())
            .collect();
        in_schema.extend(property_keys(&branches[1]));
        assert_eq!(
            in_schema, variants,
            "the ValueType definition lists other variants"
        );
    }

    #[test]
    fn param_sources_match() {
        let schema = schema("registry.json");
        let in_schema: BTreeSet<String> = def(&schema, "ParamSource")["enum"]
            .as_array()
            .expect("enum")
            .iter()
            .map(|name| name.as_str().expect("string").to_owned())
            .collect();

        assert_eq!(in_schema, serde_variants::<ParamSource>(json!("?")));
    }

    #[test]
    fn every_registry_document_in_the_repository_validates() {
        let validator = validator(&schema("registry.json"));
        let mut documents = vec![
            (
                "crates/cleverhans-demo/registry.json".to_owned(),
                read_json("crates/cleverhans-demo/registry.json"),
            ),
            (
                "typescript/create-cleverhans/templates/shared/registry.json".to_owned(),
                read_json("typescript/create-cleverhans/templates/shared/registry.json"),
            ),
        ];
        for entry in std::fs::read_dir(repo_path("spec/vectors/fixtures")).expect("fixtures") {
            let name = entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned();
            let fixture = read_json(&format!("spec/vectors/fixtures/{name}"));
            documents.push((
                format!("spec/vectors/fixtures/{name}#/registry"),
                fixture["registry"].clone(),
            ));
        }
        assert!(documents.len() >= 3, "no fixtures found");

        for (name, document) in documents {
            assert_valid(&validator, &document, &name);
            RegistrySchema::from_json(&document.to_string())
                .unwrap_or_else(|err| panic!("{name} does not load: {err}"));
        }
    }

    #[test]
    fn the_demo_registry_names_the_schema_by_a_path_that_exists() {
        let document = read_json("crates/cleverhans-demo/registry.json");
        let reference = document["$schema"].as_str().expect("demo names its schema");

        let target = repo_path("crates/cleverhans-demo").join(reference);
        assert_eq!(
            std::fs::canonicalize(&target).expect("the $schema path exists"),
            std::fs::canonicalize(repo_path("spec/schemas/registry.json")).expect("schema"),
        );
    }

    /// A small valid document the tables below edit.
    fn base() -> Value {
        json!({
            "spec_version": "0.1",
            "blocks": [
                { "block_type": "confirm",
                  "slots": [ { "name": "title", "type": "string", "required": true } ] }
            ],
            "actions": [
                { "id": "record.remove",
                  "description": "Remove the selected record",
                  "params": [
                      { "name": "recordId", "type": "string", "source": "context",
                        "required": true },
                      { "name": "mode", "description": "How to remove",
                        "type": { "string_enum": ["soft", "hard"] },
                        "source": "utterance", "required": false }
                  ],
                  "block_type": "confirm",
                  "mutates": true,
                  "authz_key": "record.remove",
                  "display": { "title": "Remove record" } }
            ],
            "context_params": { "recordId": "selected_record_id" }
        })
    }

    fn edited(edit: impl FnOnce(&mut Value)) -> Value {
        let mut document = base();
        edit(&mut document);
        document
    }

    fn set(pointer: &'static str, value: Value) -> impl FnOnce(&mut Value) {
        move |document: &mut Value| {
            *document
                .pointer_mut(pointer)
                .unwrap_or_else(|| panic!("no {pointer} in the base document")) = value;
        }
    }

    fn insert(pointer: &'static str, key: &'static str, value: Value) -> impl FnOnce(&mut Value) {
        move |document: &mut Value| {
            document
                .pointer_mut(pointer)
                .and_then(Value::as_object_mut)
                .unwrap_or_else(|| panic!("no object at {pointer}"))
                .insert(key.to_owned(), value);
        }
    }

    fn remove(pointer: &'static str, key: &'static str) -> impl FnOnce(&mut Value) {
        move |document: &mut Value| {
            let removed = document
                .pointer_mut(pointer)
                .and_then(Value::as_object_mut)
                .unwrap_or_else(|| panic!("no object at {pointer}"))
                .remove(key);
            assert!(removed.is_some(), "no {key} at {pointer}");
        }
    }

    #[test]
    fn invalid_documents_are_rejected_by_the_schema_and_by_from_json() {
        let action = "/actions/0";
        let param = "/actions/0/params/0";
        let slot = "/blocks/0/slots/0";
        let cases: Vec<(&str, Value)> = vec![
            // An unknown field at each level.
            (
                "unknown top-level field",
                edited(insert("", "extra", json!(1))),
            ),
            (
                "unknown block field",
                edited(insert("/blocks/0", "extra", json!(1))),
            ),
            (
                "unknown slot field",
                edited(insert(slot, "extra", json!(1))),
            ),
            (
                "unknown action field",
                edited(insert(action, "extra", json!(1))),
            ),
            (
                "unknown param field",
                edited(insert(param, "extra", json!(1))),
            ),
            (
                "unknown display field",
                edited(insert("/actions/0/display", "extra", json!(1))),
            ),
            (
                "unknown key beside string_enum",
                edited(insert("/actions/0/params/1/type", "extra", json!(1))),
            ),
            (
                "misspelled field",
                edited(|doc| {
                    remove(action, "mutates")(doc);
                    insert(action, "mutatess", json!(true))(doc);
                }),
            ),
            // Wrong value types.
            (
                "spec_version not a string",
                edited(set("/spec_version", json!(1))),
            ),
            (
                "$schema not a string",
                edited(insert("", "$schema", json!(1))),
            ),
            ("blocks not an array", edited(set("/blocks", json!({})))),
            (
                "mutates not a boolean",
                edited(set("/actions/0/mutates", json!("yes"))),
            ),
            (
                "required not a boolean",
                edited(set("/actions/0/params/0/required", json!(1))),
            ),
            (
                "unknown value type",
                edited(set("/actions/0/params/0/type", json!("date"))),
            ),
            (
                "value type in another case",
                edited(set("/actions/0/params/0/type", json!("String"))),
            ),
            (
                "string_enum not an array",
                edited(set(
                    "/actions/0/params/1/type",
                    json!({ "string_enum": "soft" }),
                )),
            ),
            (
                "string_enum of numbers",
                edited(set(
                    "/actions/0/params/1/type",
                    json!({ "string_enum": [1] }),
                )),
            ),
            (
                "unit value type as an object",
                edited(set("/actions/0/params/0/type", json!({ "string": [] }))),
            ),
            (
                "unknown source",
                edited(set("/actions/0/params/0/source", json!("model"))),
            ),
            (
                "display title not a string",
                edited(set("/actions/0/display", json!({ "title": 1 }))),
            ),
            (
                "keywords not strings",
                edited(set(
                    "/actions/0/display",
                    json!({ "title": "t", "keywords": [1] }),
                )),
            ),
            (
                "null description",
                edited(set("/actions/0/description", Value::Null)),
            ),
            (
                "context_params not a map",
                edited(set("/context_params", json!([]))),
            ),
            (
                "context path not a string",
                edited(set("/context_params/recordId", json!(1))),
            ),
            // Bad context_params paths.
            (
                "path: unknown field",
                edited(set("/context_params/recordId", json!("selected_record"))),
            ),
            (
                "path: traversal into a scalar field",
                edited(set(
                    "/context_params/recordId",
                    json!("selected_record_id.foo"),
                )),
            ),
            (
                "path: empty params key",
                edited(set("/context_params/recordId", json!("params."))),
            ),
            (
                "path: empty extensions key",
                edited(set("/context_params/recordId", json!("extensions."))),
            ),
            (
                "path: unknown prefix",
                edited(set("/context_params/recordId", json!("para.x"))),
            ),
            (
                "path: bare map",
                edited(set("/context_params/recordId", json!("params"))),
            ),
            (
                "path: empty",
                edited(set("/context_params/recordId", json!(""))),
            ),
            (
                "path: wrong case",
                edited(set("/context_params/recordId", json!("Route"))),
            ),
            (
                "path: leading space",
                edited(set("/context_params/recordId", json!(" route"))),
            ),
            (
                "path: trailing newline",
                edited(set("/context_params/recordId", json!("route\n"))),
            ),
            // A missing required field at each level.
            ("missing spec_version", edited(remove("", "spec_version"))),
            ("missing blocks", edited(remove("", "blocks"))),
            ("missing actions", edited(remove("", "actions"))),
            (
                "missing block_type",
                edited(remove("/blocks/0", "block_type")),
            ),
            ("missing slots", edited(remove("/blocks/0", "slots"))),
            ("missing slot type", edited(remove(slot, "type"))),
            ("missing slot required", edited(remove(slot, "required"))),
            ("missing action id", edited(remove(action, "id"))),
            (
                "missing action description",
                edited(remove(action, "description")),
            ),
            ("missing action params", edited(remove(action, "params"))),
            (
                "missing action block_type",
                edited(remove(action, "block_type")),
            ),
            ("missing mutates", edited(remove(action, "mutates"))),
            ("missing authz_key", edited(remove(action, "authz_key"))),
            ("missing param name", edited(remove(param, "name"))),
            ("missing param source", edited(remove(param, "source"))),
            ("missing param required", edited(remove(param, "required"))),
            (
                "missing display title",
                edited(remove("/actions/0/display", "title")),
            ),
        ];

        let validator = validator(&schema("registry.json"));
        for (name, document) in cases {
            assert!(
                !validator.is_valid(&document),
                "the schema accepts an invalid document: {name}\n{document:#}"
            );
            assert!(
                RegistrySchema::from_json(&document.to_string()).is_err(),
                "from_json accepts an invalid document: {name}\n{document:#}"
            );
        }
    }

    #[test]
    fn valid_edge_cases_are_accepted_by_the_schema_and_by_from_json() {
        let cases: Vec<(&str, Value)> = vec![
            ("the base document", base()),
            (
                "the smallest document",
                json!({ "spec_version": "0.1", "blocks": [], "actions": [] }),
            ),
            (
                "a draft-suffixed version",
                edited(set("/spec_version", json!("0.1.0-draft"))),
            ),
            (
                "a patch version",
                edited(set("/spec_version", json!("0.1.5"))),
            ),
            (
                "a relative $schema",
                edited(insert(
                    "",
                    "$schema",
                    json!("../../spec/schemas/registry.json"),
                )),
            ),
            (
                "an absolute $schema",
                edited(insert(
                    "",
                    "$schema",
                    json!("https://cleverhans.dev/spec/v0.1/registry.json"),
                )),
            ),
            ("a null $schema", edited(insert("", "$schema", Value::Null))),
            ("no context_params", edited(remove("", "context_params"))),
            (
                "empty context_params",
                edited(set("/context_params", json!({}))),
            ),
            (
                "path: route",
                edited(set("/context_params/recordId", json!("route"))),
            ),
            (
                "path: view_type",
                edited(set("/context_params/recordId", json!("view_type"))),
            ),
            (
                "path: params key",
                edited(set("/context_params/recordId", json!("params.id"))),
            ),
            (
                "path: extensions key",
                edited(set("/context_params/recordId", json!("extensions.tenant"))),
            ),
            (
                "path: a key with dots",
                edited(set("/context_params/recordId", json!("params.a.b"))),
            ),
            (
                "path: a key that is a dot",
                edited(set("/context_params/recordId", json!("params.."))),
            ),
            (
                "path: a key with a space",
                edited(set("/context_params/recordId", json!("extensions. x"))),
            ),
            (
                "path: a key with a newline",
                edited(set("/context_params/recordId", json!("params.a\nb"))),
            ),
            (
                "a mapping for a param no action has",
                edited(insert("/context_params", "unused", json!("route"))),
            ),
            ("no display", edited(remove("/actions/0", "display"))),
            (
                "a null display",
                edited(set("/actions/0/display", Value::Null)),
            ),
            (
                "a full display",
                edited(set(
                    "/actions/0/display",
                    json!({ "title": "t", "description": "d", "keywords": ["k"],
                            "group": "g", "tags": ["ai"] }),
                )),
            ),
            (
                "a null display group",
                edited(set(
                    "/actions/0/display",
                    json!({ "title": "t", "group": null }),
                )),
            ),
            (
                "an empty string_enum",
                edited(set(
                    "/actions/0/params/1/type",
                    json!({ "string_enum": [] }),
                )),
            ),
            (
                "type integer",
                edited(set("/actions/0/params/0/type", json!("integer"))),
            ),
            (
                "type number",
                edited(set("/actions/0/params/0/type", json!("number"))),
            ),
            (
                "type boolean",
                edited(set("/actions/0/params/0/type", json!("boolean"))),
            ),
            (
                "type json",
                edited(set("/actions/0/params/0/type", json!("json"))),
            ),
            (
                "a block without slots",
                edited(set("/blocks/0/slots", json!([]))),
            ),
            (
                "an action without params",
                edited(set("/actions/0/params", json!([]))),
            ),
            (
                "empty strings",
                edited(set("/actions/0/authz_key", json!(""))),
            ),
        ];

        let validator = validator(&schema("registry.json"));
        for (name, document) in cases {
            assert_valid(&validator, &document, name);
            RegistrySchema::from_json(&document.to_string())
                .unwrap_or_else(|err| panic!("from_json rejects a valid document: {name}: {err}"));
        }
    }
}

mod vectors {
    use super::*;

    fn vector_files(dir: &str) -> Vec<(String, Value)> {
        let mut files: Vec<(String, Value)> = std::fs::read_dir(repo_path(dir))
            .unwrap_or_else(|err| panic!("read {dir}: {err}"))
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(".json"))
            .map(|name| {
                let path = format!("{dir}/{name}");
                let value = read_json(&path);
                (path, value)
            })
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files
    }

    /// Replaces `{"$ref": "NAME"}` directives with the string a runner
    /// would substitute (an opaque bound ID).
    fn substitute_refs(value: &Value) -> Value {
        match value {
            Value::Object(map) if map.len() == 1 && map.contains_key("$ref") => {
                json!(format!("bound:{}", map["$ref"].as_str().unwrap_or("?")))
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, child)| (key.clone(), substitute_refs(child)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(substitute_refs).collect()),
            other => other.clone(),
        }
    }

    /// Every client event the vectors send: `steps[].send` (agent layer,
    /// webhook-service layer) and `frames[].json` (binding layer).
    fn sent_client_events() -> Vec<(String, Value)> {
        let mut events = Vec::new();
        for dir in ["spec/vectors/cases", "spec/vectors/webhook/service"] {
            for (path, vector) in vector_files(dir) {
                for (i, step) in vector["steps"].as_array().into_iter().flatten().enumerate() {
                    if let Some(sent) = step.get("send") {
                        events.push((format!("{path} steps[{i}].send"), substitute_refs(sent)));
                    }
                }
                for (i, frame) in vector["frames"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    if let Some(sent) = frame.get("json") {
                        events.push((format!("{path} frames[{i}].json"), substitute_refs(sent)));
                    }
                }
            }
        }
        events
    }

    #[test]
    fn every_sent_client_event_validates_and_deserializes() {
        let validator = validator(&schema("client-event.json"));
        let events = sent_client_events();
        assert!(
            events.len() >= 20,
            "found only {} sent client events",
            events.len()
        );

        for (location, event) in events {
            assert_valid(&validator, &event, &location);
            serde_json::from_value::<ClientEvent>(event)
                .unwrap_or_else(|err| panic!("{location} does not deserialize: {err}"));
        }
    }

    #[test]
    fn every_scripted_server_event_validates() {
        let validator = validator(&schema("server-event.json"));
        let known_states = serde_variants::<ProposalState>(json!("?"));
        let mut count = 0;

        for (path, vector) in vector_files("spec/vectors/client") {
            for (i, step) in vector["steps"].as_array().into_iter().flatten().enumerate() {
                let Some(event) = step.get("server") else {
                    continue;
                };
                let location = format!("{path} steps[{i}].server");
                count += 1;
                assert_valid(&validator, event, &location);
                // The Rust reference reads every scripted event except a
                // state outside spec §7: `ProposalState` is a closed enum,
                // while the wire (and the schema) carry a plain string.
                let unknown_state = event["type"] == "proposal_state_changed"
                    && !event["state"]
                        .as_str()
                        .is_some_and(|state| known_states.contains(state));
                assert_eq!(
                    serde_json::from_value::<ServerEvent>(event.clone()).is_ok(),
                    !unknown_state,
                    "{location}: Rust deserialization"
                );
            }
        }
        assert!(count >= 5, "found only {count} scripted server events");
    }
}
