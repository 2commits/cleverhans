//! The stable telemetry event contract (`cleverhans::telemetry::*`).
//!
//! The framework crates carry no metrics dependency: they emit structured
//! `tracing` events under these targets, and any subscriber converts them —
//! the service's OTEL layer, or an in-process host's own. Target names and
//! field names are part of the contract; adding a field is
//! backwards-compatible, renaming one is not.
//!
//! | target | fields |
//! |---|---|
//! | `cleverhans::telemetry::proposal` | `action_id`, `state`, `reason` |
//! | `cleverhans::telemetry::session` | `phase` (`opened`/`closed`), `reason`, `duration_ms` |
//! | `cleverhans::telemetry::delivery` | `endpoint`, `outcome`, `status`, `duration_ms` |
//! | `cleverhans::telemetry::delivery_retry` | `action_id`, `attempt` |
//! | `cleverhans::telemetry::llm` | `outcome`, `duration_ms` |
//!
//! Events are emitted at `INFO`. A subscriber that turns them into metrics
//! must not sit behind a log-level filter, or the counters go silently
//! empty when an operator turns logging down.

use std::fmt::Display;
use std::time::Instant;

/// Records one proposal lifecycle transition (spec §7).
///
/// Call this *after* the store transition succeeded: the signal mirrors the
/// state machine, not attempted edges, so a rejected transition never shows
/// up as a state change that did not happen.
pub fn proposal_state(action_id: &str, state: impl Display, reason: Option<&str>) {
    tracing::info!(
        target: "cleverhans::telemetry::proposal",
        action_id,
        state = %state,
        reason = reason.unwrap_or(""),
        "proposal state"
    );
}

/// The open/closed pair for one envelope session, as an RAII guard.
///
/// `closed` is emitted on drop rather than at each exit, so a task abort,
/// a panic, or a `return` added later cannot leak the paired
/// `cleverhans.sessions.active` increment — an up-down counter that only
/// ever drifts upward never recovers without a restart.
pub struct SessionSpan {
    started: Instant,
    reason: &'static str,
}

impl SessionSpan {
    /// Emits `phase = "opened"` and starts the duration clock.
    #[must_use]
    pub fn open() -> Self {
        tracing::info!(
            target: "cleverhans::telemetry::session",
            phase = "opened",
            "envelope session opened"
        );
        Self {
            started: Instant::now(),
            reason: "closed",
        }
    }

    /// Overrides the `reason` reported when the session closes.
    pub fn set_reason(&mut self, reason: &'static str) {
        self.reason = reason;
    }
}

impl Drop for SessionSpan {
    fn drop(&mut self) {
        tracing::info!(
            target: "cleverhans::telemetry::session",
            phase = "closed",
            reason = self.reason,
            duration_ms = self.started.elapsed().as_millis() as u64,
            "envelope session closed"
        );
    }
}
