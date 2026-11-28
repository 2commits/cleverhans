//! Keeps the JSON Schemas in `spec/` growing additively (SPEC.md §13): a
//! schema may differ from its snapshot in `spec/baseline/` — what the last
//! release had — only by additions, unless `spec/baseline/breaks.json`
//! accepts the break with a reason.
//!
//! - [`compare`] is a pure function from (baseline, current) to the breaks
//!   between them, each a JSON Pointer and a [`Kind`]; the `rule` module
//!   tests it case by case, in the allowed and the refused direction.
//! - One test per schema folder runs it over the real files and prints
//!   every break that `breaks.json` does not accept, with what to do. An
//!   accepted break the comparison no longer finds fails as a stale entry.
//! - `breaks.json` is checked against its own small schema, and every file
//!   in `spec/baseline/` has a counterpart under `spec/` unless its removal
//!   is accepted.
//!
//! The baseline is refreshed with `scripts/snapshot-schemas.sh` when a
//! release is cut (docs/releasing.md).
//!
//! # The rule
//!
//! Allowed, anywhere: a new key under `properties` that is not in
//! `required`; a new key under `$defs`; a new alternative in `oneOf` or
//! `anyOf`; a new value in `enum`; any change to an annotation (`title`,
//! `description`, `$comment`, `examples`, `default`, `deprecated`); a new
//! order of `required`, `enum`, a `type` list or the alternatives. A node
//! the baseline does not have (a new property, definition or alternative)
//! is not compared at all, and neither is a new schema file.
//!
//! Every other difference is a break. A pointer names the place in the
//! baseline file (with baseline array indices); a break about a name the
//! baseline lacks extends that path by the name.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// The schema folders under `spec/` that `spec/baseline/` mirrors.
const FOLDERS: [&str; 2] = ["schemas", "webhook/schemas"];
const BASELINE: &str = "spec/baseline";
const BREAKS: &str = "spec/baseline/breaks.json";
const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Keywords that never change what validates.
const ANNOTATIONS: [&str; 6] = [
    "title",
    "description",
    "$comment",
    "examples",
    "default",
    "deprecated",
];
/// Keywords whose value is one subschema, compared by the same rule.
const SUBSCHEMA: [&str; 12] = [
    "items",
    "additionalItems",
    "additionalProperties",
    "unevaluatedItems",
    "unevaluatedProperties",
    "propertyNames",
    "contains",
    "not",
    "if",
    "then",
    "else",
    "contentSchema",
];
/// Keywords whose value is a list of subschemas that all apply: compared
/// position by position, and a different length is a break.
const SUBSCHEMA_LISTS: [&str; 2] = ["allOf", "prefixItems"];
/// Keywords whose value maps names to subschemas that constrain the
/// instance: a key added or removed is a break.
const SUBSCHEMA_MAPS: [&str; 2] = ["patternProperties", "dependentSchemas"];

/// What kind of non-additive change a break is. The names are the `kind`
/// values of `breaks.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// A schema file of the baseline is missing now.
    FileRemoved,
    /// A key under `properties` is gone.
    PropertyRemoved,
    /// A key under `$defs` is gone.
    DefRemoved,
    /// A `oneOf`/`anyOf` alternative is gone.
    AlternativeRemoved,
    /// An `enum` value is gone.
    EnumValueRemoved,
    /// A property the baseline has (or names) is now in `required`.
    MadeRequired,
    /// A name the baseline lists in `required` is no longer there.
    MadeOptional,
    /// A property the baseline does not have is added as required.
    NewRequiredProperty,
    /// A validation keyword the baseline does not have.
    KeywordAdded,
    /// A validation keyword of the baseline is gone.
    KeywordRemoved,
    /// A validation keyword has another value.
    KeywordChanged,
}

impl Kind {
    const ALL: [Self; 11] = [
        Self::FileRemoved,
        Self::PropertyRemoved,
        Self::DefRemoved,
        Self::AlternativeRemoved,
        Self::EnumValueRemoved,
        Self::MadeRequired,
        Self::MadeOptional,
        Self::NewRequiredProperty,
        Self::KeywordAdded,
        Self::KeywordRemoved,
        Self::KeywordChanged,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::FileRemoved => "file-removed",
            Self::PropertyRemoved => "property-removed",
            Self::DefRemoved => "def-removed",
            Self::AlternativeRemoved => "alternative-removed",
            Self::EnumValueRemoved => "enum-value-removed",
            Self::MadeRequired => "made-required",
            Self::MadeOptional => "made-optional",
            Self::NewRequiredProperty => "new-required-property",
            Self::KeywordAdded => "keyword-added",
            Self::KeywordRemoved => "keyword-removed",
            Self::KeywordChanged => "keyword-changed",
        }
    }

    fn meaning(self) -> &'static str {
        match self {
            Self::FileRemoved => "the schema file is gone",
            Self::PropertyRemoved => "a property is gone",
            Self::DefRemoved => "a definition is gone",
            Self::AlternativeRemoved => "an alternative (message type, variant) is gone",
            Self::EnumValueRemoved => "an enum value is gone",
            Self::MadeRequired => "an optional property is now required",
            Self::MadeOptional => "a required property is now optional",
            Self::NewRequiredProperty => "a new property is required in an existing shape",
            Self::KeywordAdded => "a new validation keyword constrains an existing shape",
            Self::KeywordRemoved => "a validation keyword is gone",
            Self::KeywordChanged => "a validation keyword has another value",
        }
    }

    fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// One non-additive difference inside a schema.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Break {
    pointer: String,
    kind: Kind,
}

impl Break {
    fn new(pointer: &str, kind: Kind) -> Self {
        Self {
            pointer: pointer.to_owned(),
            kind,
        }
    }
}

/// Every break between a baseline schema and the current one, sorted by
/// pointer. Both are valid draft 2020-12 schemas; a keyword of the wrong
/// JSON type reads as empty.
fn compare(baseline: &Value, current: &Value) -> Vec<Break> {
    let mut breaks = Vec::new();
    compare_schema(baseline, current, "", &mut breaks);
    breaks.sort();
    breaks
}

/// `pointer` extended by one reference token (RFC 6901 escaping).
fn child(pointer: &str, token: &str) -> String {
    format!("{pointer}/{}", token.replace('~', "~0").replace('/', "~1"))
}

