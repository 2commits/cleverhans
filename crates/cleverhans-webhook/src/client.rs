//! HTTP delivery to a host's §14 endpoints: headers, timeouts, the execute
//! retry loop, and the transport-security startup refusals (§14.8).

use std::str::FromStr;
use std::time::Duration;

use reqwest::StatusCode;
use serde::Serialize;

use crate::wire::{
    AuthorizeResponse, DryRunResponse, ExecuteRequest, ExecuteResponse, SeamRequest,
    VerifySessionRequest, VerifySessionResponse, WEBHOOK_VERSION,
};

/// One host endpoint as `"METHOD /path"` deployment configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// HTTP method (POST for every §14 endpoint; parsing stays general).
    pub method: reqwest::Method,
    /// Path joined onto the upstream base URL.
    pub path: String,
}

/// A route string that is not `"METHOD /path"`.
#[derive(Debug, thiserror::Error)]
#[error("invalid route `{0}` — expected `METHOD /path` (e.g. `POST /internal/cleverhans/execute`)")]
pub struct RouteParseError(String);

impl FromStr for Route {
    type Err = RouteParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split_whitespace();
        let (Some(method), Some(path), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(RouteParseError(value.to_owned()));
        };
        let method = reqwest::Method::from_str(&method.to_uppercase())
            .map_err(|_| RouteParseError(value.to_owned()))?;
        if !path.starts_with('/') {
            return Err(RouteParseError(value.to_owned()));
        }
        Ok(Self {
            method,
            path: path.to_owned(),
        })
    }
}

/// Per-endpoint delivery timeouts (spec §14.7 defaults).
#[derive(Debug, Clone)]
pub struct Timeouts {
    /// `verify_session` (default 5 s).
    pub verify_session: Duration,
    /// `authorize` (default 5 s).
    pub authorize: Duration,
    /// `dry_run` (default 10 s).
    pub dry_run: Duration,
    /// `execute` (default 30 s).
    pub execute: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            verify_session: Duration::from_secs(5),
            authorize: Duration::from_secs(5),
            dry_run: Duration::from_secs(10),
            execute: Duration::from_secs(30),
        }
    }
}

/// Retry policy — execute only (spec §14.7), safe under §12.14 idempotency.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total attempts including the first (default 3).
    pub execute_attempts: u32,
    /// Base backoff between attempts, doubled each retry (default 200 ms).
    pub backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            execute_attempts: 3,
            backoff: Duration::from_millis(200),
        }
    }
}

/// Deployment configuration for a [`HostClient`].
#[derive(Debug, Clone)]
pub struct HostClientConfig {
    /// Upstream origin, e.g. `http://127.0.0.1:3000`.
    pub base_url: String,
    /// Service-to-service bearer secret (spec §14.2).
    pub secret: Option<String>,
    /// HMAC signing key for `X-CleverHans-Signature` (spec §14.2,
    /// optional).
    pub signing_key: Option<String>,
    /// Per-endpoint timeouts.
    pub timeouts: Timeouts,
    /// Execute retry policy.
    pub retry: RetryPolicy,
    /// Permit a plaintext non-loopback upstream (spec §14.8 refuses it).
    pub danger_allow_remote_http: bool,
    /// Permit running without a service secret (spec §14.8 refuses it).
    pub danger_allow_missing_secret: bool,
}

/// Startup-time configuration refusals (spec §14.8).
#[derive(Debug, thiserror::Error)]
pub enum ClientConfigError {
    /// The base URL failed to parse.
    #[error("invalid upstream base_url `{0}`")]
    InvalidBaseUrl(String),
    /// Plaintext HTTP to a non-loopback host.
    #[error(
        "refusing plaintext http to non-loopback upstream `{0}` — use https, a loopback \
         address, or set danger_allow_remote_http"
    )]
    RemotePlaintext(String),
    /// No service secret configured.
    #[error(
        "refusing to run without a service secret — webhook endpoints are execution surface \
         (spec §12.11); configure one or set danger_allow_missing_secret"
    )]
    MissingSecret,
    /// The underlying HTTP client failed to build.
    #[error("http client: {0}")]
    Http(String),
}

