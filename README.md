# CleverHans

A propose-only, in-app, human-in-the-loop (HITL) agent framework. The agent
**never acts on the system**: it proposes actions (and dynamic UI) against an
app that already knows its own state, and the app executes through its own
normal authorized path after explicit user confirmation.

The normative protocol lives in [`spec/SPEC.md`](spec/SPEC.md); language-neutral
conformance vectors in [`spec/vectors/`](spec/vectors/README.md). Everything
else is a reference implementation of it.

## Install

Host the agent from whichever backend you already have — one Rust core under
every binding, prebuilt binaries for Node and Python:

```sh
cargo add cleverhans --features ws,anthropic   # Rust (axum mount)
npm install @cleverhans/node                   # Node host
pip install cleverhans-hitl                    # Python host
npm install @cleverhans/react @cleverhans/ui   # React frontend
```

Per-language quickstarts, action authoring, architecture, and
troubleshooting: [`docs/`](docs/README.md). Fastest complete reference:

```sh
# Mount into an existing axum app, offline (scripted model):
cargo run -p cleverhans --example mount_axum --features "ws test-util"
```

## Layout

Rust in `crates/`, TypeScript in `typescript/` (pnpm workspace), Python in
`python/`.

| Crate / package | What it is |
|---|---|
| `crates/cleverhans-core` | Registry (+ declarative schema), validation pipeline, propose-only state machine, agent loop, seam traits |
| `crates/cleverhans-grpc` | gRPC bidi-stream binding (envelope-only proto) |
| `crates/cleverhans-ws-core` | Framework-neutral session loop for WebSocket + JSON bindings |
| `crates/cleverhans-ws` | WebSocket + JSON binding (axum adapter over `cleverhans-ws-core`) |
| `crates/cleverhans-llm-anthropic` | `LlmProvider` over the Anthropic Messages API (streaming) |
| `crates/cleverhans-llm-ollama` | `LlmProvider` over a local Ollama daemon (zero egress) |
| `crates/cleverhans-codegen` | Registry document → TypeScript + Python types (library + CLI) |
| `crates/cleverhans-evals` | Action-mapping eval harness (utterance + context → expected action) |
| `crates/cleverhans-conformance` | Runner for the `spec/vectors/` conformance suite |
| `crates/cleverhans-ffi` | Shared FFI layer: JSON principal, frame pump, provider factory |
| `crates/cleverhans-py` | PyO3 extension module (packaged from `python/cleverhans-hitl`) |
| `crates/cleverhans-node` | napi-rs native module (packaged as `@cleverhans/node`) |
| `crates/cleverhans-demo` | Runnable dogfood server + eval CLI (registry authored in `registry.json`) |
| `typescript/cleverhans-react` | Headless React lib: session store, hooks, block router, primitives, WS transport |
| `typescript/cleverhans-ui` | Optional styled block pack + `<AgentChat>` window |
| `typescript/cleverhans-node` | Node backend binding: host the agent from a Node server |
| `typescript/node-demo` | Example Node host (WS server + scripted smoke flow) |
| `typescript/playground` | Local playground app for the React packages |
| `python/cleverhans-hitl` | Python backend binding (maturin package `cleverhans-hitl`) |
| `python/examples` | Example Python hosts (scripted smoke flow + FastAPI WebSocket recipe) |
| `crates/cleverhans` | Batteries-included facade: prelude, features, `examples/mount_axum.rs` |
| `docs/` | Quickstarts per language, architecture, action authoring, troubleshooting |

## Try it

```sh
# Chat against the demo document registry (pick one provider):
ANTHROPIC_API_KEY=sk-... cargo run -p cleverhans-demo -- serve
OLLAMA_MODEL=qwen3       cargo run -p cleverhans-demo -- serve
# then open http://127.0.0.1:8787

# Action-mapping evals against the same registry:
ANTHROPIC_API_KEY=sk-... cargo run -p cleverhans-demo -- eval crates/cleverhans-demo/eval-cases.json

# Host the agent from Python or Node instead (no API key needed — scripted):
python/cleverhans-hitl/.venv/bin/python python/examples/demo.py
cd typescript/node-demo && node src/smoke.ts
```

Registry types are generated from the declarative document
(`crates/cleverhans-demo/registry.json`): `cd typescript && pnpm codegen`
(`pnpm codegen:check` is the CI freshness gate). Without a Rust toolchain:
`npx cleverhans-codegen` (npm) or `cleverhans_agent.generate_types(...)` (Python).

## Tests

```sh
cargo test && cargo clippy --all-targets --all-features -- -D warnings
# The binding crates are outside default-members; lint them explicitly:
cargo clippy -p cleverhans-py -p cleverhans-node -- -D warnings
# The typescript workspace builds the native Node binding, so pnpm test and
# typecheck need a Rust toolchain:
cd typescript && pnpm install && pnpm -r test && pnpm -r typecheck
# Python binding (once): cd python/cleverhans-hitl && python3.12 -m venv .venv \
#   && .venv/bin/pip install maturin pytest
cd python/cleverhans-hitl && .venv/bin/maturin develop && .venv/bin/python -m pytest tests/
```

Conformance vectors (`spec/vectors/`) run in all three suites — Rust
(`cargo test -p cleverhans-conformance`), Python, and Node — so every
binding exhibits identical protocol behavior.

Tracking: ED-536.
