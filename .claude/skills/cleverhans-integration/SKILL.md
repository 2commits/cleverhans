---
name: cleverhans-integration
description: How to integrate and manage CleverHans (propose-only HITL agent framework) in a host app — Rust backend (registry, seams, WS mount, LLM providers, evals) and TypeScript frontend (@cleverhans/react session/hooks, @cleverhans/ui widgets). Use when adding actions, wiring an agent into an app, building custom proposal UI, or writing/running eval cases.
---

# CleverHans integration (Rust + TypeScript)

CleverHans is propose-only: the agent **never executes**. It proposes actions against app-declared context; the app executes through its own authorized path only after explicit user confirmation. The normative contract is `spec/SPEC.md` (§4 registry, §7 lifecycle, §9 seams, §12 security invariants). Never work around this model — e.g. never execute from a handler without a `ConfirmedProposal`, never let the model write context-sourced params.

Spec version: `cleverhans_core::SPEC_VERSION = "0.1"`. Proposal lifecycle: `Proposed → {Invalid, Validated}`, `Validated → {Confirmed, Rejected, Expired}`, `Confirmed → {Executed, Failed, Expired}`. External code observes states; only `ProposalStore::confirm` can mint a `ConfirmedProposal` (non-Clone confirmation witness).

## Rust backend

Crates (workspace members, dep names as-is): `cleverhans-core`, `cleverhans-ws` (+ framework-neutral `cleverhans-ws-core`), `cleverhans-llm-anthropic`, `cleverhans-llm-ollama`, `cleverhans-evals`, `cleverhans-grpc` (alt transport), `cleverhans-codegen` (registry → TS types). Canonical end-to-end wiring: `crates/cleverhans-demo/src/main.rs` and `crates/cleverhans-demo/src/registry.rs`.

### 1. Define the registry

Everything is generic over your app's principal type `P` (e.g. `DemoUser`).

```rust
use cleverhans_core::registry::*;
use cleverhans_core::seams::static_slots;
use cleverhans_core::slots;

let registry = Registry::<MyUser>::builder()
    .block(BlockDef {
        block_type: "confirm".into(),
        slots: vec![
            SlotSpec { name: "title".into(), ty: ValueType::String, required: true },
            SlotSpec { name: "detail".into(), ty: ValueType::String, required: false },
        ],
    })
    .action(
        ActionDef {
            id: "document.rename".into(),
            description: "Rename the currently selected document".into(),
            params: vec![
                ParamSpec { name: "documentId".into(), description: "...".into(),
                            ty: ValueType::String, source: ParamSource::Context, required: true },
                ParamSpec { name: "title".into(), description: "New title".into(),
                            ty: ValueType::String, source: ParamSource::Utterance, required: true },
            ],
            block_type: "confirm".into(),
            mutates: true,
            authz_key: "documents.write".into(),
        },
        Arc::new(RenameHandler),          // ActionHandler<P>
        Some(Arc::new(RenameDryRun)),     // DryRunHandler<P> — REQUIRED when mutates: true
        Some(Arc::new(|params: &cleverhans_core::JsonMap, _preview: Option<&DryRunPreview>| slots! {
            "title": "Rename document",
            "detail": format!("New title: {}", params["title"]),
        })),
    )
    .build()?; // errors: DuplicateAction, DuplicateBlock, UnknownBlockType, MissingDryRun
```

Key rules:
- `ParamSource::Context` params are filled by the framework via your `ContextParamResolver` — the model never sees or writes them. `ParamSource::Utterance` params are the only ones exposed in `Registry::tool_defs()`.
- Every `mutates: true` action must supply a dry-run handler; `.build()` rejects otherwise.
- Slot builders: closures implement `SlotBuilder` directly (blanket impl); use `static_slots(slots! { ... })` for fixed cards. The `slots!` macro takes `json!`-object syntax.
- Avoid `__` in action ids — LLM providers mangle `.` → `__` for tool-name rules.