/// Why a delivery produced no usable 200 body.
#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    /// The endpoint answered with a non-2xx status.
    #[error("host answered {status}")]
    Status {
        /// The HTTP status.
        status: StatusCode,
        /// The response body, for logs and host-authored `error` fields.
        body: String,
    },
    /// The delivery timed out or never connected — the outcome is unknown.
    #[error("delivery failed: {0}")]
    Unreachable(String),
    /// A 2xx answer whose body did not match the contract.
    #[error("malformed host response: {0}")]
    MalformedBody(String),
}

impl DeliveryError {
    /// Whether the host received-and-answered this delivery (§14.6: an
    /// answered call is never retried; only unknown outcomes are).
    #[must_use]
    pub fn is_answered(&self) -> bool {
        matches!(self, Self::Status { .. } | Self::MalformedBody(_))
    }
}

/// The result of the execute delivery loop (§14.6 table).
#[derive(Debug)]
pub enum ExecuteDelivery {
    /// A 200 with a contract-shaped body.
    Answered(ExecuteResponse),
    /// The host answered, but not with a usable 200 body.
    AnsweredError(DeliveryError),
    /// Every attempt timed out or failed to connect — outcome unknown.
    Unknown(String),
}

/// HTTP client for a host's §14 endpoints.
pub struct HostClient {
    http: reqwest::Client,
    base_url: String,
    secret: Option<String>,
    signing_key: Option<String>,
    timeouts: Timeouts,
    retry: RetryPolicy,
}

impl HostClient {
    /// Builds a client, applying the §14.8 startup refusals.
    ///
    /// # Errors
    ///
    /// [`ClientConfigError`] on an unparseable base URL, a plaintext
    /// non-loopback upstream, or a missing secret (absent the explicit
    /// `danger_` overrides).
    pub fn new(config: HostClientConfig) -> Result<Self, ClientConfigError> {
        let url = reqwest::Url::parse(&config.base_url)
            .map_err(|_| ClientConfigError::InvalidBaseUrl(config.base_url.clone()))?;
        let loopback = url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if url.scheme() == "http" && !loopback && !config.danger_allow_remote_http {
            return Err(ClientConfigError::RemotePlaintext(config.base_url));
        }
        if config.secret.is_none() && !config.danger_allow_missing_secret {
            return Err(ClientConfigError::MissingSecret);
        }
        let http = reqwest::Client::builder()
            .build()
            .map_err(|err| ClientConfigError::Http(err.to_string()))?;
        Ok(Self {
            http,
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            secret: config.secret,
            signing_key: config.signing_key,
            timeouts: config.timeouts,
            retry: config.retry,
        })
    }

    /// Per-endpoint timeouts, for callers composing their own flows.
    #[must_use]
    pub fn timeouts(&self) -> &Timeouts {
        &self.timeouts
    }

    async fn deliver(
        &self,
        route: &Route,
        timeout: Duration,
        body: &impl Serialize,
    ) -> Result<reqwest::Response, DeliveryError> {
        let url = format!("{}{}", self.base_url, route.path);
        // Serialize once and send those exact bytes: the §14.2 signature is
        // over the body as delivered, never a re-serialization.
        let bytes = serde_json::to_vec(body)
            .map_err(|err| DeliveryError::MalformedBody(err.to_string()))?;
        let mut request = self
            .http
            .request(route.method.clone(), &url)
            .timeout(timeout)
            .header("content-type", "application/json")
            .header("x-cleverhans-webhook-version", WEBHOOK_VERSION)
            .header("x-cleverhans-delivery", uuid::Uuid::new_v4().to_string());
        if let Some(secret) = &self.secret {
            request = request.header("authorization", format!("Bearer {secret}"));
        }
        if let Some(key) = &self.signing_key {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or_default();
            request = request.header(
                crate::sign::SIGNATURE_HEADER,
                crate::sign::signature_header(key.as_bytes(), timestamp, &bytes),
            );
        }
        let response = request.body(bytes).send().await.map_err(|err| {
            DeliveryError::Unreachable(if err.is_timeout() {
                format!("timeout after {timeout:?} delivering to {url}")
            } else {
                format!("{err} delivering to {url}")
            })
        })?;
        Ok(response)
    }

