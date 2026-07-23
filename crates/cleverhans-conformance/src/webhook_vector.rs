//! Webhook-topology runners: rerun the agent-layer vectors through the §14
//! webhook seams (behavior preservation), and drive the
//! `spec/vectors/webhook/service/` vectors against a service assembled from
//! `cleverhans-webhook` + a scripted [`MockHost`].

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use cleverhans_core::agent::{Agent, Session};
use cleverhans_core::declarative::{DeclarativeSlots, LlmItem};
use cleverhans_core::envelope::ClientEvent;
use cleverhans_core::registry::RegistryBuilder;
use cleverhans_core::seams::{DryRunHandler, SlotBuilder};
use cleverhans_webhook::client::{HostClient, HostClientConfig, RetryPolicy, Route, Timeouts};
use cleverhans_webhook::seams::{WebhookAuthz, WebhookDryRun, WebhookHandler, WebhookVerifier};

use crate::fixture::{ExecutionExpectation, Fixture, ScriptedLlm, Step, Vector};
use crate::matcher::{Bindings, match_events, match_value, substitute};
use crate::mock_host::{
    AUTHORIZE_PATH, DRY_RUN_PATH, EXECUTE_PATH, HostScript, MockHost, VERIFY_SESSION_PATH,
    default_principal,
};

/// Service deployment knobs a vector may pin.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// The service-to-service bearer secret.
    pub secret: String,
    /// Header forward-allowlist; defaults to `authorization` + `cookie`.
    #[serde(default)]
    pub forward_headers: Option<Vec<String>>,
}

/// One `webhook/service/` step.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStep {
    /// Attempt stream establishment with these client headers.
    Connect {
        /// Client headers presented at establishment.
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    /// Assert the establishment outcome (101 = established).
    ExpectConnect {
        /// HTTP status.
        status: u16,
    },
    /// Send a client envelope event.
    Send(Value),
    /// Match the server events emitted since the last send.
    Expect(Vec<Value>),
}

/// One `webhook/service/` vector.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceVector {
    /// Vector name (file stem by convention).
    pub name: String,
    /// What the vector asserts.
    #[serde(default)]
    pub description: String,
    /// Spec section references.
    #[serde(default)]
    pub spec: Vec<String>,
    /// Always `"webhook-service"`.
    pub layer: String,
    /// Fixture name.
    pub fixture: String,
    /// Service deployment knobs.
    pub service_config: ServiceConfig,
    /// Scripted model output.
    #[serde(default)]
    pub llm: Vec<Vec<LlmItem>>,
    /// Per-endpoint host response overrides.
    #[serde(default)]
    pub host: HostScript,
    /// Steps.
    pub steps: Vec<ServiceStep>,
    /// The exact ordered webhook deliveries the host must have received.
    #[serde(default)]
    pub expect_deliveries: Vec<Value>,
}

fn short_timeouts() -> Timeouts {
    // Scripted `{"timeout": true}` behaviors hold the response open; test
    // clients must give up quickly.
    Timeouts {
        verify_session: Duration::from_millis(500),
        authorize: Duration::from_millis(500),
        dry_run: Duration::from_millis(500),
        execute: Duration::from_millis(500),
    }
}

