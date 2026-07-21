# cleverhans-llm-anthropic

`LlmProvider` implementation over the Anthropic Messages API (streaming) for
[CleverHans](https://github.com/nordalf/cleverhans). Usually consumed through
the [`cleverhans`](https://crates.io/crates/cleverhans) facade's `anthropic`
feature, whose `llm::from_env()` bootstraps from `ANTHROPIC_API_KEY` (+
optional `ANTHROPIC_MODEL`).

`AnthropicConfig::new(api_key)` with public `model` / `base_url` /
`max_tokens` fields for direct construction (gateways, custom endpoints).
