---
name: cleverhans-integration
description: How to integrate and manage CleverHans (propose-only HITL agent framework) in a host app — Rust backend (registry, seams, WS mount, LLM providers, evals) and TypeScript frontend (@cleverhans/react session/hooks, @cleverhans/ui widgets). Use when adding actions, wiring an agent into an app, building custom proposal UI, or writing/running eval cases.
---

# CleverHans integration (Rust + TypeScript)

CleverHans is propose-only: the agent **never executes**. It proposes actions against app-declared context; the app executes through its own authorized path only after explicit user confirmation. The normative contract is `spec/SPEC.md` (§4 registry, §7 lifecycle, §9 seams, §12 security invariants). Never work around this model — e.g. never execute from a handler without a `ConfirmedProposal`, never let the model write context-sourced params.

Spec version: `cleverhans_core::SPEC_VERSION = "0.1"`. Proposal lifecycle: `Proposed → {Invalid, Validated}`, `Validated → {Confirmed, Rejected, Expired}`, `Confirmed → {Executed, Failed, Expired}`. External code observes states; only `ProposalStore::confirm` can mint a `ConfirmedProposal` (non-Clone confirmation witness).

Repo layout: Rust crates in `crates/`, TypeScript packages in `typescript/`, Python in `python/`. Canonical end-to-end wiring: `crates/cleverhans/examples/mount_axum.rs` (mount into an existing axum app, runs offline), `crates/cleverhans-demo/` (full Rust dogfood), `typescript/playground/src/App.tsx` (React), `typescript/node-demo/src/server.ts` (Node host via `bindAgentSocket`), `python/examples/fastapi_ws.py` (Python host). Human-facing guides live in `docs/` (per-language quickstarts, action authoring, troubleshooting).

## Rust backend

**Use the `cleverhans` facade crate** — one dependency, one prelude, instead of wiring the sub-crates by hand:

```toml
[dependencies]
cleverhans = { version = "0.1", features = ["ws", "anthropic"] }  # or "ollama", "evals"

[dev-dependencies]
cleverhans = { version = "0.1", features = ["test-util"] }
```

```rust
use cleverhans::prelude::*; // Registry, seams traits, Agent, slots!, agent_router*, providers, typed_handler…
```

Features: `ws` (axum binding), `anthropic` / `ollama` (providers + `llm::from_env`), `evals` (eval harness), `test-util` (offline test doubles). The core protocol is always present. The sub-crates (`cleverhans-core`, `cleverhans-ws`, `cleverhans-llm-*`, `cleverhans-evals`, `cleverhans-codegen`, `cleverhans-conformance`, `cleverhans-grpc`, `cleverhans-ffi`, `cleverhans-py`, `cleverhans-node`) still exist if you need one directly.

### 1. Author the registry as a document

The wire-visible registry (blocks, actions, params, context mappings — everything except handlers) is a versioned JSON document, `registry.json`. It is the single source of truth: the backend loads it, the codegen CLI reads it, conformance fixtures share it. Example entry:

```json
{
  "spec_version": "0.1",
  "blocks": [
    { "block_type": "confirm", "slots": [
      { "name": "title", "type": "string", "required": true },
      { "name": "detail", "type": "string", "required": false }
    ]}
  ],
  "actions": [
    { "id": "document.rename", "description": "Rename the selected document",
      "params": [
        { "name": "documentId", "type": "string", "source": "context", "required": true },
        { "name": "title", "type": "string", "source": "utterance", "required": true }
      ],
      "block_type": "confirm", "mutates": true, "authz_key": "documents.write" }
  ],
  "context_params": { "documentId": "selected_record_id" }
}
```

- `source: "context"` params are filled by the framework from `context_params` (a param → context-path map: `route`, `selected_record_id`, `view_type`, `params.<key>`, `extensions.<key>`); the model never sees or writes them. `source: "utterance"` params are the only ones exposed to the model.
- Every `mutates: true` action must have a dry-run handler; `.build()` rejects otherwise. A non-mutating action with a dry-run is also rejected.
- Avoid `__` in action ids — LLM providers mangle `.` → `__` for tool-name rules.