/// A schema as a keyword map; `true` is the empty schema. `false` and
/// non-schemas have no map and are compared by value.
fn schema_map<'a>(
    node: &'a Value,
    empty: &'a Map<String, Value>,
) -> Option<&'a Map<String, Value>> {
    match node {
        Value::Object(map) => Some(map),
        Value::Bool(true) => Some(empty),
        _ => None,
    }
}

fn compare_schema(base: &Value, cur: &Value, at: &str, out: &mut Vec<Break>) {
    let empty = Map::new();
    let (Some(base), Some(cur)) = (schema_map(base, &empty), schema_map(cur, &empty)) else {
        if base != cur {
            out.push(Break::new(at, Kind::KeywordChanged));
        }
        return;
    };

    compare_properties(base, cur, at, out);
    compare_defs(base, cur, at, out);

    let keywords: BTreeSet<&str> = base.keys().chain(cur.keys()).map(String::as_str).collect();
    for keyword in keywords {
        if ANNOTATIONS.contains(&keyword) || matches!(keyword, "properties" | "required" | "$defs")
        {
            continue;
        }
        let here = child(at, keyword);
        match (base.get(keyword), cur.get(keyword)) {
            (Some(b), Some(c)) => compare_keyword(keyword, b, c, &here, out),
            (Some(_), None) => out.push(Break::new(&here, Kind::KeywordRemoved)),
            (None, _) => out.push(Break::new(&here, Kind::KeywordAdded)),
        }
    }
}

fn compare_keyword(keyword: &str, base: &Value, cur: &Value, here: &str, out: &mut Vec<Break>) {
    match keyword {
        "oneOf" | "anyOf" => compare_alternatives(base, cur, here, out),
        "enum" => compare_enum(base, cur, here, out),
        "type" => {
            if type_names(base) != type_names(cur) {
                out.push(Break::new(here, Kind::KeywordChanged));
            }
        }
        k if SUBSCHEMA.contains(&k) => compare_schema(base, cur, here, out),
        k if SUBSCHEMA_LISTS.contains(&k) => match (base.as_array(), cur.as_array()) {
            (Some(b), Some(c)) if b.len() == c.len() => {
                for (i, (b, c)) in b.iter().zip(c).enumerate() {
                    compare_schema(b, c, &child(here, &i.to_string()), out);
                }
            }
            _ => out.push(Break::new(here, Kind::KeywordChanged)),
        },
        k if SUBSCHEMA_MAPS.contains(&k) => match (base.as_object(), cur.as_object()) {
            (Some(b), Some(c)) => {
                let keys: BTreeSet<&String> = b.keys().chain(c.keys()).collect();
                for key in keys {
                    let at = child(here, key);
                    match (b.get(key), c.get(key)) {
                        (Some(b), Some(c)) => compare_schema(b, c, &at, out),
                        (Some(_), None) => out.push(Break::new(&at, Kind::KeywordRemoved)),
                        (None, _) => out.push(Break::new(&at, Kind::KeywordAdded)),
                    }
                }
            }
            _ => {
                if base != cur {
                    out.push(Break::new(here, Kind::KeywordChanged));
                }
            }
        },
        _ => {
            if base != cur {
                out.push(Break::new(here, Kind::KeywordChanged));
            }
        }
    }
}

fn object_keyword<'a>(
    schema: &'a Map<String, Value>,
    keyword: &str,
) -> Option<&'a Map<String, Value>> {
    schema.get(keyword).and_then(Value::as_object)
}

fn required_names(schema: &Map<String, Value>) -> BTreeSet<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

/// `properties` and `required` of one shape. A removed property that was
/// required is reported once, as `property-removed`.
fn compare_properties(
    base: &Map<String, Value>,
    cur: &Map<String, Value>,
    at: &str,
    out: &mut Vec<Break>,
) {
    let empty = Map::new();
    let base_props = object_keyword(base, "properties").unwrap_or(&empty);
    let cur_props = object_keyword(cur, "properties").unwrap_or(&empty);
    let props_at = child(at, "properties");

    for (name, b) in base_props {
        let here = child(&props_at, name);
        match cur_props.get(name) {
            Some(c) => compare_schema(b, c, &here, out),
            None => out.push(Break::new(&here, Kind::PropertyRemoved)),
        }
    }

    let (base_required, cur_required) = (required_names(base), required_names(cur));
    for name in cur_required.difference(&base_required) {
        let kind = if cur_props.contains_key(*name) && !base_props.contains_key(*name) {
            Kind::NewRequiredProperty
        } else {
            Kind::MadeRequired
        };
        out.push(Break::new(&child(&props_at, name), kind));
    }
    for name in base_required.difference(&cur_required) {
        if base_props.contains_key(*name) && !cur_props.contains_key(*name) {
            continue;
        }
        out.push(Break::new(&child(&props_at, name), Kind::MadeOptional));
    }
}

fn compare_defs(
    base: &Map<String, Value>,
    cur: &Map<String, Value>,
    at: &str,
    out: &mut Vec<Break>,
) {
    let empty = Map::new();
    let cur_defs = object_keyword(cur, "$defs").unwrap_or(&empty);
    let defs_at = child(at, "$defs");
    for (name, b) in object_keyword(base, "$defs").unwrap_or(&empty) {
        let here = child(&defs_at, name);
        match cur_defs.get(name) {
            Some(c) => compare_schema(b, c, &here, out),
            None => out.push(Break::new(&here, Kind::DefRemoved)),
        }
    }
}

fn type_names(value: &Value) -> BTreeSet<String> {
    match value {
        Value::String(name) => BTreeSet::from([name.clone()]),
        Value::Array(names) => names
            .iter()
            .map(|name| {
                name.as_str()
                    .map_or_else(|| name.to_string(), str::to_owned)
            })
            .collect(),
        other => BTreeSet::from([other.to_string()]),
    }
}

fn compare_enum(base: &Value, cur: &Value, here: &str, out: &mut Vec<Break>) {
    let (Some(base), Some(cur)) = (base.as_array(), cur.as_array()) else {
        if base != cur {
            out.push(Break::new(here, Kind::KeywordChanged));
        }
        return;
    };
    for (i, value) in base.iter().enumerate() {
        if !cur.contains(value) {
            out.push(Break::new(
                &child(here, &i.to_string()),
                Kind::EnumValueRemoved,
            ));
        }
    }
}

