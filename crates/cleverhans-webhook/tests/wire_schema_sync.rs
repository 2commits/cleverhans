//! Keeps the serde wire types and `spec/webhook/schemas/*.json` in sync:
//! serialized request bodies must carry exactly the schema's required key
//! set, and response bodies must deserialize from schema-shaped documents.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use cleverhans_webhook::wire::{
    AuthorizeResponse, BuildSlotsRequest, BuildSlotsResponse, Decision, DryRunResponse,
    ExecuteRequest, ExecuteResponse, SeamKind, SeamRequest, VerifySessionRequest,
    VerifySessionResponse, WEBHOOK_VERSION,
};

fn schema(name: &str) -> Value {
    let path = format!(
        "{}/../../spec/webhook/schemas/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("read {path}: {err}");
    }))
    .expect("schema parses")
}

fn required_keys(schema: &Value) -> BTreeSet<String> {
    schema["required"]
        .as_array()
        .expect("schema has required")
        .iter()
        .map(|key| key.as_str().expect("string key").to_owned())
        .collect()
}

fn body_keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("body is an object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn request_bodies_match_the_schema_required_sets() {
    let verify = serde_json::to_value(VerifySessionRequest {
        webhook_version: WEBHOOK_VERSION,
        session_id: "s_1".to_owned(),
        headers: [("cookie".to_owned(), "sid=x".to_owned())].into(),
    })
    .expect("serializes");
    assert_eq!(
        body_keys(&verify),
        required_keys(&schema("verify_session.request"))
    );

    let seam = serde_json::to_value(SeamRequest {
        webhook_version: WEBHOOK_VERSION,
        kind: SeamKind::Authorize,
        session_id: "s_1".to_owned(),
        action_id: "a.b".to_owned(),
        params: cleverhans_core::JsonMap::new(),
        principal: json!({}),
    })
    .expect("serializes");
    assert_eq!(
        body_keys(&seam),
        required_keys(&schema("authorize.request"))
    );
    assert_eq!(body_keys(&seam), required_keys(&schema("dry_run.request")));

    let execute = serde_json::to_value(ExecuteRequest {
        webhook_version: WEBHOOK_VERSION,
        kind: SeamKind::Execute,
        session_id: "s_1".to_owned(),
        action_id: "a.b".to_owned(),
        params: cleverhans_core::JsonMap::new(),
        principal: json!({}),
        idempotency_key: "ik_1".to_owned(),
        attempt: 1,
    })
    .expect("serializes");
    assert_eq!(
        body_keys(&execute),
        required_keys(&schema("execute.request"))
    );

    let build_slots = serde_json::to_value(BuildSlotsRequest {
        webhook_version: WEBHOOK_VERSION,
        kind: SeamKind::BuildSlots,
        session_id: "s_1".to_owned(),
        action_id: "a.b".to_owned(),
        params: cleverhans_core::JsonMap::new(),
        principal: json!({}),
        preview: None,
    })
    .expect("serializes");
    assert_eq!(
        body_keys(&build_slots),
        required_keys(&schema("build_slots.request"))
    );
}

#[test]
fn response_bodies_deserialize_from_schema_shaped_documents() {
    let verify: VerifySessionResponse =
        serde_json::from_value(json!({"principal": {"user_id": "u"}})).expect("verify response");
    assert_eq!(verify.principal, json!({"user_id": "u"}));

    let allow: AuthorizeResponse =
        serde_json::from_value(json!({"decision": "allow"})).expect("allow");
    assert_eq!(allow.decision, Decision::Allow);
    let deny: AuthorizeResponse =
        serde_json::from_value(json!({"decision": "deny", "reason": "r"})).expect("deny");
    assert_eq!(deny.decision, Decision::Deny);

    let preview: DryRunResponse =
        serde_json::from_value(json!({"outcome": "preview", "preview": {}})).expect("preview");
    assert!(matches!(preview, DryRunResponse::Preview { .. }));
    let rejected: DryRunResponse =
        serde_json::from_value(json!({"outcome": "rejected"})).expect("rejected");
    assert!(matches!(
        rejected,
        DryRunResponse::Rejected { reason: None }
    ));

    let executed: ExecuteResponse =
        serde_json::from_value(json!({"outcome": "executed", "result": {"ok": true}}))
            .expect("executed");
    assert!(matches!(executed, ExecuteResponse::Executed { .. }));
    let no_result: ExecuteResponse =
        serde_json::from_value(json!({"outcome": "executed"})).expect("executed without result");
    assert!(matches!(
        no_result,
        ExecuteResponse::Executed {
            result: Value::Null
        }
    ));

    let build_slots: BuildSlotsResponse =
        serde_json::from_value(json!({"slots": {"title": "Rename"}})).expect("slots response");
    assert_eq!(build_slots.slots["title"], json!("Rename"));
}
