//! The §9 seams as webhook forwarders: drop-in `ActionHandler` /
//! `DryRunHandler` / `AuthzResolver` implementations over
//! `P = serde_json::Value` that deliver to a host's §14 endpoints and map
//! every outcome per the normative tables.
//!
//! # The session principal envelope
//!
//! The seam traits carry no session identity, so the service-side principal
//! is an *internal* envelope `{"session_id": ..., "principal": <host's>}`
//! produced by [`WebhookVerifier`]. Seams unwrap it before delivery: the
//! wire carries the host's principal byte-identical (spec §12.13); the
//! envelope never leaves the process.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use cleverhans_core::JsonMap;
use cleverhans_core::async_trait;
use cleverhans_core::envelope::DryRunPreview;
use cleverhans_core::error::HandlerError;
use cleverhans_core::seams::{ActionHandler, AuthzDecision, AuthzResolver, DryRunHandler};

use crate::client::{ExecuteDelivery, HostClient, Route};
use crate::wire::{
    AuthorizeResponse, Decision, DryRunResponse, ExecuteRequest, ExecuteResponse, SeamKind,
    SeamRequest, VerifySessionRequest, WEBHOOK_VERSION,
};

use std::sync::Arc;

/// Wraps a host principal into the internal session envelope.
#[must_use]
pub fn session_principal(session_id: &str, principal: Value) -> Value {
    json!({ "session_id": session_id, "principal": principal })
}

/// Unwraps the internal session envelope.
///
/// # Errors
///
/// [`HandlerError::Internal`] when the principal was not produced by
/// [`WebhookVerifier`] — a service wiring error, never a host fault.
pub fn split_principal(wrapped: &Value) -> Result<(&str, &Value), HandlerError> {
    let (Some(session_id), Some(principal)) = (
        wrapped.get("session_id").and_then(Value::as_str),
        wrapped.get("principal"),
    ) else {
        return Err(HandlerError::Internal(
            "principal missing the service session envelope (wire the agent with \
             WebhookVerifier)"
                .to_owned(),
        ));
    };
    Ok((session_id, principal))
}

fn seam_request(
    kind: SeamKind,
    action_id: &str,
    params: &JsonMap,
    wrapped_principal: &Value,
) -> Result<SeamRequest, HandlerError> {
    let (session_id, principal) = split_principal(wrapped_principal)?;
    Ok(SeamRequest {
        webhook_version: WEBHOOK_VERSION,
        kind,
        session_id: session_id.to_owned(),
        action_id: action_id.to_owned(),
        params: params.clone(),
        principal: principal.clone(),
    })
}

/// [`ActionHandler`] delivering to a host's execute endpoint (§14.6).
pub struct WebhookHandler {
    client: Arc<HostClient>,
    action_id: String,
    route: Route,
}

impl WebhookHandler {
    /// Binds one action's execute deliveries to a route.
    #[must_use]
    pub fn new(client: Arc<HostClient>, action_id: impl Into<String>, route: Route) -> Self {
        Self {
            client,
            action_id: action_id.into(),
            route,
        }
    }
}

#[async_trait]
impl ActionHandler<Value> for WebhookHandler {
    async fn execute(&self, params: &JsonMap, principal: &Value) -> Result<Value, HandlerError> {
        let (session_id, principal) = split_principal(principal)?;
        let request = ExecuteRequest {
            webhook_version: WEBHOOK_VERSION,
            kind: SeamKind::Execute,
            session_id: session_id.to_owned(),
            action_id: self.action_id.clone(),
            params: params.clone(),
            principal: principal.clone(),
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            attempt: 1,
        };
        match self.client.execute(&self.route, &request).await {
            ExecuteDelivery::Answered(ExecuteResponse::Executed { result }) => Ok(result),
            ExecuteDelivery::Answered(ExecuteResponse::Rejected { reason }) => Err(
                HandlerError::Rejected(reason.unwrap_or_else(|| "rejected by host".to_owned())),
            ),
            ExecuteDelivery::AnsweredError(err) => Err(HandlerError::Internal(err.to_string())),
            // §14.6: a failed delivery asserts the outcome is unknown, not
            // that nothing executed — the host owns the source of truth.
            ExecuteDelivery::Unknown(_) => Err(HandlerError::Internal(
                "execution outcome unknown".to_owned(),
            )),
        }
    }
}

/// [`DryRunHandler`] delivering to a host's dry_run endpoint (§14.5).
pub struct WebhookDryRun {
    client: Arc<HostClient>,
    action_id: String,
    route: Route,
}

