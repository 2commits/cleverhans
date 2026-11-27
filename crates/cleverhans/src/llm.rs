//! Provider bootstrap — from the environment ([`from_env`]) or from a
//! declarative spec ([`LlmSpec`]/[`build_llm`], the config-document encoding
//! shared by the FFI bindings and the standalone service).

use std::sync::Arc;

use cleverhans_core::seams::LlmProvider;

/// No usable provider configuration in the environment.
#[cfg(any(feature = "anthropic", feature = "ollama", feature = "openai"))]
#[derive(Debug, thiserror::Error)]
#[error("no LLM provider configured: set {expected}")]
pub struct FromEnvError {
    /// The env vars the enabled features would accept.
    expected: &'static str,
}

/// Declarative LLM provider selection, deserialized from a host config
/// object (`{"provider": "anthropic" | "ollama" | "openai" | "scripted",
/// ...}`). Each variant exists only when its feature is enabled; `scripted`
/// (feature `test-util`) powers conformance and smoke tests without an API
/// key.
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
    /// OpenAI-compatible Chat Completions server — vLLM, SGLang, NIM,
    /// Ollama's `/v1`, hosted compatible APIs (feature `openai`). Tagged
    /// `"openai"`, not `"open_ai"`.
    #[cfg(feature = "openai")]
    #[serde(rename = "openai")]
    OpenAi {
        /// Model name as the server knows it.
        model: String,
        /// API base including the version path, e.g.
        /// `http://localhost:8000/v1`. Required: no default server.
        base_url: String,
        /// Bearer key; local servers often need none.
        #[serde(default)]
        api_key: Option<String>,
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
        #[cfg(feature = "openai")]
        LlmSpec::OpenAi {
            model,
            base_url,
            api_key,
        } => {
            let mut config = cleverhans_llm_openai::OpenAiConfig::new(base_url, model);
            config.api_key = api_key;
            Arc::new(cleverhans_llm_openai::OpenAiProvider::new(config))
        }
        #[cfg(feature = "test-util")]
        LlmSpec::Scripted { script } => Arc::new(cleverhans_core::test_util::ScriptedLlm::new(
            script
                .into_iter()
                .map(|turn| turn.into_iter().map(Into::into).collect::<Vec<_>>()),
        )),
    }
}

/// Picks an [`LlmProvider`] from the environment, first match wins:
///
/// 1. `OLLAMA_MODEL` (feature `ollama`) — local daemon, zero egress
/// 2. `OPENAI_BASE_URL` and `OPENAI_MODEL`, both set (feature `openai`) —
///    an OpenAI-compatible server such as vLLM, with `OPENAI_API_KEY`
///    optionally sent as a bearer key. `OPENAI_API_KEY` alone never selects
///    this provider: there is no default server.
/// 3. `ANTHROPIC_API_KEY` (feature `anthropic`), with `ANTHROPIC_MODEL`
///    optionally overriding the default model
///
/// # Errors
///
/// [`FromEnvError`] naming the env vars the enabled features accept, when
/// none of them is set.
#[cfg(any(feature = "anthropic", feature = "ollama", feature = "openai"))]
pub fn from_env() -> Result<Arc<dyn LlmProvider>, FromEnvError> {
    #[cfg(feature = "ollama")]
    if let Ok(model) = std::env::var("OLLAMA_MODEL") {
        tracing::info!(provider = "ollama", model = model.as_str(), "llm from env");
        return Ok(Arc::new(cleverhans_llm_ollama::OllamaProvider::new(
            cleverhans_llm_ollama::OllamaConfig::new(model),
        )));
    }
    #[cfg(feature = "openai")]
    if let (Ok(base_url), Ok(model)) = (
        std::env::var("OPENAI_BASE_URL"),
        std::env::var("OPENAI_MODEL"),
    ) {
        tracing::info!(provider = "openai", model = model.as_str(), "llm from env");
        let mut config = cleverhans_llm_openai::OpenAiConfig::new(base_url, model);
        config.api_key = std::env::var("OPENAI_API_KEY").ok();
        return Ok(Arc::new(cleverhans_llm_openai::OpenAiProvider::new(config)));
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
        expected: match (
            cfg!(feature = "ollama"),
            cfg!(feature = "openai"),
            cfg!(feature = "anthropic"),
        ) {
            (true, true, true) => {
                "OLLAMA_MODEL, OPENAI_BASE_URL + OPENAI_MODEL, or ANTHROPIC_API_KEY"
            }
            (true, true, false) => "OLLAMA_MODEL or OPENAI_BASE_URL + OPENAI_MODEL",
            (true, false, true) => "OLLAMA_MODEL or ANTHROPIC_API_KEY",
            (true, false, false) => "OLLAMA_MODEL",
            (false, true, true) => "OPENAI_BASE_URL + OPENAI_MODEL or ANTHROPIC_API_KEY",
            (false, true, false) => "OPENAI_BASE_URL + OPENAI_MODEL",
            (false, false, _) => "ANTHROPIC_API_KEY",
        },
    })
}

#[cfg(all(test, feature = "openai"))]
mod tests {
    use serde_json::json;

    use super::*;

    mod openai_spec {
        use super::*;

        #[test]
        fn deserializes_from_the_openai_tag() {
            let spec: LlmSpec = serde_json::from_value(json!({
                "provider": "openai",
                "model": "Qwen/Qwen3-32B",
                "base_url": "http://localhost:8000/v1",
            }))
            .expect("valid spec");

            assert!(matches!(
                spec,
                LlmSpec::OpenAi { ref model, ref base_url, api_key: None }
                    if model == "Qwen/Qwen3-32B" && base_url == "http://localhost:8000/v1"
            ));
        }

        #[test]
        fn carries_an_optional_api_key() {
            let spec: LlmSpec = serde_json::from_value(json!({
                "provider": "openai",
                "model": "m",
                "base_url": "https://llm.internal/v1",
                "api_key": "sk-local",
            }))
            .expect("valid spec");

            assert!(
                matches!(spec, LlmSpec::OpenAi { api_key: Some(ref key), .. } if key == "sk-local")
            );
        }

        #[test]
        fn requires_a_base_url_and_rejects_the_snake_case_tag() {
            let no_base_url =
                serde_json::from_value::<LlmSpec>(json!({"provider": "openai", "model": "m"}));
            let snake_case = serde_json::from_value::<LlmSpec>(json!({
                "provider": "open_ai", "model": "m", "base_url": "http://localhost:8000/v1",
            }));

            assert!(no_base_url.is_err());
            assert!(snake_case.is_err());
        }

        #[test]
        fn builds_a_provider_without_network() {
            let _provider = build_llm(LlmSpec::OpenAi {
                model: "m".to_owned(),
                base_url: "http://localhost:8000/v1".to_owned(),
                api_key: None,
            });
        }
    }
}