### 2. Implement the seams (`cleverhans_core::seams`)

```rust
#[async_trait]
impl ActionHandler<MyUser> for RenameHandler {
    async fn execute(&self, params: &JsonMap, principal: &MyUser)
        -> Result<serde_json::Value, HandlerError> { /* app's own authorized path */ }
}
#[async_trait]
impl DryRunHandler<MyUser> for RenameDryRun {
    async fn dry_run(&self, params: &JsonMap, principal: &MyUser)
        -> Result<DryRunPreview, HandlerError> { /* affected_count, sample_ids, summary */ }
}
#[async_trait]
impl AuthzResolver<MyUser> for MyAuthz {
    async fn authorize(&self, principal: &MyUser, action_id: &str, params: &JsonMap)
        -> AuthzDecision { AuthzDecision::Allow /* or Deny(reason) */ }
}
impl ContextParamResolver for SelectionResolver {  // sync, not generic
    fn resolve(&self, action_id: &str, param: &ParamSpec, context: &Context)
        -> Option<serde_json::Value> {
        (param.name == "documentId")
            .then(|| context.selected_record_id.clone().map(Into::into)).flatten()
    }
}
```

Validation order (runs at propose AND confirm time): existence → param fill + typecheck (context via resolver; unknown params and model writes to context params rejected) → authz → dry-run (iff mutates) → slot build → slot check. `ValidationFailure::is_model_fixable()` gates agent retries.

### 3. Pick an LLM provider

```rust
// Anthropic (DEFAULT_MODEL = "claude-opus-4-8"):
let mut cfg = AnthropicConfig::new(std::env::var("ANTHROPIC_API_KEY")?);
if let Ok(m) = std::env::var("ANTHROPIC_MODEL") { cfg.model = m; }  // model override
let llm: Arc<dyn LlmProvider> = Arc::new(AnthropicProvider::new(cfg));

// Ollama (zero egress, no credential):
let llm: Arc<dyn LlmProvider> = Arc::new(OllamaProvider::new(OllamaConfig::new("qwen3".into())));
```

Custom providers implement `LlmProvider::complete` (and optionally `complete_stream`).

### 4. Assemble the agent and mount the WebSocket

```rust
let agent = Arc::new(Agent::with_config(
    Arc::new(registry), llm, Arc::new(MyAuthz), Arc::new(SelectionResolver),
    AgentConfig { app_instructions: Some("This app is ...".into()), ..AgentConfig::default() },
));

// PrincipalExtractor maps HTTP headers → principal at WS upgrade; the framework
// never constructs a principal itself. Do real auth here.
struct HeaderAuth;
impl PrincipalExtractor<MyUser> for HeaderAuth {
    fn extract(&self, headers: &HeaderMap) -> Result<MyUser, StatusCode> { /* ... */ }
}

let app = axum::Router::new()
    .merge(cleverhans_ws::agent_router("/agent", agent, Arc::new(HeaderAuth)));
axum::serve(listener, app).await?;
```

- `AgentConfig`: `app_instructions` (appended to the non-replaceable `DEFAULT_SYSTEM_PROMPT`), `max_validation_retries` (default 2), `describe_context` (default true).
- One `Session::new(principal)` per authenticated stream; `agent_router` handles this.
- The session loop enforces init-first: first frame must be `ClientEvent::Init` or the socket closes with `init_required`.
- Logging is `tracing`-based inside `run_session`; demo filter: `"info,cleverhans_ws=info"`, override with `RUST_LOG`.
- Non-axum transports: use `cleverhans_ws_core::run_session(agent, principal, inbound_string_stream, tx)` directly, or `cleverhans-grpc`.

### 5. Evals

Cases are a JSON array (`crates/cleverhans-demo/eval-cases.json` is the sample):

