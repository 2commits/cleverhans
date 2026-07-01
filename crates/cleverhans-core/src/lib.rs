//! Reference backend for the CleverHans propose-only HITL agent protocol.
//!
//! The agent never acts on the host application. It proposes actions (and
//! dynamic UI) from a closed, app-owned registry; the application executes
//! through its own normal authorized path after explicit user confirmation.
//! See `spec/SPEC.md` for the normative protocol this crate implements.
//!
//! Crate layout follows the spec's framework boundary (§9):
//!
//! - [`registry`] — the closed action/block registry, the shared contract
//! - [`envelope`] — transport-agnostic client/server messages (§6)
//! - [`proposal`] — the proposal lifecycle state machine (§7)
//! - [`validation`] — propose-time and confirm-time validation pipeline
//! - [`seams`] — the traits an application implements to plug in
//! - [`agent`] — the agent loop tying the above together

pub mod agent;
pub mod envelope;
pub mod error;
pub mod proposal;
pub mod registry;
pub mod seams;
pub mod validation;

/// Spec version this crate implements; `Init.spec_version` is checked
/// against the same major.minor prefix (§13).
pub const SPEC_VERSION: &str = "0.1";

/// JSON object type used for `params`, `slots`, and extension maps.
pub type JsonMap = serde_json::Map<String, serde_json::Value>;