    async fn deliver_json<T: serde::de::DeserializeOwned>(
        &self,
        route: &Route,
        timeout: Duration,
        body: &impl Serialize,
    ) -> Result<T, DeliveryError> {
        let response = self.deliver(route, timeout, body).await?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|err| DeliveryError::Unreachable(err.to_string()))?;
        if !status.is_success() {
            return Err(DeliveryError::Status { status, body: text });
        }
        serde_json::from_str(&text).map_err(|err| DeliveryError::MalformedBody(err.to_string()))
    }

    /// Delivers a `verify_session` call (§14.3). Returns the raw status and
    /// body so the caller applies the normative refusal mapping.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::Unreachable`] when the delivery never completed.
    pub async fn verify_session(
        &self,
        route: &Route,
        request: &VerifySessionRequest,
    ) -> Result<(StatusCode, Option<VerifySessionResponse>), DeliveryError> {
        let response = self
            .deliver(route, self.timeouts.verify_session, request)
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let parsed = if status.is_success() {
            Some(
                serde_json::from_str::<VerifySessionResponse>(&body)
                    .map_err(|err| DeliveryError::MalformedBody(err.to_string()))?,
            )
        } else {
            None
        };
        Ok((status, parsed))
    }

    /// Delivers an `authorize` call (§14.4).
    ///
    /// # Errors
    ///
    /// Any [`DeliveryError`]; callers MUST map every error to deny (§14.4).
    pub async fn authorize(
        &self,
        route: &Route,
        request: &SeamRequest,
    ) -> Result<AuthorizeResponse, DeliveryError> {
        self.deliver_json(route, self.timeouts.authorize, request)
            .await
    }

    /// Delivers a `dry_run` call (§14.5).
    ///
    /// # Errors
    ///
    /// Any [`DeliveryError`]; callers map it per the §14.5 table.
    pub async fn dry_run(
        &self,
        route: &Route,
        request: &SeamRequest,
    ) -> Result<DryRunResponse, DeliveryError> {
        self.deliver_json(route, self.timeouts.dry_run, request)
            .await
    }

    /// Runs the execute delivery loop (§14.6): bounded retry with doubling
    /// backoff on unknown outcomes only, same `idempotency_key`, fresh
    /// delivery ID, incremented `attempt`.
    pub async fn execute(&self, route: &Route, request: &ExecuteRequest) -> ExecuteDelivery {
        let mut request = request.clone();
        let mut backoff = self.retry.backoff;
        let attempts = self.retry.execute_attempts.max(1);
        let mut last_unreachable = String::new();
        for attempt in 1..=attempts {
            request.attempt = attempt;
            match self
                .deliver_json::<ExecuteResponse>(route, self.timeouts.execute, &request)
                .await
            {
                Ok(response) => return ExecuteDelivery::Answered(response),
                Err(err) if err.is_answered() => return ExecuteDelivery::AnsweredError(err),
                Err(DeliveryError::Unreachable(message)) => {
                    tracing::warn!(
                        action_id = request.action_id.as_str(),
                        attempt,
                        "execute delivery unreachable: {message}"
                    );
                    last_unreachable = message;
                    if attempt < attempts {
                        tokio::time::sleep(backoff).await;
                        backoff *= 2;
                    }
                }
                Err(err) => return ExecuteDelivery::AnsweredError(err),
            }
        }
        ExecuteDelivery::Unknown(last_unreachable)
    }
}
