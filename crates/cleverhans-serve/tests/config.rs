//! Startup validation: every config mistake fails fast, named, before the
//! service binds.

use cleverhans_core::schema::RegistrySchema;
use cleverhans_serve::config::{Config, ConfigError};
use serde_json::json;

fn schema() -> RegistrySchema {
    RegistrySchema::from_json(
        &json!({
            "spec_version": "0.1",
            "blocks": [{"block_type": "confirm",
                        "slots": [{"name": "title", "type": "string", "required": true},
                                   {"name": "detail", "type": "string", "required": false}]}],
            "actions": [
                {"id": "record.archive", "description": "Archive the selected record",
                 "params": [{"name": "recordId", "type": "string", "source": "context",
                             "required": true}],
                 "block_type": "confirm", "mutates": true, "authz_key": "record.archive"},
                {"id": "record.open", "description": "Open the selected record",
                 "params": [{"name": "recordId", "type": "string", "source": "context",
                             "required": true}],
                 "block_type": "confirm", "mutates": false, "authz_key": "record.open"}
            ],
            "context_params": {"recordId": "selected_record_id"}
        })
        .to_string(),
    )
    .expect("valid schema")
}

const BASE: &str = r#"
[upstream]
base_url = "http://localhost:3000"
secret_env = "CFG_TEST_SECRET"

[auth]
verify = "POST /internal/cleverhans/verify-session"

[authz]
endpoint = "POST /internal/cleverhans/authorize"

[llm]
provider = "scripted"
script = []
"#;

fn with_actions(actions: &str) -> String {
    format!("{BASE}\n{actions}")
}

fn set_secret() {
    // SAFETY: test-only env mutation; the name is unique to this test file.
    unsafe { std::env::set_var("CFG_TEST_SECRET", "s3cret") };
}

#[test]
fn unknown_fields_fail_fast() {
    let err = Config::from_toml(&format!("{BASE}\ntypo_section = 1")).expect_err("unknown field");
    assert!(err.to_string().contains("typo_section"), "got {err}");
}

#[test]
fn wildcard_covers_every_action_and_explicit_entries_win() {
    set_secret();
    let config = Config::from_toml(&with_actions(
        r#"
[actions."*"]
execute = "POST /hooks/{action}/execute"
dry_run = "POST /hooks/{action}/dry-run"

[actions."record.archive"]
execute = "POST /special/archive"
dry_run = "POST /special/archive/preview"
slots = { title = { const = "Archive record" } }

[actions."record.open".slots]
title = { const = "Open record" }
"#,
    ))
    .expect("parses");
    let resolved = config.resolve(&schema()).expect("resolves");

    assert_eq!(
        resolved.actions["record.archive"].execute.path,
        "/special/archive"
    );
    assert_eq!(
        resolved.actions["record.open"].execute.path,
        "/hooks/record.open/execute"
    );
    assert!(resolved.actions["record.archive"].slots.is_some());
}

#[test]
fn missing_execute_route_names_the_action() {
    set_secret();
    let config = Config::from_toml(&with_actions(
        r#"
[actions."record.archive"]
execute = "POST /a"
dry_run = "POST /a/p"
slots = { title = { const = "t" } }
"#,
    ))
    .expect("parses");
    let err = config
        .resolve(&schema())
        .expect_err("record.open uncovered");
    assert!(err.to_string().contains("record.open"), "got {err}");
}

#[test]
fn mutating_action_without_dry_run_route_is_refused() {
    set_secret();
    let config = Config::from_toml(&with_actions(
        r#"
[actions."*"]
execute = "POST /hooks/{action}"

[actions."record.archive".slots]
title = { const = "t" }
[actions."record.open".slots]
title = { const = "t" }
"#,
    ))
    .expect("parses");
    let err = config.resolve(&schema()).expect_err("no dry_run");
    assert!(
        err.to_string().contains("record.archive") && err.to_string().contains("dry_run"),
        "got {err}"
    );
}

#[test]
fn build_slots_route_resolves_and_skips_slot_coverage() {
    set_secret();
    // No declarative slots for either action: without build_slots this
    // config is rejected (required `title` slot uncovered); with the route
    // the host authors slots at runtime and coverage moves to the
    // propose-time schema check.
    let config = Config::from_toml(&with_actions(
        r#"
[actions."*"]
execute = "POST /hooks/{action}"
dry_run = "POST /hooks/{action}/preview"
build_slots = "POST /hooks/{action}/slots"
"#,
    ))
    .expect("parses");
    let resolved = config
        .resolve(&schema())
        .expect("resolves without slot tables");

    assert_eq!(
        resolved.actions["record.archive"]
            .build_slots
            .as_ref()
            .expect("route")
            .path,
        "/hooks/record.archive/slots"
    );
    assert!(resolved.actions["record.open"].build_slots.is_some());
}

