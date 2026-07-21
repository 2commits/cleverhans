//! CleverHans agent assembly — scaffolded starter. Drop into your crate as
//! `src/agent.rs`, swap `Principal` for your real user type, and mount:
//!
//! ```ignore
//! let app = Router::new()
//!     .merge(cleverhans::ws::agent_router_from_extension("/agent", agent()?))
//!     .layer(your_auth_layer); // inserts Extension<Principal>
//! ```

use std::sync::Arc;

use cleverhans::prelude::*;

/// Your app's authenticated user, exactly as your auth middleware produces
/// it. The framework never invents its own identity model.
#[derive(Clone)]
pub struct Principal {
    pub user_id: String,
}

pub fn agent() -> Result<Arc<Agent<Principal>>, Box<dyn std::error::Error>> {
    let schema = RegistrySchema::from_json(include_str!("../cleverhans/registry.json"))?;
    // Unmapped context params fail here, at startup — not per proposal.
    let context_resolver = schema.context_resolver()?;

    let registry = RegistryBuilder::from_schema(schema)
        .bind("record.archive", |action| {
            action
                .handler(|params: JsonMap, principal: Principal| async move {
                    // Your app's normal, already-authorized execution path.
                    let id = params["recordId"].as_str().unwrap_or_default().to_owned();
                    Ok(serde_json::json!({ "archived": id, "by": principal.user_id }))
                })
                .dry_run(|params: JsonMap, _: Principal| async move {
                    Ok(DryRunPreview {
                        affected_count: 1,
                        sample_ids: params["recordId"].as_str().map(String::from).into_iter().collect(),
                        summary: Some("Archive the selected record".to_owned()),
                        ..DryRunPreview::default()
                    })
                })
                .static_slots(slots! { "title": "Archive record" })
        })
        .build()?;

    Ok(Arc::new(Agent::new(
        Arc::new(registry),
        cleverhans::llm::from_env()?, // ANTHROPIC_API_KEY or OLLAMA_MODEL
        Arc::new(AllowAll),           // swap for a closure over your permission system
        Arc::new(context_resolver),
    )))
}
