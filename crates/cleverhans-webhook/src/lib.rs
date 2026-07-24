//! The host webhook contract (spec §14) for the CleverHans standalone
//! service topology (§10.2): the agent runs as a separate process and
//! reaches the host app's seams — handlers, dry-run, authorization,
//! transport authentication — over four HTTP endpoints the host implements
//! in any language.
//!
//! - [`wire`] — serde projections of the §14 request/response bodies
//!   (machine-readable schemas: `spec/webhook/schemas/`)
//! - [`client`] — delivery: headers, per-endpoint timeouts, the execute
//!   retry loop (§14.6), startup transport-security refusals (§14.8)
//! - [`seams`] — drop-in `ActionHandler`/`DryRunHandler`/`AuthzResolver`
//!   implementations over `P = serde_json::Value`, plus the
//!   `verify_session` verifier
//!
//! The `cleverhans serve` binary is the reference deployment of this crate;
//! it is equally usable in-process by any Rust host that wants remote
//! execution without the standalone binary.

pub mod client;
pub mod seams;
pub mod sign;
pub mod wire;

pub use client::{
    ClientConfigError, DeliveryError, ExecuteDelivery, HostClient, HostClientConfig, RetryPolicy,
    Route, Timeouts,
};
pub use seams::{WebhookAuthz, WebhookDryRun, WebhookHandler, WebhookVerifier, session_principal};
pub use wire::WEBHOOK_VERSION;
