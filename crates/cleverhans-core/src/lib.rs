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
//! - [`schema`] — the declarative registry document (load, attach handlers)
//! - [`envelope`] — transport-agnostic client/server messages (§6)
//! - [`proposal`] — the proposal lifecycle state machine (§7)
//! - [`validation`] — propose-time and confirm-time validation pipeline
//! - [`seams`] — the traits an application implements to plug in
//! - [`agent`] — the agent loop tying the above together
//! - [`test_util`] (feature `test-util`) — deterministic doubles for
//!   integration tests without a live model

pub mod agent;
pub mod envelope;
pub mod error;
pub mod proposal;
pub mod registry;
pub mod schema;
pub mod seams;
#[cfg(feature = "test-util")]
pub mod test_util;
pub mod validation;

/// Spec version this crate implements; `Init.spec_version` is checked
/// against the same major.minor prefix (§13).
pub const SPEC_VERSION: &str = "0.1";

/// Whether a spec version string is compatible with [`SPEC_VERSION`]:
/// same major.minor on a segment boundary, so `"0.1"`, `"0.1.0"`, and
/// `"0.1.0-draft"` match while `"0.10"` and `"0.1x"` do not.
#[must_use]
pub fn spec_version_compatible(version: &str) -> bool {
    version
        .strip_prefix(SPEC_VERSION)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.') || rest.starts_with('-'))
}

/// JSON object type used for `params`, `slots`, and extension maps.
pub type JsonMap = serde_json::Map<String, serde_json::Value>;

/// Re-exported so trait-impl seam registrations (`#[async_trait]` on
/// [`seams::ActionHandler`] etc.) never require integrators to add and
/// version-match the `async-trait` crate themselves.
pub use async_trait::async_trait;

#[doc(hidden)]
pub mod __private {
    pub use serde_json;
}

/// Builds a [`JsonMap`] with [`serde_json::json!`] object syntax — the slot
/// counterpart to `json!`, for use in [`seams::SlotBuilder`] closures and
/// [`seams::static_slots`]:
///
/// ```
/// use cleverhans_core::slots;
///
/// let new_title = "Launch Plan";
/// let map = slots! {
///     "title": "Rename document",
///     "detail": format!("New title: {new_title}"),
/// };
/// assert_eq!(map["title"], "Rename document");
/// ```
#[macro_export]
macro_rules! slots {
    ( $($tt:tt)* ) => {
        match $crate::__private::serde_json::json!({ $($tt)* }) {
            $crate::__private::serde_json::Value::Object(map) => map,
            _ => unreachable!("json! with braces always yields an object"),
        }
    };
}