/// Assembles a webhook-seamed agent over a [`MockHost`].
///
/// # Panics
///
/// On fixture-authoring errors — loud failure is the point.
#[must_use]
pub fn build_webhook_agent(
    fixture: &Fixture,
    llm: &[Vec<LlmItem>],
    host: &MockHost,
    secret: &str,
) -> (Arc<Agent<Value>>, Arc<HostClient>) {
    let client = Arc::new(
        HostClient::new(HostClientConfig {
            base_url: host.base_url(),
            secret: Some(secret.to_owned()),
            timeouts: short_timeouts(),
            retry: RetryPolicy {
                execute_attempts: 2,
                backoff: Duration::from_millis(20),
            },
            danger_allow_remote_http: false,
            danger_allow_missing_secret: false,
        })
        .expect("mock host client config"),
    );
    let mut builder = RegistryBuilder::from_schema(fixture.registry.clone());
    for def in &fixture.registry.actions {
        let script = fixture
            .scripts
            .get(&def.id)
            .unwrap_or_else(|| panic!("fixture `{}` has no script for `{}`", fixture.name, def.id));
        let dry_run = def.mutates.then(|| {
            Arc::new(WebhookDryRun::new(
                Arc::clone(&client),
                def.id.clone(),
                Route::from_str(&format!("POST {DRY_RUN_PATH}")).expect("route"),
            )) as Arc<dyn DryRunHandler<Value>>
        });
        builder = builder.attach(
            def.id.clone(),
            Arc::new(WebhookHandler::new(
                Arc::clone(&client),
                def.id.clone(),
                Route::from_str(&format!("POST {EXECUTE_PATH}")).expect("route"),
            )),
            dry_run,
            script
                .slots
                .clone()
                .map(|slots| Arc::new(DeclarativeSlots(slots)) as Arc<dyn SlotBuilder>),
        );
    }
    let registry = builder.build().expect("fixture registry is valid");
    let agent = Agent::new(
        Arc::new(registry),
        Arc::new(ScriptedLlm::new(llm)),
        Arc::new(WebhookAuthz::new(
            Arc::clone(&client),
            Route::from_str(&format!("POST {AUTHORIZE_PATH}")).expect("route"),
        )),
        Arc::new(
            fixture
                .registry
                .context_resolver()
                .expect("fixture registries map every context param"),
        ),
    );
    (Arc::new(agent), client)
}

/// Reruns an agent-layer vector with every seam replaced by its webhook
/// implementation against a [`MockHost`] serving the same scripts — the
/// behavior-preservation proof for the §14 binding.
///
/// # Errors
///
/// Any expectation mismatch, exactly as [`crate::run_vector`].
pub async fn run_agent_vector_via_webhooks(
    fixture: &Fixture,
    vector: &Vector,
) -> Result<(), String> {
    const SECRET: &str = "conformance-secret";
    let host = MockHost::spawn(
        fixture.clone(),
        vector.authz.clone(),
        HostScript::new(),
        SECRET,
    )
    .await;
    let (agent, _client) = build_webhook_agent(fixture, &vector.llm, &host, SECRET);
    let principal =
        cleverhans_webhook::seams::session_principal("s_conformance", default_principal());
    let mut session = Session::new(principal);
    let mut bindings = Bindings::default();
    let mut buffer: Vec<Value> = Vec::new();

    for (index, step) in vector.steps.iter().enumerate() {
        match step {
            Step::Send(payload) => {
                let payload = substitute(payload, &bindings);
                let event: ClientEvent = serde_json::from_value(payload)
                    .map_err(|err| format!("step {index}: send is not a ClientEvent: {err}"))?;
                for server_event in agent.handle(&mut session, event).await {
                    buffer.push(
                        serde_json::to_value(&server_event)
                            .map_err(|err| format!("step {index}: encode: {err}"))?,
                    );
                }
            }
            Step::Expect(expected) => {
                let actual = crate::runner::normalize(std::mem::take(&mut buffer), vector);
                match_events(expected, &actual, &mut bindings)
                    .map_err(|err| format!("step {index}: {err}"))?;
            }
        }
    }
    if let Some(expected) = &vector.executions {
        let actual: Vec<ExecutionExpectation> =
            host.executions.lock().expect("execution log").clone();
        if *expected != actual {
            return Err(format!(
                "executions diverge: expected {expected:?}, got {actual:?}"
            ));
        }
    }
    Ok(())
}

