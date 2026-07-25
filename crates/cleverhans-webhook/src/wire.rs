//! Serde projections of the spec §14 wire bodies. The JSON Schema files in
//! `spec/webhook/schemas/` describe the same shapes; a round-trip test keeps
//! the two in sync.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use cleverhans_core::JsonMap;
use cleverhans_core::envelope::DryRunPreview;

/// The webhook contract version this crate implements, carried in
/// `X-CleverHans-Webhook-Version` and every request body (spec §14.2).
pub const WEBHOOK_VERSION: u32 = 1;

/// `verify_session` request body (spec §14.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifySessionRequest {
    /// Always [`WEBHOOK_VERSION`].
    pub webhook_version: u32,
    /// Service-generated, opaque, unique per envelope stream.
    pub session_id: String,
    /// The client's stream-establishment headers, restricted to the
    /// configured forward-allowlist. Keys lowercased.
    pub headers: BTreeMap<String, String>,
}

/// `verify_session` 200 response body (spec §14.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifySessionResponse {
    /// Any JSON value; echoed byte-identical on every subsequent call
    /// (spec §12.13). The service never inspects it.
    pub principal: Value,
}

/// The `kind` discriminator on seam request bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeamKind {
    /// §14.4.
    Authorize,
    /// §14.5.
    DryRun,
    /// §14.6.
    Execute,
    /// §14.9.
    BuildSlots,
}

/// `authorize` / `dry_run` request body (spec §14.4–14.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeamRequest {
    /// Always [`WEBHOOK_VERSION`].
    pub webhook_version: u32,
    /// [`SeamKind::Authorize`] or [`SeamKind::DryRun`].
    pub kind: SeamKind,
    /// The session this call belongs to.
    pub session_id: String,
    /// A registered action ID.
    pub action_id: String,
    /// Fully validated, context-filled params.
    pub params: JsonMap,
    /// Verbatim echo of the `verify_session` principal.
    pub principal: Value,
}

/// `execute` request body (spec §14.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteRequest {
    /// Always [`WEBHOOK_VERSION`].
    pub webhook_version: u32,
    /// Always [`SeamKind::Execute`].
    pub kind: SeamKind,
    /// The session this call belongs to.
    pub session_id: String,
    /// A registered action ID.
    pub action_id: String,
    /// Fully validated, context-filled params.
    pub params: JsonMap,
    /// Verbatim echo of the `verify_session` principal.
    pub principal: Value,
    /// Minted once per confirmed execution, stable across retry attempts.
    /// Hosts MUST execute at most once per key (spec §12.14).
    pub idempotency_key: String,
    /// 1-based; increments per retry attempt.
    pub attempt: u32,
}

/// `build_slots` request body (spec §14.9).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildSlotsRequest {
    /// Always [`WEBHOOK_VERSION`].
    pub webhook_version: u32,
    /// Always [`SeamKind::BuildSlots`].
    pub kind: SeamKind,
    /// The session this call belongs to.
    pub session_id: String,
    /// A registered action ID.
    pub action_id: String,
    /// Fully validated, context-filled params.
    pub params: JsonMap,
    /// Verbatim echo of the `verify_session` principal.
    pub principal: Value,
    /// The §6.4 preview computed for this proposal; `null` for
    /// non-mutating actions (§9.7: the builder receives the preview).
    pub preview: Option<DryRunPreview>,
}

/// `build_slots` 200 response body (spec §14.9). No outcome envelope —
/// slot building is presentation, not a gate; failures fail closed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildSlotsResponse {
    /// Slot name → value for the action's block type; MUST pass the block
    /// slot schema (§7.1 step 4).
    pub slots: JsonMap,
}

/// `authorize` 200 response body (spec §14.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizeResponse {
    /// The decision.
    pub decision: Decision,
    /// Optional with deny; surfaced in validation failures.
    #[serde(default)]
    pub reason: Option<String>,
}

/// The `decision` field of an [`AuthorizeResponse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Permit.
    Allow,
    /// Refuse, with an optional reason beside it.
    Deny,
}

/// `dry_run` 200 response body (spec §14.5), tagged by `outcome`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DryRunResponse {
    /// A side-effect-free, permission-correct preview.
    Preview {
        /// The §6.4 preview; `{}` is a valid empty preview.
        preview: DryRunPreview,
    },
    /// The host declined to preview (business rule).
    Rejected {
        /// Optional host-authored reason.
        #[serde(default)]
        reason: Option<String>,
    },
}

/// `execute` 200 response body (spec §14.6), tagged by `outcome`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ExecuteResponse {
    /// The host executed the action.
    Executed {
        /// Any JSON value; carried in `ProposalStateChanged.result`.
        #[serde(default)]
        result: Value,
    },
    /// The host refused (business rule).
    Rejected {
        /// Optional host-authored reason.
        #[serde(default)]
        reason: Option<String>,
    },
}
