//! Batteries-included facade for the CleverHans propose-only HITL agent
//! framework: one dependency line, one prelude.
//!
//! ```toml
//! [dependencies]
//! cleverhans = { version = "0.1", features = ["ws", "anthropic"] }
//! ```
//!
//! With features `ws` + a provider enabled:
//!
//! ```ignore
//! use std::sync::Arc;
//! use cleverhans::prelude::*;
//!
//! let agent = Arc::new(Agent::new(
//!     Arc::new(registry),
//!     cleverhans::llm::from_env()?,
//!     authz,
//!     Arc::new(resolver),
//! ));
//! let app = axum::Router::new().merge(agent_router_from_extension("/agent", agent));
//! ```
//!
//! Feature map (all off by default; the core protocol is always there):
//!
//! - `ws` — WebSocket + JSON binding for axum ([`ws`])
//! - `anthropic` / `ollama` — LLM providers ([`anthropic`], [`ollama`]) and
//!   the [`llm::from_env`] bootstrap
//! - `evals` — action-mapping eval harness ([`evals`])
//! - `test-util` — deterministic doubles ([`test_util`]) for offline tests

pub use cleverhans_core::{
    JsonMap, SPEC_VERSION, agent, async_trait, envelope, error, proposal, registry, schema, seams,
    slots, spec_version_compatible, validation,
};

#[cfg(feature = "test-util")]
pub use cleverhans_core::test_util;

/// The axum WebSocket binding (feature `ws`).
#[cfg(feature = "ws")]
pub use cleverhans_ws as ws;

/// The Anthropic Messages API provider (feature `anthropic`).
#[cfg(feature = "anthropic")]
pub use cleverhans_llm_anthropic as anthropic;

/// The local Ollama provider (feature `ollama`).
#[cfg(feature = "ollama")]
pub use cleverhans_llm_ollama as ollama;

/// The action-mapping eval harness (feature `evals`).
#[cfg(feature = "evals")]
pub use cleverhans_evals as evals;

#[cfg(any(feature = "anthropic", feature = "ollama"))]
pub mod llm;

/// Everything an integration typically names, one glob away.
pub mod prelude {
    pub use cleverhans_core::JsonMap;
    pub use cleverhans_core::agent::{Agent, AgentConfig, Session};
    pub use cleverhans_core::async_trait;
    pub use cleverhans_core::envelope::{
        ActionProposal, ClientEvent, Context, DryRunPreview, ServerEvent,
    };
    pub use cleverhans_core::error::{HandlerError, LlmError, RegistryError, ValidationFailure};
    pub use cleverhans_core::registry::{
        ActionBinding, ActionDef, BlockDef, ParamSource, ParamSpec, Registry, RegistryBuilder,
        SlotSpec, ValueType,
    };
    pub use cleverhans_core::schema::{MappedContextResolver, RegistrySchema};
    pub use cleverhans_core::seams::{
        ActionHandler, AllowAll, AuthzDecision, AuthzResolver, ContextParamResolver, DryRunHandler,
        LlmProvider, SlotBuilder, static_slots, typed_dry_run, typed_handler,
    };
    pub use cleverhans_core::slots;

    #[cfg(feature = "ws")]
    pub use cleverhans_ws::{PrincipalExtractor, agent_router, agent_router_from_extension};

    #[cfg(feature = "anthropic")]
    pub use cleverhans_llm_anthropic::{AnthropicConfig, AnthropicProvider};

    #[cfg(feature = "ollama")]
    pub use cleverhans_llm_ollama::{OllamaConfig, OllamaProvider};

    #[cfg(any(feature = "anthropic", feature = "ollama"))]
    pub use crate::llm::from_env;
}