#[test]
fn required_slots_must_be_covered() {
    set_secret();
    let config = Config::from_toml(&with_actions(
        r#"
[actions."*"]
execute = "POST /hooks/{action}"
dry_run = "POST /hooks/{action}/preview"
"#,
    ))
    .expect("parses");
    let err = config.resolve(&schema()).expect_err("title slot uncovered");
    assert!(err.to_string().contains("title"), "got {err}");
}

#[test]
fn unmatched_action_entry_is_refused() {
    set_secret();
    let config = Config::from_toml(&with_actions(
        r#"
[actions."*"]
execute = "POST /hooks/{action}"
dry_run = "POST /hooks/{action}/preview"

[actions."record.archive".slots]
title = { const = "t" }
[actions."record.open".slots]
title = { const = "t" }

[actions."record.ghost"]
execute = "POST /nope"
"#,
    ))
    .expect("parses");
    let err = config.resolve(&schema()).expect_err("ghost entry");
    assert!(err.to_string().contains("record.ghost"), "got {err}");
}

#[test]
fn missing_secret_env_names_the_variable() {
    let toml = BASE.replace("CFG_TEST_SECRET", "CFG_TEST_SECRET_UNSET");
    let config = Config::from_toml(
        &with_actions(
            r#"
[actions."*"]
execute = "POST /hooks/{action}"
dry_run = "POST /hooks/{action}/preview"

[actions."record.archive".slots]
title = { const = "t" }
[actions."record.open".slots]
title = { const = "t" }
"#,
        )
        .replacen(BASE, &toml, 1),
    )
    .expect("parses");
    let err = config.resolve(&schema()).expect_err("missing env");
    assert!(matches!(err, ConfigError::MissingEnv(ref name, _) if name == "CFG_TEST_SECRET_UNSET"));
}

#[test]
fn llm_section_resolves_and_refuses() {
    set_secret();
    let scripted = Config::from_toml(BASE).expect("parses");
    assert!(scripted.llm.resolve().is_ok());

    let bad = Config::from_toml(&BASE.replace(
        "provider = \"scripted\"\nscript = []",
        "provider = \"martian\"",
    ))
    .expect("parses");
    let err = bad.llm.resolve().expect_err("unknown provider");
    assert!(err.to_string().contains("martian"), "got {err}");

    let anthropic = Config::from_toml(&BASE.replace(
        "provider = \"scripted\"\nscript = []",
        "provider = \"anthropic\"\napi_key_env = \"CFG_TEST_NO_SUCH_KEY\"",
    ))
    .expect("parses");
    let err = anthropic.llm.resolve().expect_err("missing key env");
    assert!(
        err.to_string().contains("CFG_TEST_NO_SUCH_KEY"),
        "got {err}"
    );
}

// Endpoint precedence is tested through the pure `resolve_endpoint`:
// mutating the process environment races every other test thread reading
// it (which is why `set_var` is `unsafe` in edition 2024).
mod telemetry_section {
    use super::{BASE, Config};
    use cleverhans_serve::telemetry::resolve_endpoint;

    fn with_telemetry(body: &str) -> Config {
        Config::from_toml(&format!("{BASE}\n[telemetry]\n{body}")).expect("parses")
    }

    #[test]
    fn is_absent_when_the_section_is_omitted() {
        let config = Config::from_toml(BASE).expect("parses");
        assert_eq!(config.telemetry.otlp_endpoint, None, "off by default");
    }

    #[test]
    fn parses_the_export_interval() {
        let config = with_telemetry("export_interval_ms = 500\n");
        assert_eq!(config.telemetry.export_interval_ms, Some(500));
    }

    #[test]
    fn parses_the_configured_endpoint() {
        let config = with_telemetry("otlp_endpoint = \"http://localhost:4318\"\n");
        assert_eq!(
            config.telemetry.otlp_endpoint.as_deref(),
            Some("http://localhost:4318")
        );
    }

    #[test]
    fn resolves_to_none_without_config_or_env() {
        assert_eq!(resolve_endpoint(None, None), None);
    }

    #[test]
    fn resolves_to_the_standard_env_when_config_is_silent() {
        assert_eq!(
            resolve_endpoint(None, Some("http://collector:4318")).as_deref(),
            Some("http://collector:4318")
        );
    }

    #[test]
    fn prefers_the_configured_endpoint_over_the_env() {
        assert_eq!(
            resolve_endpoint(Some("http://localhost:4318"), Some("http://collector:4318"))
                .as_deref(),
            Some("http://localhost:4318")
        );
    }
}

#[test]
fn route_strings_are_validated() {
    set_secret();
    let config = Config::from_toml(&with_actions(
        r#"
[actions."*"]
execute = "not-a-route"
dry_run = "POST /ok"

[actions."record.archive".slots]
title = { const = "t" }
[actions."record.open".slots]
title = { const = "t" }
"#,
    ))
    .expect("parses");
    let err = config.resolve(&schema()).expect_err("bad route");
    assert!(err.to_string().contains("not-a-route"), "got {err}");
}
