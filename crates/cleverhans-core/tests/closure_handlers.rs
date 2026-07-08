//! The ergonomic handler surface: closures as handlers (blanket impls) and
//! typed-params adapters over codegen-style structs.

use std::sync::Arc;

use serde_json::json;

use cleverhans_core::JsonMap;
use cleverhans_core::envelope::DryRunPreview;
use cleverhans_core::error::HandlerError;
use cleverhans_core::seams::{ActionHandler, DryRunHandler, typed_dry_run, typed_handler};
use cleverhans_core::slots;

#[derive(Clone, Debug, PartialEq)]
struct User {
    name: String,
}

fn user() -> User {
    User {
        name: "alex".to_owned(),
    }
}

#[derive(serde::Deserialize)]
struct RenameParams {
    #[serde(rename = "documentId")]
    document_id: String,
    title: String,
}

#[tokio::test]
async fn closures_are_action_handlers() {
    let handler: Arc<dyn ActionHandler<User>> =
        Arc::new(|params: JsonMap, principal: User| async move {
            Ok(json!({ "title": params["title"], "by": principal.name }))
        });

    let result = handler
        .execute(&slots! { "title": "Roadmap" }, &user())
        .await
        .expect("handler succeeds");

    assert_eq!(result, json!({ "title": "Roadmap", "by": "alex" }));
}

#[tokio::test]
async fn closures_are_dry_run_handlers() {
    let handler: Arc<dyn DryRunHandler<User>> = Arc::new(|_: JsonMap, _: User| async move {
        Ok(DryRunPreview {
            affected_count: 3,
            ..DryRunPreview::default()
        })
    });

    let preview = handler
        .dry_run(&JsonMap::new(), &user())
        .await
        .expect("dry run succeeds");

    assert_eq!(preview.affected_count, 3);
}

#[tokio::test]
async fn typed_handler_deserializes_validated_params() {
    let handler = typed_handler(|params: RenameParams, principal: User| async move {
        Ok(json!({
            "id": params.document_id,
            "title": params.title,
            "by": principal.name,
        }))
    });

    let result = handler
        .execute(
            &slots! { "documentId": "doc-1", "title": "Roadmap" },
            &user(),
        )
        .await
        .expect("typed handler succeeds");

    assert_eq!(
        result,
        json!({ "id": "doc-1", "title": "Roadmap", "by": "alex" })
    );
}

#[tokio::test]
async fn typed_handler_reports_params_drift_as_internal() {
    let handler =
        typed_handler(|params: RenameParams, _: User| async move { Ok(json!(params.title)) });

    // `documentId` missing: the params type no longer matches the registry.
    let err = handler
        .execute(&slots! { "title": "Roadmap" }, &user())
        .await
        .expect_err("drift must fail");

    assert!(
        matches!(&err, HandlerError::Internal(msg) if msg.contains("params type")),
        "drift must be an internal error, got {err:?}"
    );
}

#[tokio::test]
async fn typed_dry_run_deserializes_validated_params() {
    let handler = typed_dry_run(|params: RenameParams, _: User| async move {
        Ok(DryRunPreview {
            affected_count: 1,
            sample_ids: vec![params.document_id],
            summary: Some(params.title),
            extensions: JsonMap::new(),
        })
    });

    let preview = handler
        .dry_run(
            &slots! { "documentId": "doc-1", "title": "Roadmap" },
            &user(),
        )
        .await
        .expect("typed dry run succeeds");

    assert_eq!(preview.sample_ids, vec!["doc-1".to_owned()]);
    assert_eq!(preview.summary.as_deref(), Some("Roadmap"));
}
