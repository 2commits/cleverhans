# cleverhans

Batteries-included facade for [CleverHans](https://github.com/nordalf/cleverhans),
a propose-only, in-app, human-in-the-loop agent framework. The agent never
acts on your system: it proposes actions from your closed registry; your app
executes through its own authorized path after explicit user confirmation.

```toml
[dependencies]
cleverhans = { version = "0.1", features = ["ws", "anthropic"] }
```

```rust,ignore
use std::sync::Arc;
use cleverhans::prelude::*;

let agent = Arc::new(Agent::new(
    Arc::new(registry),
    cleverhans::llm::from_env()?,
    Arc::new(AllowAll),
    Arc::new(schema.context_resolver()?),
));
let app = axum::Router::new().merge(agent_router_from_extension("/agent", agent));
```

The complete wiring — registry document, handlers via `.bind()`, dry-runs,
context params, WebSocket mount — lives in
[`examples/mount_axum.rs`](examples/mount_axum.rs), runnable offline:

```sh
cargo run -p cleverhans --example mount_axum --features "ws test-util"
```

Features (all off by default; the core protocol is always there): `ws` (axum
WebSocket binding), `anthropic` / `ollama` (LLM providers + `llm::from_env`),
`evals` (action-mapping eval harness), `test-util` (deterministic doubles).

Protocol spec and integration guides:
[github.com/nordalf/cleverhans](https://github.com/nordalf/cleverhans).
