# CleverHans

A propose-only, in-app, human-in-the-loop (HITL) agent framework. The agent
**never acts on the system**: it proposes actions (and dynamic UI) against an
app that already knows its own state, and the app executes through its own
normal authorized path after explicit user confirmation.

The normative protocol lives in [`spec/SPEC.md`](spec/SPEC.md). Everything
else is a reference implementation of it.

## Layout

| Crate / package | What it is |
|---|---|
| `crates/cleverhans-core` | Registry, validation pipeline, propose-only state machine, agent loop, seam traits |
| `crates/cleverhans-grpc` | gRPC bidi-stream binding (envelope-only proto) |
| `crates/cleverhans-ws` | WebSocket + JSON binding (axum) |
| `crates/cleverhans-llm-anthropic` | `LlmProvider` over the Anthropic Messages API (streaming) |
| `crates/cleverhans-llm-ollama` | `LlmProvider` over a local Ollama daemon (zero egress) |
| `crates/cleverhans-codegen` | Rust registry → TypeScript types |
| `crates/cleverhans-evals` | Action-mapping eval harness (utterance + context → expected action) |
| `crates/cleverhans-demo` | Runnable dogfood server + eval CLI |
| `packages/cleverhans-react` | Headless React lib: session store, hooks, block router, primitives, WS transport |
| `packages/cleverhans-ui` | Optional styled block pack + `<AgentChat>` window |

## Try it

```sh
# Chat against the demo document registry (pick one provider):
ANTHROPIC_API_KEY=sk-... cargo run -p cleverhans-demo -- serve
OLLAMA_MODEL=qwen3       cargo run -p cleverhans-demo -- serve
# then open http://127.0.0.1:8787

# Action-mapping evals against the same registry:
ANTHROPIC_API_KEY=sk-... cargo run -p cleverhans-demo -- eval crates/cleverhans-demo/eval-cases.json
```

## Tests

```sh
cargo test && cargo clippy --all-targets --all-features -- -D warnings
pnpm install && pnpm -r test && pnpm -r typecheck
```

Tracking: ED-536.
