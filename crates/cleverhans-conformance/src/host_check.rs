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
    /// §14.2 HMAC signing key, for hosts that require payload signatures.
    pub signing_key: Option<String>,
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
            signing_key: None,
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
    /// Optional-endpoint vector (e.g. §14.9 build_slots): a `404` anywhere
    /// in it means SKIP, not FAIL — hosts without the endpoint stay
    /// conformant.
    #[serde(default)]
    pub optional: bool,
    /// Ordered request/response pairs.
    pub requests: Vec<HostRequest>,
}

/// How a vector concluded (when it didn't fail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCheckOutcome {
    /// Every request matched.
    Passed,
    /// An optional vector hit a `404` — the host doesn't implement the
    /// endpoint, which is conformant.
    Skipped(String),
}

/// Replays one vector against the target.
///
/// # Errors
///
/// A report naming the failing request index and mismatch.
pub async fn run_host_vector(
    target: &HostCheckTarget,
    vector: &HostVector,
) -> Result<HostCheckOutcome, String> {
    let client = reqwest::Client::new();
    let mut bindings = Bindings::default();
    let base = target.base_url.trim_end_matches('/');

    for (index, request) in vector.requests.iter().enumerate() {
        let url = format!("{base}{}", target.path(&request.endpoint));
        let body = substitute(&request.body, &bindings);
        // Serialize once; a §14.2 signature must cover the exact sent bytes.
        let bytes =
            serde_json::to_vec(&body).map_err(|err| format!("request {index}: encode: {err}"))?;
        let mut call = client
            .post(&url)
            .header("content-type", "application/json")
            .header(
                "x-cleverhans-webhook-version",
                request.webhook_version.unwrap_or(1).to_string(),
            )
            .header("x-cleverhans-delivery", uuid_like(index));
        if let Some(key) = &target.signing_key {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or_default();
            call = call.header(
                cleverhans_webhook::sign::SIGNATURE_HEADER,
                cleverhans_webhook::sign::signature_header(key.as_bytes(), now, &bytes),
            );
        }
        call = call.body(bytes);
        call = match request.auth {
            AuthMode::Valid => call.header("authorization", format!("Bearer {}", target.secret)),
            AuthMode::Invalid => call.header("authorization", "Bearer definitely-wrong"),
            AuthMode::None => call,
        };
        let response = call
            .send()
            .await
            .map_err(|err| format!("request {index} ({url}): {err}"))?;
        let raw_status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        if vector.optional && raw_status == 404 {
            return Ok(HostCheckOutcome::Skipped(format!(
                "endpoint `{}` not implemented (optional)",
                request.endpoint
            )));
        }
        let status = Value::from(raw_status);

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
    Ok(HostCheckOutcome::Passed)
}

/// Deterministic per-request delivery IDs (this replayer asserts host
/// behavior, not service randomness).
fn uuid_like(index: usize) -> String {
    format!("host-check-{index}")
}
