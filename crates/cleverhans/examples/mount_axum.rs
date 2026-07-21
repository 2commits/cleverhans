//! Mounting CleverHans into an existing axum app — the complete wiring in
//! one file. Runs offline against a scripted model:
//!
//! ```text
//! cargo run -p cleverhans --example mount_axum --features "ws test-util"
//! ```
//!
//! then connect a WebSocket client to ws://127.0.0.1:8790/agent (the
//! `@cleverhans/react` transport, `websocat`, ...). For a live model swap
//! [`ScriptedLlm`] for `cleverhans::llm::from_env()?` (features `anthropic`
//! / `ollama`).

use std::sync::Arc;

use axum::{Router, routing::get};

use cleverhans::prelude::*;
use cleverhans::seams::CompletionItem;
use cleverhans::test_util::ScriptedLlm;

/// Your app's authenticated user, exactly as your auth middleware already
/// produces it. The framework never invents its own identity model.
#[derive(Clone)]
struct User {
    name: String,
}

/// The registry document — usually `include_str!("registry.json")`; inline
/// here so the example is one file. One mutating action on the selected
/// record, its param filled from app context (never by the model).
const REGISTRY: &str = r#"{
  "spec_version": "0.1",
  "blocks": [
    {
      "block_type": "confirm",
      "slots": [{ "name": "title", "type": "string", "required": true }]
    }
  ],
  "actions": [
    {
      "id": "record.archive",
      "description": "Archive the currently selected record",
      "params": [
        { "name": "recordId", "type": "string", "source": "context", "required": true }
      ],
      "block_type": "confirm",
      "mutates": true,
      "authz_key": "record.archive"
    }
  ],
  "context_params": { "recordId": "selected_record_id" }
}"#;

fn agent() -> Arc<Agent<User>> {
    let schema = RegistrySchema::from_json(REGISTRY).expect("registry document is valid");
    // Unmapped context params fail here, at startup — not per proposal.
    let context_resolver = schema
        .context_resolver()
        .expect("every context param is mapped");

    let registry = RegistryBuilder::from_schema(schema)
        .bind("record.archive", |action| {
            action
                .handler(|params: JsonMap, user: User| async move {
                    // Your app's normal, already-authorized execution path.
                    let id = params["recordId"].as_str().unwrap_or("?");
                    Ok(serde_json::json!({ "archived": id, "by": user.name }))
                })
                .dry_run(|params: JsonMap, _: User| async move {
                    Ok(DryRunPreview {
                        affected_count: 1,
                        sample_ids: params["recordId"]
                            .as_str()
                            .map(String::from)
                            .into_iter()
                            .collect(),
                        summary: Some("archive the selected record".to_owned()),
                        ..DryRunPreview::default()
                    })
                })
                .static_slots(slots! { "title": "Archive record" })
        })
        .build()
        .expect("registry is valid");

    // Scripted model: every user message proposes `record.archive`. Swap for
    // `cleverhans::llm::from_env()?` to talk to Anthropic or Ollama.
    let llm = Arc::new(ScriptedLlm::new(std::iter::repeat_n(
        vec![CompletionItem::ToolCall {
            name: "record.archive".to_owned(),
            arguments: JsonMap::new(),
        }],
        64,
    )));

    Arc::new(Agent::new(
        Arc::new(registry),
        llm,
        Arc::new(AllowAll), // swap for a closure over your permission system
        Arc::new(context_resolver),
    ))
}

#[tokio::main]
async fn main() {
    // Stand-in for your real auth middleware: whatever inserts your user
    // type as an axum Extension already works as the session principal.
    let fake_auth = axum::middleware::from_fn(
        |mut req: axum::extract::Request, next: axum::middleware::Next| async {
            req.extensions_mut().insert(User {
                name: "alex".to_owned(),
            });
            next.run(req).await
        },
    );

    // Your existing app + one merge line.
    let app = Router::new()
        .route("/", get(|| async { "the rest of your app" }))
        .merge(cleverhans::ws::agent_router_from_extension(
            "/agent",
            agent(),
        ))
        .layer(fake_auth);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8790")
        .await
        .expect("bind 127.0.0.1:8790");
    println!("envelope stream at ws://127.0.0.1:8790/agent");
    axum::serve(listener, app).await.expect("serve");
}
