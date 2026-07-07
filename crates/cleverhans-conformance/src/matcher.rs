//! The vector matching engine. Semantics are normative and documented in
//! `spec/vectors/README.md`:
//!
//! - objects match by subset (additive envelope evolution, spec §13)
//! - arrays match element-wise with exact length
//! - expected lists match actual lists exactly in count and order
//! - directives: `$bind`, `$ref`, `$exact`, `$keys`, `$absent`

use std::collections::BTreeMap;

use serde_json::Value;

/// Values captured by `$bind`, referenced by `$ref`.
#[derive(Debug, Default)]
pub struct Bindings(BTreeMap<String, Value>);

impl Bindings {
    fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }
}

/// Replaces every `{"$ref": "NAME"}` in an outbound payload with the bound
/// value, so vectors stay independent of implementation ID formats.
///
/// # Panics
///
/// On a `$ref` to a never-bound name — a vector-authoring error.
#[must_use]
pub fn substitute(payload: &Value, bindings: &Bindings) -> Value {
    match payload {
        Value::Object(map) => {
            if let Some((key, Value::String(name))) = single_entry(map)
                && key == "$ref"
            {
                return bindings
                    .get(name)
                    .unwrap_or_else(|| panic!("$ref to unbound name `{name}`"))
                    .clone();
            }
            Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), substitute(v, bindings)))
                    .collect(),
            )
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| substitute(v, bindings)).collect())
        }
        other => other.clone(),
    }
}

fn single_entry(map: &serde_json::Map<String, Value>) -> Option<(&str, &Value)> {
    if map.len() == 1 {
        map.iter().next().map(|(k, v)| (k.as_str(), v))
    } else {
        None
    }
}

/// Matches an expected event list against actual events, exactly in count
/// and order, binding `$bind` names along the way.
///
/// # Errors
///
/// A human-readable mismatch description with the matcher path.
pub fn match_events(
    expected: &[Value],
    actual: &[Value],
    bindings: &mut Bindings,
) -> Result<(), String> {
    if expected.len() != actual.len() {
        return Err(format!(
            "expected {} event(s), got {}: {}",
            expected.len(),
            actual.len(),
            serde_json::to_string(actual).unwrap_or_default()
        ));
    }
    for (index, (want, got)) in expected.iter().zip(actual).enumerate() {
        match_value(want, got, bindings, &format!("event[{index}]"))?;
    }
    Ok(())
}