/// Runs one `webhook/service/` vector: assembles the webhook-seamed service
/// in-process, drives establishment + envelope steps, and matches the
/// host-observed delivery log.
///
/// # Errors
///
/// Any expectation mismatch naming the failing step or delivery.
pub async fn run_service_vector(fixture: &Fixture, vector: &ServiceVector) -> Result<(), String> {
    let host = MockHost::spawn(
        fixture.clone(),
        crate::fixture::AuthzScript::default(),
        vector.host.clone(),
        &vector.service_config.secret,
    )
    .await;
    let (agent, client) =
        build_webhook_agent(fixture, &vector.llm, &host, &vector.service_config.secret);
    let verifier = WebhookVerifier::new(
        client,
        Route::from_str(&format!("POST {VERIFY_SESSION_PATH}")).expect("route"),
        vector
            .service_config
            .forward_headers
            .clone()
            .unwrap_or_else(|| vec!["authorization".to_owned(), "cookie".to_owned()]),
    );

    let mut session: Option<Session<Value>> = None;
    let mut connect_status: Option<u16> = None;
    let mut bindings = Bindings::default();
    let mut buffer: Vec<Value> = Vec::new();

    for (index, step) in vector.steps.iter().enumerate() {
        match step {
            ServiceStep::Connect { headers } => {
                let mut header_map = http::HeaderMap::new();
                for (name, value) in headers {
                    header_map.insert(
                        http::header::HeaderName::from_str(name)
                            .map_err(|err| format!("step {index}: header name: {err}"))?,
                        value
                            .parse()
                            .map_err(|_| format!("step {index}: header value"))?,
                    );
                }
                match verifier.verify(&header_map).await {
                    Ok(principal) => {
                        connect_status = Some(101);
                        session = Some(Session::new(principal));
                    }
                    Err(status) => connect_status = Some(status.as_u16()),
                }
            }
            ServiceStep::ExpectConnect { status } => {
                let actual = connect_status
                    .ok_or_else(|| format!("step {index}: expect_connect before connect"))?;
                if actual != *status {
                    return Err(format!(
                        "step {index}: expected connect status {status}, got {actual}"
                    ));
                }
            }
            ServiceStep::Send(payload) => {
                let session = session
                    .as_mut()
                    .ok_or_else(|| format!("step {index}: send before an established connect"))?;
                let payload = substitute(payload, &bindings);
                let event: ClientEvent = serde_json::from_value(payload)
                    .map_err(|err| format!("step {index}: send is not a ClientEvent: {err}"))?;
                for server_event in agent.handle(session, event).await {
                    buffer.push(
                        serde_json::to_value(&server_event)
                            .map_err(|err| format!("step {index}: encode: {err}"))?,
                    );
                }
            }
            ServiceStep::Expect(expected) => {
                // Same normalization as the agent layer: drop chat deltas.
                let actual: Vec<Value> = std::mem::take(&mut buffer)
                    .into_iter()
                    .filter(|event| {
                        !(event.get("type").and_then(Value::as_str) == Some("chat_message")
                            && event.get("done") == Some(&Value::Bool(false)))
                    })
                    .collect();
                match_events(expected, &actual, &mut bindings)
                    .map_err(|err| format!("step {index}: {err}"))?;
            }
        }
    }

    let deliveries: Vec<Value> = host
        .deliveries
        .lock()
        .expect("delivery log")
        .iter()
        .map(crate::mock_host::Delivery::to_value)
        .collect();
    if vector.expect_deliveries.len() != deliveries.len() {
        return Err(format!(
            "expected {} deliveries, got {}: {}",
            vector.expect_deliveries.len(),
            deliveries.len(),
            serde_json::to_string(&deliveries).unwrap_or_default()
        ));
    }
    for (index, (want, got)) in vector.expect_deliveries.iter().zip(&deliveries).enumerate() {
        match_value(want, got, &mut bindings, &format!("delivery[{index}]"))
            .map_err(|err| format!("delivery {index}: {err}"))?;
    }
    Ok(())
}
