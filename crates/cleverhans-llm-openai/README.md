# cleverhans-llm-openai

`LlmProvider` implementation over the OpenAI-compatible Chat Completions API
(`POST {base_url}/chat/completions`) for
[CleverHans](https://github.com/2commits/cleverhans) — a self-hosted vLLM,
SGLang or NIM server, Ollama's `/v1`, or a hosted compatible API. The base URL
is required and includes the version path (`http://localhost:8000/v1`); the
API key is optional. Usually consumed through the
[`cleverhans`](https://crates.io/crates/cleverhans) facade's `openai`
feature, whose `llm::from_env()` bootstraps from `OPENAI_BASE_URL` +
`OPENAI_MODEL` (+ optional `OPENAI_API_KEY`).

vLLM serves tool calls only when it was started with
`--enable-auto-tool-choice` and `--tool-call-parser <parser>`; start a
reasoning model with its `--reasoning-parser` so its reasoning stays out of
the chat.
