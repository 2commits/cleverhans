//! Conformance vector runner for the CleverHans protocol.
//!
//! Vectors (`spec/vectors/`) are JSON documents that script every
//! nondeterministic seam — LLM, authz, handlers, dry-run, slots — as pure
//! data, feed client envelope events in, and assert the outbound server
//! events under the matching rules documented in `spec/vectors/README.md`.
//!
//! This crate is a library on purpose: the fixture→seam builders and the
//! matcher are reused by binding test suites (Python, Node) so every
//! implementation runs the same vectors.

pub mod fixture;
pub mod host_check;
pub mod matcher;
pub mod mock_host;
pub mod runner;
pub mod webhook_vector;

pub use fixture::{ExecutionLog, Fixture, Vector, build_agent};
pub use host_check::{HostCheckOutcome, HostCheckTarget, HostVector, run_host_vector};
pub use mock_host::MockHost;
pub use runner::run_vector;
pub use webhook_vector::{ServiceVector, run_agent_vector_via_webhooks, run_service_vector};
