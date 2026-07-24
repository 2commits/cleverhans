//! Replays `spec/vectors/webhook/host/` vectors against a candidate host —
//! the third-party conformance story: implement the four §14 endpoints,
//! run these until green. Wrapped by the `cleverhans host-check` CLI.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::matcher::{Bindings, match_value, substitute};

/// The candidate host under test.
#[derive(Debug, Clone)]
pub struct HostCheckTarget {
    /// Host origin, e.g. `https://your-app/`.
    pub base_url: String,
    /// The service secret the host expects.
    pub secret: String,
    /// Endpoint name → path overrides; defaults are the §14.1 paths.
    pub paths: BTreeMap<String, String>,
}

impl HostCheckTarget {
    /// A target with the default §14.1 endpoint paths.
    #[must_use]
    pub fn new(base_url: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            secret: secret.into(),
            paths: BTreeMap::new(),
        }
    }

    fn path(&self, endpoint: &str) -> String {
        self.paths
            .get(endpoint)
            .cloned()
            .unwrap_or_else(|| format!("/cleverhans/{endpoint}"))
    }
}

/// The bearer credential a request presents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// The configured secret.
    Valid,
    /// A wrong secret.
    Invalid,
    /// No `Authorization` header at all.
    None,
}

/// One request/response pair.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRequest {
    /// §14 endpoint name.
    pub endpoint: String,
    /// Bearer credential mode.
    pub auth: AuthMode,
    /// `X-CleverHans-Webhook-Version` override (default 1).
    #[serde(default)]
    pub webhook_version: Option<u64>,
    /// Request body; `$ref` directives are substituted from earlier binds.
    pub body: Value,
    /// Expected response.
    pub expect: HostExpect,
}

/// Expected response: status (int or `{"$in": [..]}`) and optional body
/// matchers.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostExpect {
    /// Status matcher.
    pub status: Value,
    /// Body matchers (subset semantics).
    #[serde(default)]
    pub body: Option<Value>,
}

/// One `webhook/host/` vector.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostVector {
    /// Vector name (file stem by convention).
    pub name: String,
    /// What the vector asserts.
    #[serde(default)]
    pub description: String,
    /// Spec section references.
    #[serde(default)]
    pub spec: Vec<String>,
    /// Always `"webhook-host"`.
    pub layer: String,
    /// The fixture whose semantics the host is seeded with.
    pub fixture: String,
    /// Ordered request/response pairs.
    pub requests: Vec<HostRequest>,
}

/// Replays one vector against the target.
///
/// # Errors
///
/// A report naming the failing request index and mismatch.
pub async fn run_host_vector(target: &HostCheckTarget, vector: &HostVector) -> Result<(), String> {
    let client = reqwest::Client::new();
    let mut bindings = Bindings::default();
    let base = target.base_url.trim_end_matches('/');

    for (index, request) in vector.requests.iter().enumerate() {
        let url = format!("{base}{}", target.path(&request.endpoint));
        let body = substitute(&request.body, &bindings);
        let mut call = client
            .post(&url)
            .header(
                "x-cleverhans-webhook-version",
                request.webhook_version.unwrap_or(1).to_string(),
            )
            .header("x-cleverhans-delivery", uuid_like(index))
            .json(&body);
        call = match request.auth {
            AuthMode::Valid => call.header("authorization", format!("Bearer {}", target.secret)),
            AuthMode::Invalid => call.header("authorization", "Bearer definitely-wrong"),
            AuthMode::None => call,
        };
        let response = call
            .send()
            .await
            .map_err(|err| format!("request {index} ({url}): {err}"))?;
        let status = Value::from(response.status().as_u16());
        let text = response.text().await.unwrap_or_default();

        match_value(
            &request.expect.status,
            &status,
            &mut bindings,
            &format!("request[{index}].status"),
        )
        .map_err(|err| format!("request {index}: {err} (body: {text})"))?;

        if let Some(expected_body) = &request.expect.body {
            let actual: Value = serde_json::from_str(&text)
                .map_err(|err| format!("request {index}: response body not JSON: {err}"))?;
            match_value(
                expected_body,
                &actual,
                &mut bindings,
                &format!("request[{index}].body"),
            )
            .map_err(|err| format!("request {index}: {err}"))?;
        }
    }
    Ok(())
}

/// Deterministic per-request delivery IDs (this replayer asserts host
/// behavior, not service randomness).
fn uuid_like(index: usize) -> String {
    format!("host-check-{index}")
}
