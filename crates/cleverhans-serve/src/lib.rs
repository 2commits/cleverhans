//! Assembly of the standalone CleverHans service (spec §10.2): registry +
//! `cleverhans.toml` in, an axum router speaking the WS envelope out, with
//! every app seam reached over the §14 host webhook contract.
//!
//! The library surface exists so integration tests (and unusual hosts) can
//! mount [`build_app`]'s router themselves; the `cleverhans` binary wraps
//! it with config loading, tracing, and the `serve` / `host-check` /
//! `mock-host` subcommands.

pub mod config;
pub mod telemetry;

use std::str::FromStr;
use std::sync::Arc;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use serde_json::Value;

use cleverhans_core::agent::Agent;
use cleverhans_core::async_trait;
use cleverhans_core::declarative::DeclarativeSlots;
use cleverhans_core::registry::RegistryBuilder;
use cleverhans_core::schema::RegistrySchema;
use cleverhans_core::seams::{DryRunHandler, LlmProvider, SlotBuilder};
use cleverhans_webhook::client::HostClient;
use cleverhans_webhook::seams::{
    WebhookAuthz, WebhookDryRun, WebhookHandler, WebhookSlots, WebhookVerifier,
};
use cleverhans_ws::{AsyncPrincipalExtractor, agent_router_async};

use crate::config::Resolved;

/// Assembly errors, all fatal at startup.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Webhook client refusals (spec §14.8) or bad base URL.
    #[error(transparent)]
    Client(#[from] cleverhans_webhook::client::ClientConfigError),
    /// Registry attachment invariants.
    #[error("registry: {0}")]
    Registry(String),
    /// Context-param mapping gaps.
    #[error("registry: {0}")]
    Schema(String),
}

/// Adapts [`WebhookVerifier`] to the WS mount's extractor seam.
struct VerifierExtractor(WebhookVerifier);

#[async_trait]
impl AsyncPrincipalExtractor<Value> for VerifierExtractor {
    async fn extract(&self, headers: &HeaderMap) -> Result<Value, StatusCode> {
        self.0.verify(headers).await.map_err(|status| {
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE)
        })
    }
}

/// Builds the service router: the WS envelope mount at the configured path
/// plus `/healthz`.
///
/// # Errors
///
/// [`BuildError`] on any startup refusal — nothing binds on a bad config.
pub fn build_app(
    resolved: &Resolved,
    schema: &RegistrySchema,
    llm: Arc<dyn LlmProvider>,
) -> Result<Router, BuildError> {
    let client = Arc::new(HostClient::new(resolved.client.clone())?);
    // Every model call is timed via a telemetry event; conversion to OTEL
    // only happens when the metrics layer is installed.
    let llm: Arc<dyn LlmProvider> = Arc::new(crate::telemetry::InstrumentedLlm(llm));

    let mut builder = RegistryBuilder::from_schema(schema.clone());
    for def in &schema.actions {
        let action = resolved.actions.get(&def.id).ok_or_else(|| {
            BuildError::Registry(format!("no resolved routes for action `{}`", def.id))
        })?;
        let dry_run = action.dry_run.clone().map(|route| {
            Arc::new(WebhookDryRun::new(
                Arc::clone(&client),
                def.id.clone(),
                route,
            )) as Arc<dyn DryRunHandler<Value>>
        });
        let handler = Arc::new(WebhookHandler::new(
            Arc::clone(&client),
            def.id.clone(),
            action.execute.clone(),
        )) as Arc<dyn cleverhans_core::seams::ActionHandler<Value>>;
        // §14.9: a build_slots route wins over the declarative slots table.
        builder = if let Some(route) = action.build_slots.clone() {
            let slots = WebhookSlots::new(Arc::clone(&client), def.id.clone(), route);
            builder.bind(def.id.clone(), move |binding| {
                let binding = binding.handler(handler).async_slots(slots);
                match dry_run {
                    Some(dry_run) => binding.dry_run(dry_run),
                    None => binding,
                }
            })
        } else {
            builder.attach(
                def.id.clone(),
                handler,
                dry_run,
                action
                    .slots
                    .clone()
                    .map(|slots| Arc::new(DeclarativeSlots(slots)) as Arc<dyn SlotBuilder>),
            )
        };
    }
    let registry = builder
        .build()
        .map_err(|err| BuildError::Registry(err.to_string()))?;
    let resolver = schema
        .context_resolver()
        .map_err(|err| BuildError::Schema(err.to_string()))?;

    let agent = Arc::new(Agent::with_config(
        Arc::new(registry),
        llm,
        Arc::new(WebhookAuthz::new(
            Arc::clone(&client),
            resolved.authorize.clone(),
        )),
        Arc::new(resolver),
        resolved.agent.clone(),
    ));
    let verifier = WebhookVerifier::new(
        client,
        resolved.verify.clone(),
        resolved.forward_headers.clone(),
    );

    Ok(Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(agent_router_async(
            &resolved.path,
            agent,
            Arc::new(VerifierExtractor(verifier)),
        )))
}

/// Parses a registry document string (schema + version gate).
///
/// # Errors
///
/// The schema error, stringly — the caller reports and exits.
pub fn load_schema(registry_json: &str) -> Result<RegistrySchema, String> {
    RegistrySchema::from_json(registry_json).map_err(|err| err.to_string())
}

// Re-exported for the binary's route parsing in overrides.
pub use cleverhans_webhook::client::Route;

/// Convenience: parses a `"METHOD /path"` string.
///
/// # Errors
///
/// The route parse error, stringly.
pub fn parse_route(value: &str) -> Result<Route, String> {
    Route::from_str(value).map_err(|err| err.to_string())
}