impl WebhookDryRun {
    /// Binds one action's dry-run deliveries to a route.
    #[must_use]
    pub fn new(client: Arc<HostClient>, action_id: impl Into<String>, route: Route) -> Self {
        Self {
            client,
            action_id: action_id.into(),
            route,
        }
    }
}

#[async_trait]
impl DryRunHandler<Value> for WebhookDryRun {
    async fn dry_run(
        &self,
        params: &JsonMap,
        principal: &Value,
    ) -> Result<DryRunPreview, HandlerError> {
        let request = seam_request(SeamKind::DryRun, &self.action_id, params, principal)?;
        match self.client.dry_run(&self.route, &request).await {
            Ok(DryRunResponse::Preview { preview }) => Ok(preview),
            Ok(DryRunResponse::Rejected { reason }) => Err(HandlerError::Rejected(
                reason.unwrap_or_else(|| "rejected by host".to_owned()),
            )),
            Err(err) => Err(HandlerError::Internal(err.to_string())),
        }
    }
}

/// [`AuthzResolver`] delivering to a host's authorize endpoint (§14.4).
/// Every delivery failure maps to deny — fail closed, always.
pub struct WebhookAuthz {
    client: Arc<HostClient>,
    route: Route,
}

impl WebhookAuthz {
    /// Binds authorization to the host's single authorize endpoint.
    #[must_use]
    pub fn new(client: Arc<HostClient>, route: Route) -> Self {
        Self { client, route }
    }
}

#[async_trait]
impl AuthzResolver<Value> for WebhookAuthz {
    async fn authorize(
        &self,
        principal: &Value,
        action_id: &str,
        params: &JsonMap,
    ) -> AuthzDecision {
        let request = match seam_request(SeamKind::Authorize, action_id, params, principal) {
            Ok(request) => request,
            Err(err) => return AuthzDecision::Deny(err.to_string()),
        };
        match self.client.authorize(&self.route, &request).await {
            Ok(AuthorizeResponse {
                decision: Decision::Allow,
                ..
            }) => AuthzDecision::Allow,
            Ok(AuthorizeResponse {
                decision: Decision::Deny,
                reason,
            }) => AuthzDecision::Deny(reason.unwrap_or_else(|| "denied by host".to_owned())),
            Err(err) => {
                tracing::error!(action_id, "authorize delivery failed, denying: {err}");
                AuthzDecision::Deny("authorization unavailable".to_owned())
            }
        }
    }
}

/// Verifies stream establishment against a host's verify_session endpoint
/// (§14.3) and produces the internal session-envelope principal.
pub struct WebhookVerifier {
    client: Arc<HostClient>,
    route: Route,
    forward_headers: Vec<String>,
}

impl WebhookVerifier {
    /// Binds verification to a route with a header forward-allowlist
    /// (lowercased; defaults belong to the caller's config layer).
    #[must_use]
    pub fn new(client: Arc<HostClient>, route: Route, forward_headers: Vec<String>) -> Self {
        Self {
            client,
            route,
            forward_headers: forward_headers
                .into_iter()
                .map(|name| name.to_ascii_lowercase())
                .collect(),
        }
    }

    /// Runs the §14.3 verification: forwards allowlisted headers once,
    /// returns the wrapped session principal or the refusal status.
    ///
    /// # Errors
    ///
    /// `401`/`403` verbatim from the host; `503` for everything else
    /// (fail closed).
    pub async fn verify(&self, headers: &http::HeaderMap) -> Result<Value, http::StatusCode> {
        let forwarded: BTreeMap<String, String> = self
            .forward_headers
            .iter()
            .filter_map(|name| {
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(|value| (name.clone(), value.to_owned()))
            })
            .collect();
        let session_id = format!("s_{}", uuid::Uuid::new_v4().simple());
        let request = VerifySessionRequest {
            webhook_version: WEBHOOK_VERSION,
            session_id: session_id.clone(),
            headers: forwarded,
        };
        match self.client.verify_session(&self.route, &request).await {
            Ok((status, Some(response))) if status.is_success() => {
                Ok(session_principal(&session_id, response.principal))
            }
            Ok((status, _))
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN =>
            {
                Err(http::StatusCode::from_u16(status.as_u16())
                    .unwrap_or(http::StatusCode::UNAUTHORIZED))
            }
            Ok((status, _)) => {
                tracing::error!("verify_session answered {status}, refusing stream");
                Err(http::StatusCode::SERVICE_UNAVAILABLE)
            }
            Err(err) => {
                tracing::error!("verify_session delivery failed, refusing stream: {err}");
                Err(http::StatusCode::SERVICE_UNAVAILABLE)
            }
        }
    }
}
