# Rust quickstart (axum)

```toml
[dependencies]
cleverhans = { version = "0.1", features = ["ws", "anthropic"] }

[dev-dependencies]
cleverhans = { version = "0.1", features = ["test-util"] }
```

The complete runnable version of everything below is
[`crates/cleverhans/examples/mount_axum.rs`](../crates/cleverhans/examples/mount_axum.rs):

```sh
cargo run -p cleverhans --example mount_axum --features "ws test-util"
```

## 1. Author the registry document

`registry.json` declares blocks (UI card types + slot schemas) and actions
(id, description, params, `mutates`, `authz_key`). Params with
`source: "context"` are filled by the framework from your app context — the
model never sees or writes them; `source: "utterance"` params are the only
ones the model fills. Map context params at the bottom:

```json
{
  "spec_version": "0.1",
  "blocks": [
    { "block_type": "confirm",
      "slots": [{ "name": "title", "type": "string", "required": true }] }
  ],
  "actions": [
    { "id": "record.archive",
      "description": "Archive the currently selected record",
      "params": [
        { "name": "recordId", "type": "string", "source": "context", "required": true }
      ],
      "block_type": "confirm", "mutates": true, "authz_key": "record.archive" }
  ],
  "context_params": { "recordId": "selected_record_id" }
}
```

## 2. Bind handlers, build the agent

```rust
use std::sync::Arc;
use cleverhans::prelude::*;

let schema = RegistrySchema::from_json(include_str!("../registry.json"))?;
let context_resolver = schema.context_resolver()?; // unmapped params fail at startup

let registry = RegistryBuilder::from_schema(schema)
    .bind("record.archive", |action| action
        .handler(|params: JsonMap, user: MyUser| async move {
            // your app's normal, already-authorized execution path
            Ok(serde_json::json!({ "archived": params["recordId"] }))
        })
        .dry_run(|params: JsonMap, _: MyUser| async move {
            Ok(DryRunPreview { affected_count: 1, ..Default::default() })
        })
        .static_slots(slots! { "title": "Archive record" }))
    .build()?;

let agent = Arc::new(Agent::new(
    Arc::new(registry),
    cleverhans::llm::from_env()?,   // ANTHROPIC_API_KEY or OLLAMA_MODEL
    Arc::new(AllowAll),             // or a closure / trait impl over your permissions
    Arc::new(context_resolver),
));
```

Every `mutates: true` action needs a `.dry_run(...)` — the side-effect-free
preview users confirm against; `.build()` rejects otherwise. Handlers are
closures, structs (`#[async_trait] impl ActionHandler<P>` — `async_trait` is
re-exported from the prelude), or `typed_handler` over codegen'd param
structs (see [Adding an action](adding-actions.md)).

## 3. Mount

Your existing auth middleware already inserts your user type as an axum
`Extension`? One line:

```rust
let app = Router::new()
    .merge(cleverhans::ws::agent_router_from_extension("/agent", agent))
    .layer(my_auth_layer);
```

Header/cookie auth at upgrade instead: implement `PrincipalExtractor` and use
`agent_router("/agent", agent, Arc::new(MyExtractor))`. Non-axum hosts:
`cleverhans-ws-core::run_session` over any socket you own.

## 4. Test offline

`ScriptedLlm` (feature `test-util`) replaces the model with a script and
drives the real propose → confirm → execute pipeline — fast, offline, exact.
Then measure action-mapping accuracy against a live model with the `evals`
feature. See the [facade README](../crates/cleverhans/README.md).