/// What identifies one alternative across versions, so alternatives match
/// regardless of order: its `$ref`, else the `const` values of its
/// properties (a discriminator such as `outcome`), else its `type`.
fn alternative_key(alternative: &Value) -> Option<String> {
    if let Some(target) = alternative.get("$ref").and_then(Value::as_str) {
        return Some(format!("$ref {target}"));
    }
    let consts: Vec<String> = alternative
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, prop)| prop.get("const").map(|value| format!("{name}={value}")))
        .collect();
    if !consts.is_empty() {
        return Some(format!("const {}", consts.join(",")));
    }
    alternative.get("type").map(|ty| {
        let names: Vec<String> = type_names(ty).into_iter().collect();
        format!("type {}", names.join(","))
    })
}

/// Keys for both lists of alternatives. When an alternative has no key,
/// or two share one, both lists fall back to their positions.
fn alternative_keys(base: &[Value], cur: &[Value]) -> (Vec<String>, Vec<String>) {
    fn keyed(alternatives: &[Value]) -> Option<Vec<String>> {
        let keys: Vec<String> = alternatives
            .iter()
            .map(alternative_key)
            .collect::<Option<_>>()?;
        let unique: BTreeSet<&String> = keys.iter().collect();
        (unique.len() == keys.len()).then_some(keys)
    }
    fn positions(alternatives: &[Value]) -> Vec<String> {
        (0..alternatives.len()).map(|i| format!("#{i}")).collect()
    }
    match (keyed(base), keyed(cur)) {
        (Some(b), Some(c)) => (b, c),
        _ => (positions(base), positions(cur)),
    }
}

fn compare_alternatives(base: &Value, cur: &Value, here: &str, out: &mut Vec<Break>) {
    let (Some(base), Some(cur)) = (base.as_array(), cur.as_array()) else {
        if base != cur {
            out.push(Break::new(here, Kind::KeywordChanged));
        }
        return;
    };
    let (base_keys, cur_keys) = alternative_keys(base, cur);
    for (i, (key, alternative)) in base_keys.iter().zip(base).enumerate() {
        let at = child(here, &i.to_string());
        match cur_keys.iter().position(|k| k == key) {
            Some(j) => compare_schema(alternative, &cur[j], &at, out),
            None => out.push(Break::new(&at, Kind::AlternativeRemoved)),
        }
    }
}

/// A break in one schema file: `schema` is the path under `spec/`, as in
/// `breaks.json`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Located {
    schema: String,
    pointer: String,
    kind: Kind,
}

/// The breaks of one folder: `baseline` and `current` map file names to
/// file contents. A baseline file missing now is `file-removed`; a file new
/// now is fine; a file equal byte for byte is not parsed.
fn compare_folder(
    folder: &str,
    baseline: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> Vec<Located> {
    let mut found = Vec::new();
    for (name, base_text) in baseline {
        let schema = format!("{folder}/{name}");
        let Some(cur_text) = current.get(name) else {
            found.push(Located {
                schema,
                pointer: String::new(),
                kind: Kind::FileRemoved,
            });
            continue;
        };
        if base_text == cur_text {
            continue;
        }
        let base = parse(base_text, &format!("{BASELINE}/{schema}"));
        let cur = parse(cur_text, &format!("spec/{schema}"));
        found.extend(compare(&base, &cur).into_iter().map(|brk| Located {
            schema: schema.clone(),
            pointer: brk.pointer,
            kind: brk.kind,
        }));
    }
    found
}

/// One entry of `breaks.json`.
#[derive(Clone, Debug)]
struct Accepted {
    at: Located,
    reason: String,
    version: String,
}

/// The schema of `breaks.json`: a list of accepted breaks.
fn breaks_schema() -> Value {
    let kinds: Vec<&str> = Kind::ALL.into_iter().map(Kind::name).collect();
    json!({
        "$schema": DRAFT_2020_12,
        "title": "Accepted schema breaks (spec/baseline/breaks.json)",
        "type": "array",
        "items": {
            "type": "object",
            "required": ["schema", "pointer", "kind", "reason", "version"],
            "additionalProperties": false,
            "properties": {
                "schema": {
                    "description": "The schema's path under spec/, e.g. `schemas/client-event.json`.",
                    "type": "string",
                    "pattern": format!("^({})/[^/]+\\.json$", FOLDERS.join("|"))
                },
                "pointer": {
                    "description": "JSON Pointer of the break, as the test prints it; empty for `file-removed`.",
                    "type": "string",
                    "pattern": "^(/([^~/]|~[01])*)*$"
                },
                "kind": { "description": "The kind of break.", "enum": kinds },
                "reason": {
                    "description": "One sentence: why the break is needed.",
                    "type": "string",
                    "pattern": "\\S"
                },
                "version": {
                    "description": "Where it was decided: `spec <version>` for spec/schemas, `webhook <n>` for spec/webhook/schemas.",
                    "type": "string",
                    "pattern": "^(spec (0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)(\\.(0|[1-9][0-9]*))?(-[0-9A-Za-z.-]+)?|webhook [1-9][0-9]*)$"
                }
            }
        }
    })
}

/// Reads `breaks.json`, or every problem with it.
fn parse_breaks(document: &Value) -> Result<Vec<Accepted>, Vec<String>> {
    let validator = jsonschema::draft202012::new(&breaks_schema()).expect("breaks schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(document)
        .map(|err| format!("{err} (at `{}`)", err.instance_path()))
        .collect();
    if !errors.is_empty() {
        return Err(errors);
    }

    let text = |entry: &Value, key: &str| entry[key].as_str().unwrap_or_default().to_owned();
    let entries: Vec<Accepted> = document
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| Accepted {
            at: Located {
                schema: text(entry, "schema"),
                pointer: text(entry, "pointer"),
                kind: Kind::named(entry["kind"].as_str().unwrap_or_default())
                    .expect("the schema lists only known kinds"),
            },
            reason: text(entry, "reason"),
            version: text(entry, "version"),
        })
        .collect();

    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, entry) in entries.iter().enumerate() {
        let at = &entry.at;
        if at.schema.starts_with("webhook/") != entry.version.starts_with("webhook ") {
            problems.push(format!(
                "[{i}] {}: version `{}` must be `webhook <n>` for spec/webhook/schemas and `spec <version>` otherwise",
                at.schema, entry.version
            ));
        }
        if (at.kind == Kind::FileRemoved) != at.pointer.is_empty() {
            problems.push(format!(
                "[{i}] {}: the pointer is empty exactly for `file-removed`",
                at.schema
            ));
        }
        if !seen.insert(at.clone()) {
            problems.push(format!(
                "[{i}] {}#{} {}: listed twice",
                at.schema,
                at.pointer,
                at.kind.name()
            ));
        }
    }
    if problems.is_empty() {
        Ok(entries)
    } else {
        Err(problems)
    }
}