```json
[
  { "name": "rename in detail view",
    "utterance": "rename this to Roadmap",
    "context": { "route": "/documents/doc-1", "selected_record_id": "doc-1", "view_type": "detail" },
    "expected": { "kind": "action", "action_id": "document.rename", "params": { "title": "Roadmap" } } },
  { "name": "off-registry request",
    "utterance": "email this to the team",
    "expected": { "kind": "decline" } }
]
```

Param match is a subset (context-filled params may be omitted). Programmatic: `cleverhans_evals::{load_cases, run_suite}` → `EvalReport` with `.accuracy()` / `.all_passed()`. CLI:

```sh
ANTHROPIC_API_KEY=... cargo run -p cleverhans-demo -- eval crates/cleverhans-demo/eval-cases.json
```

Exits non-zero on any failure. When adding an action, add eval cases covering: happy path, context-dependent targeting (with and without selection), and a decline case near its semantic boundary.

## TypeScript frontend

Packages: `@cleverhans/react` (headless: session store, hooks, block router, WS transport) and `@cleverhans/ui` (styled `AgentChat` / `FloatingChat` + default blocks). Canonical example: `packages/playground/src/App.tsx`. Keep TS envelope types in sync with Rust via `cleverhans-codegen` (Rust registry → TS types).

### Session setup (framework-agnostic core)

```tsx
import { AgentSession, createWebSocketTransport } from "@cleverhans/react";

const transport = createWebSocketTransport("ws://127.0.0.1:8787/agent");
const session = new AgentSession(transport, {
  context: { route: "/documents", selected_record_id: null, view_type: "list" },
});
```

Create once (e.g. `useMemo`); the app owns the session lifetime. Keep context synced with navigation — the agent targets whatever the context says the user is standing on, and navigation expires pending proposals:

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

Default blocks `ConfirmBlock` / `BulkPreviewBlock` (`DEFAULT_BLOCKS`) render the demo registry's block types; pass custom `BlockComponents` for app-specific block types.

### Headless path: custom UI

```tsx
import { AgentProvider, useAgentSession, useAgentProposal, BlockRouter, PendingProposals } from "@cleverhans/react";

<AgentProvider session={session}>{children}</AgentProvider>

const { snapshot, sendMessage, updateContext, confirm, reject } = useAgentSession();
const { view, confirm, reject } = useAgentProposal(proposalId);
```

`confirm(id)` / `reject(id, reason?)` are the ONLY proposal writes a frontend can perform. `BlockRouter` maps `proposal.block_type` → your component (props: `BlockProps` with slots + lifecycle handle); `PendingProposals` renders all non-terminal proposals.

### Reflecting executed actions in app state

The server reports execution results via `ProposalStateChanged { state: "executed", result }`. Read them from the snapshot and fold into app state as a pure derivation (see `applyResults` in the playground):

```tsx
const snapshot = useSyncExternalStore(session.subscribe, session.getSnapshot);
const docs = applyResults(SEED, snapshot.proposals); // filter view.state === "executed", apply view.result
```

## Managing the integration

- **Add an action**: registry entry (+ block if new) → handler + dry-run (if mutating) → slot builder → `ContextParamResolver` coverage for its context params → frontend block component (or reuse `confirm`/`bulk_preview`) → eval cases.
- **Dev loop**: `cargo run -p cleverhans-demo -- serve` (needs `ANTHROPIC_API_KEY` or `OLLAMA_MODEL`) → `pnpm --filter @cleverhans/playground dev`. Demo server also serves a plain chat page at `http://127.0.0.1:8787`.
- **Full test gate**: `cargo test && cargo clippy --all-targets --all-features -- -D warnings && pnpm -r test && pnpm -r typecheck`.
- **Versioning**: client `Init` carries `spec_version`; major.minor must be compatible (spec §13). Bump `SPEC_VERSION` only with a spec change.
- **Security invariants** (spec §12) that must survive any change: model can only emit utterance params; mutating actions always dry-run; execution requires a `ConfirmedProposal`; revalidation happens at confirm time; principals come only from the app's extractor.
