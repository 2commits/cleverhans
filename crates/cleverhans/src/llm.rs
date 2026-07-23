//! Provider bootstrap — from the environment ([`from_env`]) or from a
//! declarative spec ([`LlmSpec`]/[`build_llm`], the config-document encoding
//! shared by the FFI bindings and the standalone service).

use std::sync::Arc;

use cleverhans_core::seams::LlmProvider;

/// No usable provider configuration in the environment.
#[cfg(any(feature = "anthropic", feature = "ollama"))]
#[derive(Debug, thiserror::Error)]
#[error("no LLM provider configured: set {expected}")]
pub struct FromEnvError {
    /// The env vars the enabled features would accept.
    expected: &'static str,
}

/// Declarative LLM provider selection, deserialized from a host config
/// object (`{"provider": "anthropic" | "ollama" | "scripted", ...}`). Each
/// variant exists only when its feature is enabled; `scripted` (feature
/// `test-util`) powers conformance and smoke tests without an API key.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum LlmSpec {
    /// Anthropic Messages API (feature `anthropic`).
    #[cfg(feature = "anthropic")]
    Anthropic {
        /// API key — the agent's only credential (spec §12.1).
        api_key: String,
        /// Model ID override.
        #[serde(default)]
        model: Option<String>,
    },
    /// Local Ollama daemon, zero egress (feature `ollama`).
    #[cfg(feature = "ollama")]
    Ollama {
        /// Model name known to the daemon.
        model: String,
        /// Daemon origin override.
        #[serde(default)]
        base_url: Option<String>,
    },
    /// Deterministic scripted output (feature `test-util`): one item list
    /// per LLM invocation, same encoding as the conformance vector format.
    #[cfg(feature = "test-util")]
    Scripted {
        /// The scripted turns.
        script: Vec<Vec<cleverhans_core::declarative::LlmItem>>,
    },
}

/// Builds a provider from a spec.
#[must_use]
pub fn build_llm(spec: LlmSpec) -> Arc<dyn LlmProvider> {
    match spec {
        #[cfg(feature = "anthropic")]
        LlmSpec::Anthropic { api_key, model } => {
            let mut config = cleverhans_llm_anthropic::AnthropicConfig::new(api_key);
            if let Some(model) = model {
                config.model = model;
            }
            Arc::new(cleverhans_llm_anthropic::AnthropicProvider::new(config))
        }
        #[cfg(feature = "ollama")]
        LlmSpec::Ollama { model, base_url } => {
            let mut config = cleverhans_llm_ollama::OllamaConfig::new(model);
            if let Some(base_url) = base_url {
                config.base_url = base_url;
            }
            Arc::new(cleverhans_llm_ollama::OllamaProvider::new(config))
        }
        #[cfg(feature = "test-util")]
        LlmSpec::Scripted { script } => {
            Arc::new(cleverhans_core::test_util::ScriptedLlm::new(
                script
                    .into_iter()
                    .map(|turn| turn.into_iter().map(Into::into).collect::<Vec<_>>()),
            ))
        }
    }
}

/// Picks an [`LlmProvider`] from the environment, first match wins:
///
/// 1. `OLLAMA_MODEL` (feature `ollama`) — local daemon, zero egress
/// 2. `ANTHROPIC_API_KEY` (feature `anthropic`), with `ANTHROPIC_MODEL`
///    optionally overriding the default model
///
/// # Errors
///
/// [`FromEnvError`] naming the env vars the enabled features accept, when
/// none of them is set.
#[cfg(any(feature = "anthropic", feature = "ollama"))]
pub fn from_env() -> Result<Arc<dyn LlmProvider>, FromEnvError> {
    #[cfg(feature = "ollama")]
    if let Ok(model) = std::env::var("OLLAMA_MODEL") {
        tracing::info!(provider = "ollama", model = model.as_str(), "llm from env");
        return Ok(Arc::new(cleverhans_llm_ollama::OllamaProvider::new(
            cleverhans_llm_ollama::OllamaConfig::new(model),
        )));
    }
    #[cfg(feature = "anthropic")]
    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
        let mut config = cleverhans_llm_anthropic::AnthropicConfig::new(api_key);
        if let Ok(model) = std::env::var("ANTHROPIC_MODEL") {
            config.model = model;
        }
        tracing::info!(
            provider = "anthropic",
            model = config.model.as_str(),
            "llm from env"
        );
        return Ok(Arc::new(cleverhans_llm_anthropic::AnthropicProvider::new(
            config,
        )));
    }
    Err(FromEnvError {
        expected: match (cfg!(feature = "ollama"), cfg!(feature = "anthropic")) {
            (true, true) => "OLLAMA_MODEL or ANTHROPIC_API_KEY",
            (true, false) => "OLLAMA_MODEL",
            (false, _) => "ANTHROPIC_API_KEY",
        },
    })
}