/// The found breaks no entry accepts, and the entries that match no found
/// break (stale).
fn reconcile<'a>(
    found: &'a [Located],
    accepted: &'a [Accepted],
) -> (Vec<&'a Located>, Vec<&'a Accepted>) {
    let unaccepted = found
        .iter()
        .filter(|brk| !accepted.iter().any(|entry| entry.at == **brk))
        .collect();
    let stale = accepted
        .iter()
        .filter(|entry| !found.contains(&entry.at))
        .collect();
    (unaccepted, stale)
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn parse(text: &str, what: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("parse {what}: {err}"))
}

/// The `.json` files directly in `dir`, name → contents; empty when `dir`
/// does not exist.
fn json_files(dir: &Path) -> BTreeMap<String, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return BTreeMap::new();
    };
    entries
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            let name = path
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned();
            (name, read_text(&path))
        })
        .collect()
}

/// Every file under `dir`, as paths relative to it with `/` separators.
fn files_under(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        let name = path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        if path.is_dir() {
            files_under(&path, &format!("{prefix}{name}/"), out);
        } else {
            out.push(format!("{prefix}{name}"));
        }
    }
}

fn in_folder(schema: &str, folder: &str) -> bool {
    schema
        .rsplit_once('/')
        .is_some_and(|(dir, name)| dir == folder && name.ends_with(".json"))
}

fn accepted_breaks() -> Vec<Accepted> {
    let path = repo_path(BREAKS);
    let document = parse(&read_text(&path), BREAKS);
    parse_breaks(&document).unwrap_or_else(|problems| {
        panic!(
            "{BREAKS} is not a valid list of accepted breaks:\n  {}\n\
             Each entry: {{ \"schema\", \"pointer\", \"kind\", \"reason\", \"version\" }} \
             (see spec/schemas/README.md).",
            problems.join("\n  ")
        )
    })
}

/// A `breaks.json` entry to paste for one break.
fn suggested_entry(brk: &Located) -> String {
    let version = if brk.schema.starts_with("webhook/") {
        "webhook <n>"
    } else {
        "spec <version>"
    };
    format!(
        "{{ \"schema\": {}, \"pointer\": {}, \"kind\": \"{}\", \"reason\": \"<one sentence: why the break is needed>\", \"version\": \"{version}\" }}",
        Value::from(brk.schema.as_str()),
        Value::from(brk.pointer.as_str()),
        brk.kind.name()
    )
}

fn report(folder: &str, unaccepted: &[&Located], stale: &[&Accepted]) -> String {
    let mut text = String::new();
    if !unaccepted.is_empty() {
        text.push_str(&format!(
            "spec/{folder}/ changed in a way that is not additive (SPEC.md §13). \
             Compared with the baseline in {BASELINE}/{folder}/ (what the last release had):\n\n"
        ));
        for brk in unaccepted {
            text.push_str(&format!(
                "  spec/{}#{}  {}: {}\n",
                brk.schema,
                brk.pointer,
                brk.kind.name(),
                brk.kind.meaning()
            ));
        }
        text.push_str(
            "\nWhat to do:\n\
             - Fix the schema so it only grows: add optional properties, `$defs` entries, \
             `oneOf`/`anyOf` alternatives or `enum` values, or change annotations \
             (title, description, $comment, examples, default, deprecated).\n\
             - Or, if the break is deliberate and decided, accept it in \
             spec/baseline/breaks.json, one entry per break above, with a one-sentence \
             reason and the version that decides it (see spec/schemas/README.md):\n\n",
        );
        for brk in unaccepted {
            text.push_str(&format!("  {},\n", suggested_entry(brk)));
        }
        text.push('\n');
    }
    if !stale.is_empty() {
        text.push_str(&format!(
            "Stale entries in {BREAKS}: the comparison no longer finds these breaks in \
             spec/{folder}/. Remove them:\n\n"
        ));
        for entry in stale {
            text.push_str(&format!(
                "  {}#{} {} ({}: {})\n",
                entry.at.schema,
                entry.at.pointer,
                entry.at.kind.name(),
                entry.version,
                entry.reason
            ));
        }
    }
    text
}

fn meta_validate(schema: &Value, what: &str) {
    jsonschema::draft202012::meta::validate(schema)
        .unwrap_or_else(|err| panic!("{what} is not valid draft 2020-12: {err}"));
}

/// Runs the rule over one folder of real files.
fn assert_folder_grows(folder: &str) {
    let baseline = json_files(&repo_path(&format!("{BASELINE}/{folder}")));
    assert!(
        !baseline.is_empty(),
        "{BASELINE}/{folder}/ has no schemas; run scripts/snapshot-schemas.sh"
    );
    let current = json_files(&repo_path(&format!("spec/{folder}")));
    for (name, text) in &baseline {
        let what = format!("{BASELINE}/{folder}/{name}");
        meta_validate(&parse(text, &what), &what);
    }
    for (name, text) in &current {
        let what = format!("spec/{folder}/{name}");
        meta_validate(&parse(text, &what), &what);
    }

    let accepted: Vec<Accepted> = accepted_breaks()
        .into_iter()
        .filter(|entry| in_folder(&entry.at.schema, folder))
        .collect();
    let found = compare_folder(folder, &baseline, &current);
    let (unaccepted, stale) = reconcile(&found, &accepted);
    assert!(
        unaccepted.is_empty() && stale.is_empty(),
        "\n{}",
        report(folder, &unaccepted, &stale)
    );
}

mod real_files {
    use super::*;

    #[test]
    fn envelope_and_registry_schemas_only_grow() {
        assert_folder_grows("schemas");
    }

    #[test]
    fn webhook_schemas_only_grow() {
        assert_folder_grows("webhook/schemas");
    }

