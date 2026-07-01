//! Reference gRPC transport binding for the CleverHans envelope (spec §11).
//!
//! The proto defines the envelope only; this crate converts between the
//! generated types and `cleverhans-core`'s transport-agnostic envelope, and
//! hosts the agent loop behind a tonic bidirectional stream. Other bindings
//! (WebSocket + JSON-RPC, tRPC, …) are explicitly welcome — this one is just
//! the reference.

pub mod convert;
pub mod service;

/// Generated protobuf/tonic types for `cleverhans.v1`.
#[allow(missing_docs, clippy::all)]
pub mod pb {
    tonic::include_proto!("cleverhans.v1");
}