fn match_value(
    expected: &Value,
    actual: &Value,
    bindings: &mut Bindings,
    path: &str,
) -> Result<(), String> {
    if let Value::Object(map) = expected
        && let Some((key, arg)) = single_entry(map)
        && key.starts_with('$')
    {
        return match_directive(key, arg, actual, bindings, path);
    }
    match (expected, actual) {
        (Value::Object(want), Value::Object(got)) => {
            for (key, want_value) in want {
                let field_path = format!("{path}.{key}");
                if is_absent_directive(want_value) {
                    if got.get(key).is_some_and(|v| !v.is_null()) {
                        return Err(format!("{field_path}: expected absent, got {}", got[key]));
                    }
                    continue;
                }
                let Some(got_value) = got.get(key) else {
                    return Err(format!("{field_path}: missing"));
                };
                match_value(want_value, got_value, bindings, &field_path)?;
            }
            Ok(())
        }
        (Value::Array(want), Value::Array(got)) => {
            if want.len() != got.len() {
                return Err(format!(
                    "{path}: expected {} element(s), got {}",
                    want.len(),
                    got.len()
                ));
            }
            for (index, (w, g)) in want.iter().zip(got).enumerate() {
                match_value(w, g, bindings, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        (want, got) if want == got => Ok(()),
        (want, got) => Err(format!("{path}: expected {want}, got {got}")),
    }
}

fn is_absent_directive(value: &Value) -> bool {
    matches!(
        value,
        Value::Object(map) if single_entry(map)
            .is_some_and(|(k, v)| k == "$absent" && *v == Value::Bool(true))
    )
}

fn match_directive(
    key: &str,
    arg: &Value,
    actual: &Value,
    bindings: &mut Bindings,
    path: &str,
) -> Result<(), String> {
    match key {
        "$bind" => {
            let name = arg
                .as_str()
                .ok_or_else(|| format!("{path}: $bind takes a name string"))?;
            bindings.0.insert(name.to_owned(), actual.clone());
            Ok(())
        }
        "$ref" => {
            let name = arg
                .as_str()
                .ok_or_else(|| format!("{path}: $ref takes a name string"))?;
            let bound = bindings
                .get(name)
                .ok_or_else(|| format!("{path}: $ref to unbound name `{name}`"))?;
            if bound == actual {
                Ok(())
            } else {
                Err(format!("{path}: expected bound `{name}` = {bound}, got {actual}"))
            }
        }
        "$exact" => {
            if arg == actual {
                Ok(())
            } else {
                Err(format!("{path}: expected exactly {arg}, got {actual}"))
            }
        }
        "$keys" => {
            let want: Vec<&str> = arg
                .as_array()
                .ok_or_else(|| format!("{path}: $keys takes an array"))?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let got = actual
                .as_object()
                .ok_or_else(|| format!("{path}: $keys expects an object, got {actual}"))?;
            let mut got_keys: Vec<&str> = got.keys().map(String::as_str).collect();
            let mut want_keys = want.clone();
            got_keys.sort_unstable();
            want_keys.sort_unstable();
            if want_keys == got_keys {
                Ok(())
            } else {
                Err(format!(
                    "{path}: expected keys {want_keys:?}, got {got_keys:?}"
                ))
            }
        }
        "$absent" => {
            // Reached only when the value itself is matched (not via an
            // object field); absent-as-field is handled by the caller.
            if actual.is_null() {
                Ok(())
            } else {
                Err(format!("{path}: expected absent, got {actual}"))
            }
        }
        other => Err(format!("{path}: unknown directive `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn matches(expected: Value, actual: Value) -> Result<(), String> {
        match_value(&expected, &actual, &mut Bindings::default(), "root")
    }

    #[test]
    fn objects_match_by_subset() {
        assert!(matches(json!({"a": 1}), json!({"a": 1, "b": 2})).is_ok());
        assert!(matches(json!({"a": 1, "c": 3}), json!({"a": 1})).is_err());
    }

    #[test]
    fn exact_closes_subset_matching() {
        assert!(matches(json!({"$exact": {"a": 1}}), json!({"a": 1, "b": 2})).is_err());
        assert!(matches(json!({"$exact": {"a": 1}}), json!({"a": 1})).is_ok());
    }

    #[test]
    fn keys_pins_the_key_set_without_values() {
        assert!(matches(json!({"$keys": ["a", "b"]}), json!({"b": 0, "a": 9})).is_ok());
        assert!(matches(json!({"$keys": ["a"]}), json!({"a": 1, "b": 2})).is_err());
    }

    #[test]
    fn bind_then_ref_round_trips() {
        let mut bindings = Bindings::default();

        match_value(
            &json!({"id": {"$bind": "P"}}),
            &json!({"id": "prop-1"}),
            &mut bindings,
            "root",
        )
        .expect("bind");

        assert!(
            match_value(
                &json!({"id": {"$ref": "P"}}),
                &json!({"id": "prop-1"}),
                &mut bindings,
                "root",
            )
            .is_ok()
        );
        assert_eq!(
            substitute(&json!({"proposal_id": {"$ref": "P"}}), &bindings),
            json!({"proposal_id": "prop-1"})
        );
    }

    #[test]
    fn absent_accepts_missing_or_null_fields() {
        assert!(matches(json!({"x": {"$absent": true}}), json!({})).is_ok());
        assert!(matches(json!({"x": {"$absent": true}}), json!({"x": null})).is_ok());
        assert!(matches(json!({"x": {"$absent": true}}), json!({"x": 1})).is_err());
    }

    #[test]
    fn arrays_need_exact_length() {
        assert!(matches(json!([1, 2]), json!([1, 2])).is_ok());
        assert!(matches(json!([1]), json!([1, 2])).is_err());
    }
}
