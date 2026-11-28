# @cleverhans/node

Host a [CleverHans](https://github.com/2commits/cleverhans) propose-only
agent from a Node.js backend. The agent never acts on your system: it
proposes actions from your closed registry; your handlers execute only
after explicit user confirmation, under the principal you bound to the
session. The validation pipeline and agent loop run in the shared Rust core
(prebuilt binaries per platform — no Rust toolchain needed).

```ts
import { Agent, bindAgentSocket } from "@cleverhans/node";
import { WebSocketServer } from "ws";

const agent = new Agent({
  registry: "registry.json",              // path, JSON string, or object
  handlers: { "doc.publish": publish },   // (params, principal) sync or async
  dryRuns: { "doc.publish": publishDryRun },
  authorize,                              // null/true allow, string denies
  llm: { provider: "anthropic", api_key: process.env.ANTHROPIC_API_KEY },
});

new WebSocketServer({ port: 8788, path: "/agent" }).on("connection", (ws, request) => {
  const principal = authenticate(request); // your auth, before any frame
  ws.on("message", bindAgentSocket(agent, principal, ws));
});
```

- `bindAgentSocket(agent, principal, socket)` — the whole WS glue for any
  socket-shaped object (`ws`, uWS, Express/Fastify WS plugins). When a
  non-recoverable error ends the session (`init_required`,
  `unsupported_spec_version`) it closes the socket with code 1008 and the
  error code as the reason.
- Subprotocol: your server selects `SUBPROTOCOL` (`"cleverhans.v0.1"`) when
  the client offers it; `@cleverhans/react` offers it first whenever it is
  given `protocols`. `ws`'s `WebSocketServer` does this by default (it picks
  the first offered value).
- `session.handle(frame, onEvent)` — the lower-level per-frame pump; it
  resolves `true` once the session is over, and `session.closeCode` /
  `session.closeReason` say how to close.
- Throw `Rejected` from a handler to decline as a business rule.
- `generateTypes(registry, target)` / `npx cleverhans-codegen` — registry →
  typed TS/Python/Rust modules, no Rust toolchain.
