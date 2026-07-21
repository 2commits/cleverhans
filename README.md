# CleverHans

**Put an AI agent inside your app — without ever letting it touch your system.**

CleverHans is a propose-only, human-in-the-loop agent framework. The agent
reads what the user says, sees what your app already knows about its own
state, and **proposes** actions as rich, confirmable UI cards. Nothing runs
until the user clicks confirm — and then it runs through *your* code, on
*your* normal authorized path. The model never executes anything, never sees
your credentials, and never fills in parameters your app already knows.

One Rust core powers every binding: host the agent from **Rust**, **Node.js**,
or **Python** (prebuilt binaries — no Rust toolchain needed for Node/Python),
and drop the chat UI into **React**.

## See it in two minutes

All you need is a Rust toolchain and a model — either an Anthropic API key or
a local [Ollama](https://ollama.com) daemon (zero egress):

```sh
git clone https://github.com/2commits/cleverhans && cd cleverhans

ANTHROPIC_API_KEY=sk-... cargo run -p cleverhans-demo -- serve
# or fully local:
OLLAMA_MODEL=qwen3       cargo run -p cleverhans-demo -- serve
```

Open <http://127.0.0.1:8787> and try:

> *"publish the Q3 planning doc"* — the agent proposes it, you confirm, the
> app executes.
>
> *"delete all draft documents"* — a mutating bulk action: you get a
> side-effect-free **dry-run preview** (what would be affected) before
> anything can happen.

No API key at all? Run the scripted, no-network flow to watch the full
propose → confirm → execute pipeline offline:

```sh
cargo run -p cleverhans --example mount_axum --features "ws test-util"
```

## Add it to your app

Three pieces, in any backend language:

1. **A registry** (`registry.json`) — declares the actions the agent may
   propose and the UI blocks it renders them with.
2. **An agent** — binds your handlers (your existing business logic) to those
   actions and connects a model.
3. **A WebSocket mount** — one endpoint behind your existing auth.

### 1. Declare actions in `registry.json`

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

The key idea: `source: "context"` params are filled by the framework from
your app's live context — the model never sees or writes them. The model only
fills `source: "utterance"` params, and everything it produces is validated
against this schema before your app ever sees it.

### 2. Host the agent from your backend

Pick your language — same core, same protocol, same registry file:

**Node.js** — `npm install @cleverhans/node ws`

```ts
import { Agent, bindAgentSocket } from "@cleverhans/node";
import { WebSocketServer } from "ws";

const agent = new Agent({
  registry: "registry.json",
  handlers: {
    "record.archive": async (params, principal) => archive(params.recordId),
  },
  dryRuns: {
    "record.archive": async (params) => ({ affected_count: 1, sample_ids: [params.recordId] }),
  },
  authorize: (principal, actionId) => principal.roles.includes("editor") ? null : "editors only",
  llm: { provider: "anthropic", api_key: process.env.ANTHROPIC_API_KEY },
});

const wss = new WebSocketServer({ port: 8788, path: "/agent" });
wss.on("connection", (ws, request) => {
  const principal = authenticate(request); // your auth, before any frame
  ws.on("message", bindAgentSocket(agent, principal, ws));
});
```

**Python** — `pip install cleverhans-hitl`

```python
import cleverhans_agent

agent = cleverhans_agent.Agent(
    registry="registry.json",
    handlers={"record.archive": archive},        # sync or async def
    dry_runs={"record.archive": archive_dry_run},
    authorize=authorize,
    llm={"provider": "anthropic", "api_key": os.environ["ANTHROPIC_API_KEY"]},
)
# then bridge agent.session(principal) to any WebSocket route —
# full FastAPI recipe in docs/quickstart-python.md
```

**Rust** — `cargo add cleverhans --features ws,anthropic`

```rust
let app = Router::new()
    .merge(cleverhans::ws::agent_router_from_extension("/agent", agent))
    .layer(my_auth_layer);   // full walkthrough in docs/quickstart-rust.md
```

Every `mutates: true` action requires a dry-run — the side-effect-free
preview users confirm against. The framework refuses to build without one.

### 3. Drop the chat into your React app

```sh
npm install @cleverhans/react @cleverhans/ui
```

```tsx
import { AgentSession, createWebSocketTransport } from "@cleverhans/react";
import { FloatingChat } from "@cleverhans/ui";
import "@cleverhans/ui/styles.css";

const session = new AgentSession(
  createWebSocketTransport("wss://your-app/agent"),
  { context: { route: "/records", selected_record_id: null } },
);

export function App() {
  return (
    <>
      <YourApp />
      <FloatingChat session={session} />
    </>
  );
}
```

Keep the context synced as the user navigates (`session.updateContext(...)`)
and the agent always targets what the user is actually looking at. Prefer
your own UI? `@cleverhans/react` is fully headless — see
[docs/quickstart-react.md](docs/quickstart-react.md).

Generate typed action IDs and param types for your frontend from the same
registry: `npx cleverhans-codegen --schema registry.json --ts src/generated/registry.ts`.

## How it works

```
user message ──► agent (LLM) ──► validated proposal + dry-run preview
                                        │
                                 user confirms in UI
                                        │
                    your handler runs on your authorized path ──► result
```

- **Propose-only.** The model's output is a proposal, never an execution. It
  is schema-validated, authorization-checked, and context-resolved before it
  reaches the user.
- **Your app stays in charge.** Handlers are your existing functions;
  authorization is your existing rules; auth rides your existing transport.
  The protocol envelope never carries credentials.
- **Previews before mutations.** Mutating actions must implement a dry-run,
  so users confirm against what *will* happen, not a guess.
- **Test without a model.** A scripted LLM (`test-util` / `provider:
  "scripted"`) drives the real pipeline offline; an eval harness measures
  action-mapping accuracy against live models.

The wire protocol is specified in [`spec/SPEC.md`](spec/SPEC.md), with
language-neutral conformance vectors in [`spec/vectors/`](spec/vectors/README.md)
that all three bindings pass — every host behaves identically.

## Learn more

| | |
|---|---|
| [Rust quickstart](docs/quickstart-rust.md) | Mount into an existing axum app |
| [Node.js quickstart](docs/quickstart-node.md) | Host from any Node WebSocket server |
| [Python quickstart](docs/quickstart-python.md) | FastAPI recipe |
| [React quickstart](docs/quickstart-react.md) | Styled drop-in or headless hooks |
| [Adding an action](docs/adding-actions.md) | Registry edit → codegen → handlers → UI → evals |
| [Architecture](docs/architecture.md) | How the one Rust core sits under every binding |
| [Troubleshooting](docs/troubleshooting.md) | Common integration issues |

## License

Apache-2.0. See [LICENSE](LICENSE).
