//! Provider bootstrap from the environment — the ten lines every
//! integration was going to copy from the demo.

use std::sync::Arc;

use cleverhans_core::seams::LlmProvider;

/// No usable provider configuration in the environment.
#[derive(Debug, thiserror::Error)]
#[error("no LLM provider configured: set {expected}")]
pub struct FromEnvError {
    /// The env vars the enabled features would accept.
    expected: &'static str,
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
