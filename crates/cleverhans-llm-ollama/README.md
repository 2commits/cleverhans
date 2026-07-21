# cleverhans-llm-ollama

`LlmProvider` implementation over a local Ollama daemon for
[CleverHans](https://github.com/2commits/cleverhans) — zero egress, no API
key. Usually consumed through the
[`cleverhans`](https://crates.io/crates/cleverhans) facade's `ollama`
feature, whose `llm::from_env()` bootstraps from `OLLAMA_MODEL` (+ optional
`OLLAMA_BASE_URL`).
