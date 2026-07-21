# CleverHans — Rust host

Scaffolded starter: `registry.json` (the closed action contract),
`agent.rs` (agent assembly + axum mount, ready to drop into your crate),
`eval-cases.json`.

```toml
[dependencies]
cleverhans = { version = "0.1", features = ["ws", "anthropic", "ollama"] }
```

Next steps:

1. Move `agent.rs` into your crate (e.g. `src/agent.rs`), point the
   `include_str!` at this `registry.json`, and swap `Principal` for your
   real user type.
2. Mount it: your auth middleware already inserts your user as an axum
   `Extension`, so `.merge(agent_router_from_extension("/agent", agent))`
   is the whole transport.
3. Replace the handler bodies with your real execution path.
4. Add actions: edit `registry.json`, `.bind(...)` the handlers, and
   regenerate typed constants:
   `cleverhans-codegen --schema cleverhans/registry.json --rs src/generated.rs`.
5. Keep `eval-cases.json` growing with every action; run it with
   `cleverhans::evals::{load_cases, run_suite}` (feature `evals`).

A complete runnable version of this wiring:
`cargo run -p cleverhans --example mount_axum --features "ws test-util"`
(in the CleverHans repo). Guides:
https://github.com/2commits/cleverhans/tree/main/docs