    #[test]
    fn the_accepted_breaks_file_is_valid() {
        accepted_breaks();
    }

    #[test]
    fn every_baseline_file_has_a_counterpart_or_an_accepted_removal() {
        let accepted = accepted_breaks();
        let mut files = Vec::new();
        files_under(&repo_path(BASELINE), "", &mut files);
        files.sort();

        let mut problems = Vec::new();
        for file in files.iter().filter(|file| *file != "breaks.json") {
            if !FOLDERS.iter().any(|folder| in_folder(file, folder)) {
                problems.push(format!(
                    "{BASELINE}/{file}: not a schema in a checked folder {FOLDERS:?}"
                ));
            } else if !repo_path(&format!("spec/{file}")).is_file()
                && !accepted
                    .iter()
                    .any(|entry| entry.at.schema == *file && entry.at.kind == Kind::FileRemoved)
            {
                problems.push(format!(
                    "{BASELINE}/{file}: spec/{file} does not exist and {BREAKS} accepts no \
                     `file-removed` for it"
                ));
            }
        }
        assert!(
            files.iter().any(|file| file == "breaks.json"),
            "{BREAKS} is missing; it holds the accepted breaks (`[]` when none)"
        );
        assert!(problems.is_empty(), "\n  {}", problems.join("\n  "));
    }
}

/// The rule, case by case.
mod rule {
    use super::*;

    /// A small schema with every construct the rule treats specially.
    fn base() -> Value {
        json!({
            "$schema": DRAFT_2020_12,
            "$id": "https://example.test/s.json",
            "title": "S",
            "description": "A test schema.",
            "type": "object",
            "oneOf": [ { "$ref": "#/$defs/A" }, { "$ref": "#/$defs/B" } ],
            "$defs": {
                "A": {
                    "type": "object",
                    "required": ["type", "a"],
                    "properties": {
                        "type": { "const": "a" },
                        "a": { "type": "string", "pattern": "^x", "description": "A." },
                        "n": { "type": "integer", "minimum": 0, "maximum": 10 },
                        "opt": { "type": ["string", "null"] },
                        "list": { "type": "array", "items": { "type": "string" } },
                        "map": { "type": "object", "additionalProperties": { "type": "string" } },
                        "kind": { "enum": ["x", "y"] },
                        "inner": { "$ref": "#/$defs/B" },
                        "either": { "anyOf": [ { "type": "null" }, { "$ref": "#/$defs/B" } ] },
                        "x/y~z": { "type": "string" }
                    }
                },
                "B": {
                    "type": "object",
                    "required": ["type"],
                    "additionalProperties": false,
                    "properties": {
                        "type": { "const": "b" },
                        "b": { "type": "boolean" }
                    }
                },
                "Open": { "type": "object", "description": "No properties yet." },
                "Response": {
                    "type": "object",
                    "required": ["outcome"],
                    "oneOf": [
                        {
                            "properties": { "outcome": { "const": "ok" }, "value": {} },
                            "required": ["outcome"]
                        },
                        {
                            "properties": {
                                "outcome": { "const": "no" },
                                "reason": { "type": "string" }
                            },
                            "required": ["outcome"]
                        }
                    ]
                }
            }
        })
    }

    fn with(edit: impl FnOnce(&mut Value)) -> Value {
        let mut schema = base();
        edit(&mut schema);
        schema
    }