Building programmatically instead (`Registry::builder().block(...).action(...)`) is still supported and interchangeable — `.context_param(name, path)` sets the mapping.

### 2. Generate typed bindings (kills stringly-typed drift)

`cleverhans-codegen` emits typed modules from `registry.json` — one source, three consumers:

```sh
cargo run -p cleverhans-codegen -- --schema registry.json \
  --rs src/generated.rs --ts app/generated/registry.ts --py generated/registry.py
# add --check to verify freshness without writing (CI gate)
```

No Rust toolchain? The same codegen ships in the bindings: `npx cleverhans-codegen --schema registry.json --ts out.ts` (from `@cleverhans/node`) and `cleverhans_agent.generate_types(registry, target)` (PyPI `cleverhans-hitl`).

The Rust module (`--rs`) gives you `action_ids::DOCUMENT_RENAME` constants (typo'd `bind` id → compile error) and per-action params structs like `DocumentRenameParams` (serde `Deserialize`, `deny_unknown_fields`, string-enum params become generated enums). Pin freshness with a golden test comparing `rust_module(&schema.actions, &schema.blocks)` against the committed file, or `--check` in CI.

### 3. Attach handlers — `.bind()` with named setters

Load the document, bind handlers by id, build. `.bind(id, |action| ...)` takes any impl — closure, struct, or an already-`Arc`'d handler from `typed_handler` — no `Some(Arc::new(...))` wrapping:

```rust
use cleverhans::prelude::*;
use crate::generated::{DocumentRenameParams, action_ids};

let schema = RegistrySchema::from_json(include_str!("../registry.json"))?;
let registry = RegistryBuilder::from_schema(schema)
    // (a) typed closure over codegen params — no JSON digging:
    .bind(action_ids::DOCUMENT_RENAME, |action| action
        .handler(typed_handler(move |p: DocumentRenameParams, _user: MyUser| async move {
            Ok(serde_json::json!({ "id": p.document_id, "title": p.title }))
        }))
        .dry_run(typed_dry_run(move |p: DocumentRenameParams, _user: MyUser| async move {
            Ok(DryRunPreview { affected_count: 1, ..Default::default() })
        }))
        .slots(|params: &JsonMap, _: Option<&DryRunPreview>| slots! {
            "title": "Rename document",
        }))
    // (b) plain closure over the raw JsonMap + fixed card:
    .bind(action_ids::DOCUMENT_PUBLISH, |action| action
        .handler(|params: JsonMap, _user: MyUser| async move { Ok(serde_json::Value::Null) })
        .dry_run(|_: JsonMap, _: MyUser| async move { Ok(DryRunPreview::default()) })
        .static_slots(slots! { "title": "Publish document" }))
    // (c) trait impl on a struct — for owned state, or one type serving both seams
    .bind(action_ids::DOCUMENTS_DELETE_BY_STATUS, |action| action
        .handler(DeleteByStatus(store.clone()))
        .dry_run(DeleteByStatus(store.clone()))
        .static_slots(slots! { "title": "Bulk delete" }))
    .build()?;
```

Closure handlers work via blanket impls (require `P: Clone`); `typed_handler` / `typed_dry_run` wrap a closure over a codegen params struct. Struct impls (`impl ActionHandler<P> for T`, with `#[async_trait]` re-exported from the prelude) remain for stateful handlers. A binding with no `.handler(...)` fails `.build()` with `MissingHandler`. The positional `.attach(id, handler, dry_run, slot_builder)` form still exists.

The other two seams:
- `AuthzResolver<P>` — `Allow` / `Deny(reason)`, called at propose AND confirm time. Three forms: the shipped `AllowAll` (demos/tests/transport-auth-only apps), an async closure `|principal, action_id: String, params: JsonMap| async move { ... }` via the blanket impl, or a trait impl over your permission system.
- Context params: `schema.context_resolver()?` (a `MappedContextResolver` over the document's `context_params`) for the common case — **zero app code**. It returns `Err(UnmappedContextParam)` at startup if any context-sourced param lacks a mapping. Implement `ContextParamResolver` yourself only for richer needs.

Validation order (propose and confirm time): existence → param fill + typecheck (context via resolver; unknown params and model writes to context params rejected) → authz → dry-run (iff mutates) → slot build → slot check. `ValidationFailure::is_model_fixable()` gates agent retries.

### 4. Pick an LLM provider

`llm::from_env()` is the bootstrap every app was going to copy: `OLLAMA_MODEL` wins, else `ANTHROPIC_API_KEY` (+ optional `ANTHROPIC_MODEL`), else a clear error naming the accepted vars.

```rust
let llm = cleverhans::llm::from_env()?;
// or explicitly:
let llm: Arc<dyn LlmProvider> = Arc::new(AnthropicProvider::new(AnthropicConfig::new(key)));
let llm: Arc<dyn LlmProvider> = Arc::new(OllamaProvider::new(OllamaConfig::new("qwen3".into())));
```

Custom providers implement `LlmProvider::complete` (and optionally `complete_stream`).

### 5. Assemble the agent and mount the WebSocket

```rust
let agent = Arc::new(Agent::with_config(
    Arc::new(registry), llm, Arc::new(MyAuthz), Arc::new(schema.context_resolver()?),
    AgentConfig { app_instructions: Some("This app is ...".into()), ..Default::default() },
));
```

Two router flavors — pick by how the host app authenticates:

```rust
// (a) Existing tower/axum auth middleware already resolves the caller into
//     request extensions — reuse it, no second auth path:
let app = Router::new()
    .merge(agent_router_from_extension("/agent", agent))
    .layer(my_auth_layer); // inserts Extension<MyUser>; missing → 401

// (b) Header-based auth at upgrade (cookie/bearer): implement PrincipalExtractor:
let app = Router::new().merge(agent_router("/agent", agent, Arc::new(HeaderAuth)));
```

The framework never constructs a principal. `AgentConfig`: `app_instructions` (appended to the non-replaceable `DEFAULT_SYSTEM_PROMPT`), `max_validation_retries` (default 2), `describe_context` (default true). On assembly, `Agent` logs the registry it sees via `tracing` (`info`: one line per action + a summary) — enable a subscriber to catch a missing action or wrong block at startup. Non-axum transports: `cleverhans_ws_core::run_session(...)` directly, or `cleverhans-grpc`.

### 6. Test offline, then eval

`test-util` ships `ScriptedLlm` — drive the whole propose→confirm→execute pipeline with the model replaced by a script, no network:

```rust
use cleverhans::test_util::ScriptedLlm;
let llm = Arc::new(ScriptedLlm::new([
    vec![CompletionItem::ToolCall { name: "document.rename".into(), arguments: slots! { "title": "Roadmap" } }],
    vec![CompletionItem::Text("Renamed.".into())],
]));
// pass llm.clone() to Agent; assert on llm.requests() afterward
```

Eval cases (action-mapping accuracy against a real model) are a JSON array — utterance + context → expected action or decline:

```json
[
  { "name": "rename in detail view", "utterance": "rename this to Roadmap",
    "context": { "route": "/documents/doc-1", "selected_record_id": "doc-1", "view_type": "detail" },
    "expected": { "kind": "action", "action_id": "document.rename", "params": { "title": "Roadmap" } } },
  { "name": "off-registry", "utterance": "email this", "expected": { "kind": "decline" } }
]
```

Param match is a subset. Run: `cargo run -p cleverhans-demo -- eval crates/cleverhans-demo/eval-cases.json` (exits non-zero on any failure). Programmatic: `cleverhans::evals::{load_cases, run_suite}`.

## TypeScript frontend

Packages under `typescript/`: `@cleverhans/react` (headless: session store, hooks, block router, WS transport) and `@cleverhans/ui` (styled `AgentChat` / `FloatingChat` + default blocks). Also `@cleverhans/node` (Node binding). Canonical example: `typescript/playground/src/App.tsx`. Keep TS envelope/registry types in sync via `cleverhans-codegen --ts` (committed to `typescript/playground/src/generated/registry.ts`; freshness is pinned by a Rust test).

### Session setup

```tsx
import { AgentSession, createWebSocketTransport } from "@cleverhans/react";

const transport = createWebSocketTransport("ws://127.0.0.1:8787/agent");
const session = new AgentSession(transport, {
  context: { route: "/documents", selected_record_id: null, view_type: "list" },
});
```

Create once (`useMemo`); the app owns the lifetime. Keep context synced with navigation — the agent targets whatever the context says the user is standing on, and navigation expires pending proposals:

```tsx
useEffect(() => {
  session.updateContext({ route: `/documents/${id}`, selected_record_id: id, view_type: "detail" });
}, [session, id]);
```

### Fast path: styled widgets

```tsx
import { FloatingChat /* or AgentChat */ } from "@cleverhans/ui";
import "@cleverhans/ui/styles.css";
<FloatingChat session={session} />
```

Default blocks `ConfirmBlock` / `BulkPreviewBlock` (`DEFAULT_BLOCKS`) render the demo registry's block types; pass custom `components` (merged over defaults) for app-specific block types.

### Headless path: custom UI

```tsx
import { AgentProvider, useAgentSession, useAgentProposal, BlockRouter, PendingProposals } from "@cleverhans/react";

<AgentProvider session={session}>{children}</AgentProvider>
const { snapshot, sendMessage, updateContext, confirm, reject } = useAgentSession();
const { view, confirm, reject } = useAgentProposal(proposalId);
```

`confirm(id)` / `reject(id, reason?)` are the ONLY proposal writes a frontend can perform. `BlockRouter` maps `proposal.block_type` → your component; `PendingProposals` renders all non-terminal proposals.

### Reflecting executed actions in app state

The server reports execution via `ProposalStateChanged { state: "executed", result }`. Read from the snapshot and fold into app state as a pure derivation (see `applyResults` in the playground):

```tsx
const snapshot = useSyncExternalStore(session.subscribe, session.getSnapshot);
const docs = applyResults(SEED, snapshot.proposals); // filter view.state === "executed", apply view.result
```

## Managing the integration

- **Add an action**: edit `registry.json` (+ block if new) + `context_params` mapping → regenerate bindings (codegen `--rs`/`--ts`/`--py`) → attach handler + dry-run (if mutating) + slot builder in the builder → frontend block component (or reuse `confirm`/`bulk_preview`) → eval cases (happy path, context-dependent targeting with/without selection, a decline near the boundary).
- **Dev loop**: `cargo run -p cleverhans-demo -- serve` (needs `ANTHROPIC_API_KEY` or `OLLAMA_MODEL`) → `pnpm --filter @cleverhans/playground dev`. Demo also serves a plain chat page at `http://127.0.0.1:8787`.
- **Full test gate**: `cargo test && cargo clippy --all-targets --all-features -- -D warnings` (plus `cargo clippy -p cleverhans-py -p cleverhans-node` — those two are excluded from default-members) `&& pnpm -r test && pnpm -r typecheck`.
- **Versioning**: client `Init` carries `spec_version`; major.minor must be compatible (spec §13). `RegistrySchema::from_json` version-gates the document the same way.
- **Security invariants** (spec §12) that must survive any change: model emits only utterance params; mutating actions always dry-run; execution requires a `ConfirmedProposal`; revalidation at confirm time; principals come only from the app's extractor/middleware.
