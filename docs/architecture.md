# Architecture

One Rust core implements the protocol; every host language binds to it. The
propose-only invariants (model emits only utterance params; mutating actions
always dry-run; execution requires a confirmed proposal; revalidation at
confirm time) live in exactly one place and are enforced identically for
Rust, Node, and Python hosts — the `spec/vectors/` conformance suite runs in
all three to prove it.

```
                    ┌───────────────────────────────┐
                    │        cleverhans-core         │  registry · validation ·
                    │  (the protocol, one impl)      │  state machine · agent loop ·
                    └──────┬────────────┬───────────┘  seam traits
        transports         │            │           providers
  ┌───────────────┐  ┌─────┴─────┐  ┌───┴──────────────────┐
  │ cleverhans-ws  │  │ ws-core   │  │ llm-anthropic /       │
  │ (axum)         │  │ (neutral) │  │ llm-ollama            │
  └───────────────┘  └─────┬─────┘  └──────────────────────┘
                           │
                    ┌──────┴───────┐
                    │ cleverhans-ffi │  JSON principal · frame pump ·
                    └──┬─────────┬──┘  provider factory · codegen
                       │         │
              ┌────────┴──┐  ┌───┴────────┐
              │ -py (PyO3) │  │ -node (napi)│
              └────────┬──┘  └───┬────────┘
                       │         │
              PyPI `cleverhans-hitl`  npm `@cleverhans/node`
```

- **`cleverhans`** (crates.io) — the facade: one dependency line, one
  prelude; `ws` / `anthropic` / `ollama` / `evals` / `test-util` features.
- **`cleverhans-ws-core` → `cleverhans-ws`** — a framework-neutral
  JSON-frame session loop, and the axum adapter over it. `cleverhans-grpc`
  is the gRPC binding (envelope-only proto).
- **`cleverhans-ffi`** — shared by both language bindings: principals as
  JSON blobs, per-frame pumping, the `anthropic | ollama | scripted |
  callback` provider factory, registry assembly from declarative documents,
  and codegen. A knob added here is a knob added everywhere.
- **Frontend** never touches the core: `@cleverhans/react` speaks the
  envelope over WebSocket; `@cleverhans/ui` is optional styled components
  over it.
- **`cleverhans-codegen`** — the registry document is the single source;
  generated TS/Python/Rust modules keep every consumer typed. Exposed
  through both bindings so non-Rust teams get it from their package manager.

Deployment shape (spec §10): the agent lives *inside* your backend process.
There is no separate agent service, no second auth path — the principal your
middleware already resolved is the principal every handler and authz check
runs under.