    fn at<'a>(schema: &'a mut Value, pointer: &str) -> &'a mut Value {
        schema
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("no {pointer} in the base schema"))
    }

    fn insert(schema: &mut Value, pointer: &str, key: &str, value: Value) {
        at(schema, pointer)
            .as_object_mut()
            .unwrap_or_else(|| panic!("no object at {pointer}"))
            .insert(key.to_owned(), value);
    }

    fn remove(schema: &mut Value, pointer: &str, key: &str) {
        let removed = at(schema, pointer)
            .as_object_mut()
            .unwrap_or_else(|| panic!("no object at {pointer}"))
            .remove(key);
        assert!(removed.is_some(), "no {key} at {pointer}");
    }

    fn push(schema: &mut Value, pointer: &str, value: Value) {
        at(schema, pointer)
            .as_array_mut()
            .unwrap_or_else(|| panic!("no array at {pointer}"))
            .push(value);
    }

    fn remove_at(schema: &mut Value, pointer: &str, index: usize) {
        at(schema, pointer)
            .as_array_mut()
            .unwrap_or_else(|| panic!("no array at {pointer}"))
            .remove(index);
    }

    fn set(schema: &mut Value, pointer: &str, value: Value) {
        *at(schema, pointer) = value;
    }

    const A: &str = "/$defs/A";
    const A_PROPS: &str = "/$defs/A/properties";

    /// A named current schema and the breaks expected against [`base`].
    type Case = (&'static str, Value, Vec<(&'static str, Kind)>);

    fn assert_cases(cases: Vec<Case>) {
        let base = base();
        for (name, current, expected) in cases {
            jsonschema::draft202012::meta::validate(&current)
                .unwrap_or_else(|err| panic!("{name}: the case is not a valid schema: {err}"));
            let expected: Vec<Break> = expected
                .into_iter()
                .map(|(pointer, kind)| Break::new(pointer, kind))
                .collect();
            assert_eq!(compare(&base, &current), expected, "{name}");
        }
    }

    #[test]
    fn additions_and_annotation_changes_are_allowed() {
        let cases = vec![
            ("identical", base(), vec![]),
            (
                "a new optional property",
                with(|s| insert(s, A_PROPS, "c", json!({ "type": "string" }))),
                vec![],
            ),
            (
                "a new optional property in a closed shape",
                with(|s| insert(s, "/$defs/B/properties", "c", json!({ "type": "string" }))),
                vec![],
            ),
            (
                "a first property in a shape that had none",
                with(|s| insert(s, "/$defs/Open", "properties", json!({ "c": {} }))),
                vec![],
            ),
            (
                "a new $defs entry, with required properties of its own",
                with(|s| {
                    insert(
                        s,
                        "/$defs",
                        "C",
                        json!({ "type": "object", "required": ["c"],
                                "properties": { "c": { "type": "string" } } }),
                    );
                }),
                vec![],
            ),
            (
                "a new oneOf alternative (a new message type)",
                with(|s| {
                    insert(
                        s,
                        "/$defs",
                        "C",
                        json!({ "type": "object", "required": ["type"],
                                "properties": { "type": { "const": "c" } } }),
                    );
                    push(s, "/oneOf", json!({ "$ref": "#/$defs/C" }));
                }),
                vec![],
            ),
            (
                "a new anyOf alternative",
                with(|s| {
                    push(
                        s,
                        &format!("{A_PROPS}/either/anyOf"),
                        json!({ "type": "string" }),
                    );
                }),
                vec![],
            ),
            (
                "a new inline alternative keyed by a const",
                with(|s| {
                    push(
                        s,
                        "/$defs/Response/oneOf",
                        json!({ "properties": { "outcome": { "const": "later" } },
                                "required": ["outcome", "retry_after"] }),
                    );
                }),
                vec![],
            ),
            (
                "a new enum value",
                with(|s| push(s, &format!("{A_PROPS}/kind/enum"), json!("z"))),
                vec![],
            ),
            (
                "annotations changed, added and removed",
                with(|s| {
                    set(s, "/title", json!("S2"));
                    set(s, "/description", json!("Another text."));
                    remove(s, &format!("{A_PROPS}/a"), "description");
                    insert(s, A, "$comment", json!("note"));
                    insert(s, &format!("{A_PROPS}/n"), "default", json!(3));
                    insert(s, &format!("{A_PROPS}/n"), "examples", json!([1, 2]));
                    insert(s, &format!("{A_PROPS}/opt"), "deprecated", json!(true));
                    insert(
                        s,
                        &format!("{A_PROPS}/list/items"),
                        "description",
                        json!("An item."),
                    );
                    insert(
                        s,
                        &format!("{A_PROPS}/map/additionalProperties"),
                        "title",
                        json!("Value"),
                    );
                }),
                vec![],
            ),
            (
                "required, enum, type list and alternatives reordered",
                with(|s| {
                    set(s, &format!("{A}/required"), json!(["a", "type"]));
                    set(s, &format!("{A_PROPS}/kind/enum"), json!(["y", "x"]));
                    set(s, &format!("{A_PROPS}/opt/type"), json!(["null", "string"]));
                    set(
                        s,
                        "/oneOf",
                        json!([ { "$ref": "#/$defs/B" }, { "$ref": "#/$defs/A" } ]),
                    );
                    let response = at(s, "/$defs/Response/oneOf")
                        .as_array_mut()
                        .expect("array");
                    response.reverse();
                }),
                vec![],
            ),
            (
                "the empty schema written as true",
                with(|s| set(s, "/$defs/Response/oneOf/0/properties/value", json!(true))),
                vec![],
            ),
            (
                "a type written as a one-element list",
                with(|s| set(s, &format!("{A_PROPS}/n/type"), json!(["integer"]))),
                vec![],
            ),
        ];
        assert_cases(cases);
    }

    #[test]
    fn removals_are_breaks() {
        let cases = vec![
            (
                "a property removed",
                with(|s| remove(s, A_PROPS, "n")),
                vec![("/$defs/A/properties/n", Kind::PropertyRemoved)],
            ),
            (
                "a required property removed, from properties and required",
                with(|s| {
                    remove(s, A_PROPS, "a");
                    set(s, &format!("{A}/required"), json!(["type"]));
                }),
                vec![("/$defs/A/properties/a", Kind::PropertyRemoved)],
            ),
            (
                "a required property removed from properties only",
                with(|s| remove(s, A_PROPS, "a")),
                vec![("/$defs/A/properties/a", Kind::PropertyRemoved)],
            ),
            (
                "a property whose name needs escaping",
                with(|s| remove(s, A_PROPS, "x/y~z")),
                vec![("/$defs/A/properties/x~1y~0z", Kind::PropertyRemoved)],
            ),
            (
                "a $defs entry removed",
                with(|s| remove(s, "/$defs", "Open")),
                vec![("/$defs/Open", Kind::DefRemoved)],
            ),
            (
                "a oneOf alternative removed (a message type)",
                with(|s| remove_at(s, "/oneOf", 1)),
                vec![("/oneOf/1", Kind::AlternativeRemoved)],
            ),
            (
                "an anyOf alternative removed",
                with(|s| remove_at(s, &format!("{A_PROPS}/either/anyOf"), 0)),
                vec![(
                    "/$defs/A/properties/either/anyOf/0",
                    Kind::AlternativeRemoved,
                )],
            ),
            (
                "an inline alternative removed",
                with(|s| remove_at(s, "/$defs/Response/oneOf", 0)),
                vec![("/$defs/Response/oneOf/0", Kind::AlternativeRemoved)],
            ),
            (
                "a oneOf alternative pointed at another definition",
                with(|s| set(s, "/oneOf/1", json!({ "$ref": "#/$defs/Open" }))),
                vec![("/oneOf/1", Kind::AlternativeRemoved)],
            ),
            (
                "an enum value removed",
                with(|s| set(s, &format!("{A_PROPS}/kind/enum"), json!(["y", "z"]))),
                vec![("/$defs/A/properties/kind/enum/0", Kind::EnumValueRemoved)],
            ),
        ];
        assert_cases(cases);
    }

    #[test]
    fn changes_to_required_are_breaks() {
        let cases = vec![
            (
                "an optional property made required",
                with(|s| push(s, &format!("{A}/required"), json!("n"))),
                vec![("/$defs/A/properties/n", Kind::MadeRequired)],
            ),
            (
                "a required property made optional",
                with(|s| set(s, &format!("{A}/required"), json!(["type"]))),
                vec![("/$defs/A/properties/a", Kind::MadeOptional)],
            ),
            (
                "a new property that is required",
                with(|s| {
                    insert(s, A_PROPS, "c", json!({ "type": "string" }));
                    push(s, &format!("{A}/required"), json!("c"));
                }),
                vec![("/$defs/A/properties/c", Kind::NewRequiredProperty)],
            ),
            (
                "a new required property in a shape without properties",
                with(|s| {
                    insert(s, "/$defs/Open", "properties", json!({ "c": {} }));
                    insert(s, "/$defs/Open", "required", json!(["c"]));
                }),
                vec![("/$defs/Open/properties/c", Kind::NewRequiredProperty)],
            ),
            (
                "a required name with no property of its own made optional",
                with(|s| remove(s, "/$defs/Response", "required")),
                vec![("/$defs/Response/properties/outcome", Kind::MadeOptional)],
            ),
            (
                "an optional property of an existing alternative made required",
                with(|s| push(s, "/$defs/Response/oneOf/1/required", json!("reason"))),
                vec![(
                    "/$defs/Response/oneOf/1/properties/reason",
                    Kind::MadeRequired,
                )],
            ),
        ];
        assert_cases(cases);
    }

    #[test]
    fn changed_validation_keywords_are_breaks() {
        let changed = Kind::KeywordChanged;
        let cases = vec![
            (
                "a type changed",
                with(|s| set(s, &format!("{A_PROPS}/n/type"), json!("number"))),
                vec![("/$defs/A/properties/n/type", changed)],
            ),
            (
                "a type made nullable",
                with(|s| set(s, &format!("{A_PROPS}/a/type"), json!(["string", "null"]))),
                vec![("/$defs/A/properties/a/type", changed)],
            ),
            (
                "a const changed",
                with(|s| set(s, &format!("{A_PROPS}/type/const"), json!("aa"))),
                vec![("/$defs/A/properties/type/const", changed)],
            ),
            (
                "a pattern changed",
                with(|s| set(s, &format!("{A_PROPS}/a/pattern"), json!("^[xy]"))),
                vec![("/$defs/A/properties/a/pattern", changed)],
            ),
            (
                "a minimum changed",
                with(|s| set(s, &format!("{A_PROPS}/n/minimum"), json!(1))),
                vec![("/$defs/A/properties/n/minimum", changed)],
            ),
            (
                "a maximum raised",
                with(|s| set(s, &format!("{A_PROPS}/n/maximum"), json!(20))),
                vec![("/$defs/A/properties/n/maximum", changed)],
            ),
            (
                "items changed",
                with(|s| set(s, &format!("{A_PROPS}/list/items/type"), json!("integer"))),
                vec![("/$defs/A/properties/list/items/type", changed)],
            ),
            (
                "items replaced",
                with(|s| {
                    set(
                        s,
                        &format!("{A_PROPS}/list/items"),
                        json!({ "$ref": "#/$defs/B" }),
                    )
                }),
                vec![
                    ("/$defs/A/properties/list/items/$ref", Kind::KeywordAdded),
                    ("/$defs/A/properties/list/items/type", Kind::KeywordRemoved),
                ],
            ),
            (
                "a shape closed",
                with(|s| insert(s, A, "additionalProperties", json!(false))),
                vec![("/$defs/A/additionalProperties", Kind::KeywordAdded)],
            ),
            (
                "a closed shape opened",
                with(|s| remove(s, "/$defs/B", "additionalProperties")),
                vec![("/$defs/B/additionalProperties", Kind::KeywordRemoved)],
            ),
            (
                "additionalProperties: false made true",
                with(|s| set(s, "/$defs/B/additionalProperties", json!(true))),
                vec![("/$defs/B/additionalProperties", changed)],
            ),
            (
                "an additionalProperties schema changed",
                with(|s| {
                    set(
                        s,
                        &format!("{A_PROPS}/map/additionalProperties/type"),
                        json!("integer"),
                    );
                }),
                vec![("/$defs/A/properties/map/additionalProperties/type", changed)],
            ),
            (
                "a $ref target changed",
                with(|s| set(s, &format!("{A_PROPS}/inner/$ref"), json!("#/$defs/Open"))),
                vec![("/$defs/A/properties/inner/$ref", changed)],
            ),
            (
                "the $id changed",
                with(|s| set(s, "/$id", json!("https://example.test/v2/s.json"))),
                vec![("/$id", changed)],
            ),
            (
                "the $schema changed",
                with(|s| {
                    set(
                        s,
                        "/$schema",
                        json!("https://json-schema.org/draft/2019-09/schema"),
                    )
                }),
                vec![("/$schema", changed)],
            ),
            (
                "a validation keyword added",
                with(|s| insert(s, &format!("{A_PROPS}/a"), "maxLength", json!(8))),
                vec![("/$defs/A/properties/a/maxLength", Kind::KeywordAdded)],
            ),
            (
                "a validation keyword removed",
                with(|s| remove(s, &format!("{A_PROPS}/n"), "minimum")),
                vec![("/$defs/A/properties/n/minimum", Kind::KeywordRemoved)],
            ),
            (
                "a property schema made false",
                with(|s| set(s, &format!("{A_PROPS}/opt"), json!(false))),
                vec![("/$defs/A/properties/opt", changed)],
            ),
            (
                "an enum added to an open property",
                with(|s| {
                    insert(
                        s,
                        "/$defs/Response/oneOf/0/properties/value",
                        "enum",
                        json!([1]),
                    )
                }),
                vec![(
                    "/$defs/Response/oneOf/0/properties/value/enum",
                    Kind::KeywordAdded,
                )],
            ),
            (
                "a change inside a moved alternative names its baseline position",
                with(|s| {
                    set(
                        s,
                        "/$defs/Response/oneOf/1/properties/reason/type",
                        json!("integer"),
                    );
                    at(s, "/$defs/Response/oneOf")
                        .as_array_mut()
                        .expect("array")
                        .reverse();
                }),
                vec![("/$defs/Response/oneOf/1/properties/reason/type", changed)],
            ),
            (
                "several breaks at once, sorted by pointer",
                with(|s| {
                    remove(s, "/$defs", "Open");
                    remove(s, A_PROPS, "n");
                    set(s, "/$id", json!("https://example.test/v2/s.json"));
                }),
                vec![
                    ("/$defs/A/properties/n", Kind::PropertyRemoved),
                    ("/$defs/Open", Kind::DefRemoved),
                    ("/$id", changed),
                ],
            ),
        ];
        assert_cases(cases);
    }

    #[test]
    fn alternatives_without_keys_match_by_position() {
        let base = json!({ "oneOf": [ { "minimum": 0 }, { "maximum": 9 } ] });

        let appended =
            json!({ "oneOf": [ { "minimum": 0 }, { "maximum": 9 }, { "multipleOf": 2 } ] });
        assert_eq!(compare(&base, &appended), vec![]);

        let removed = json!({ "oneOf": [ { "minimum": 0 } ] });
        assert_eq!(
            compare(&base, &removed),
            vec![Break::new("/oneOf/1", Kind::AlternativeRemoved)]
        );
    }

    fn files(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(name, text)| ((*name).to_owned(), (*text).to_owned()))
            .collect()
    }

    #[test]
    fn a_removed_file_is_a_break_and_a_new_file_is_not() {
        let schema = r#"{ "type": "object" }"#;
        let baseline = files(&[("a.json", schema), ("b.json", schema)]);

        let current = files(&[("a.json", schema), ("b.json", schema), ("c.json", "{}")]);
        assert_eq!(compare_folder("schemas", &baseline, &current), vec![]);

        let current = files(&[("b.json", schema)]);
        assert_eq!(
            compare_folder("schemas", &baseline, &current),
            vec![Located {
                schema: "schemas/a.json".to_owned(),
                pointer: String::new(),
                kind: Kind::FileRemoved,
            }]
        );
    }

    #[test]
    fn a_reformatted_file_is_compared_by_content() {
        let baseline = files(&[("a.json", r#"{ "type": "object" }"#)]);
        let current = files(&[(
            "a.json",
            "{\n  \"type\": \"object\",\n  \"title\": \"A\"\n}\n",
        )]);

        assert_eq!(compare_folder("schemas", &baseline, &current), vec![]);
    }
}

/// `breaks.json` and how its entries meet the breaks found.
mod accepted {
    use super::*;

    fn entry() -> Value {
        json!({
            "schema": "schemas/client-event.json",
            "pointer": "/$defs/Init/properties/context",
            "kind": "property-removed",
            "reason": "The context moves to its own message in 0.2.",
            "version": "spec 0.2.0"
        })
    }

    fn with(edit: impl FnOnce(&mut Map<String, Value>)) -> Value {
        let mut entry = entry();
        edit(entry.as_object_mut().expect("object"));
        json!([entry])
    }

    #[test]
    fn valid_documents_parse() {
        assert_eq!(parse_breaks(&json!([])).expect("empty list").len(), 0);
        let entries = parse_breaks(&json!([entry()])).expect("one entry");
        assert_eq!(entries[0].at.kind, Kind::PropertyRemoved);

        let webhook = json!([{
            "schema": "webhook/schemas/execute.request.json",
            "pointer": "",
            "kind": "file-removed",
            "reason": "Execute moves to a new endpoint.",
            "version": "webhook 2"
        }]);
        assert!(parse_breaks(&webhook).is_ok());
    }

    #[test]
    fn invalid_documents_are_refused() {
        let cases = vec![
            ("not a list", json!({ "breaks": [] })),
            (
                "a missing reason",
                with(|e| {
                    e.remove("reason");
                }),
            ),
            (
                "an empty reason",
                with(|e| {
                    e.insert("reason".into(), json!(" "));
                }),
            ),
            (
                "a missing version",
                with(|e| {
                    e.remove("version");
                }),
            ),
            (
                "an unknown kind",
                with(|e| {
                    e.insert("kind".into(), json!("renamed"));
                }),
            ),
            (
                "an unknown field",
                with(|e| {
                    e.insert("extra".into(), json!(1));
                }),
            ),
            (
                "a schema outside the checked folders",
                with(|e| {
                    e.insert("schema".into(), json!("vectors/x.json"));
                }),
            ),
            (
                "a pointer that is not a JSON Pointer",
                with(|e| {
                    e.insert("pointer".into(), json!("$defs/Init"));
                }),
            ),
            (
                "a version without its family",
                with(|e| {
                    e.insert("version".into(), json!("0.2.0"));
                }),
            ),
            (
                "a webhook version for an envelope schema",
                with(|e| {
                    e.insert("version".into(), json!("webhook 2"));
                }),
            ),
            (
                "a spec version for a webhook schema",
                with(|e| {
                    e.insert(
                        "schema".into(),
                        json!("webhook/schemas/execute.request.json"),
                    );
                }),
            ),
            (
                "file-removed with a pointer",
                with(|e| {
                    e.insert("kind".into(), json!("file-removed"));
                }),
            ),
            (
                "an empty pointer for another kind",
                with(|e| {
                    e.insert("pointer".into(), json!(""));
                }),
            ),
            ("the same break twice", json!([entry(), entry()])),
        ];
        for (name, document) in cases {
            assert!(parse_breaks(&document).is_err(), "accepted: {name}");
        }
    }

    fn located(pointer: &str) -> Located {
        Located {
            schema: "schemas/client-event.json".to_owned(),
            pointer: pointer.to_owned(),
            kind: Kind::PropertyRemoved,
        }
    }

    fn accepting(pointer: &str) -> Accepted {
        Accepted {
            at: located(pointer),
            reason: "Decided.".to_owned(),
            version: "spec 0.2.0".to_owned(),
        }
    }

    #[test]
    fn an_accepted_break_passes_and_an_unaccepted_one_fails() {
        let found = vec![located("/a"), located("/b")];
        let accepted = vec![accepting("/a")];

        let (unaccepted, stale) = reconcile(&found, &accepted);

        assert_eq!(unaccepted, vec![&located("/b")]);
        assert!(stale.is_empty());
    }

    #[test]
    fn an_accepted_break_that_is_not_found_is_stale() {
        let found = vec![located("/a")];
        let accepted = vec![accepting("/a"), accepting("/gone")];

        let (unaccepted, stale) = reconcile(&found, &accepted);

        assert!(unaccepted.is_empty());
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].at, located("/gone"));
    }

    #[test]
    fn an_entry_must_match_the_kind_too() {
        let found = vec![located("/a")];
        let mut other_kind = accepting("/a");
        other_kind.at.kind = Kind::MadeOptional;

        let (unaccepted, stale) = reconcile(&found, std::slice::from_ref(&other_kind));

        assert_eq!(unaccepted.len(), 1);
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn the_report_names_every_break_and_a_ready_entry() {
        let found = located("/$defs/Init/properties/context");
        let stale = accepting("/gone");

        let text = report("schemas", &[&found], &[&stale]);

        assert!(text.contains(
            "spec/schemas/client-event.json#/$defs/Init/properties/context  property-removed"
        ));
        assert!(text.contains(
            r#"{ "schema": "schemas/client-event.json", "pointer": "/$defs/Init/properties/context", "kind": "property-removed","#
        ));
        assert!(text.contains("Stale entries"));
        assert!(text.contains("schemas/client-event.json#/gone property-removed"));
    }
}
