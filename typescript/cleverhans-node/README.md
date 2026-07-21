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
  socket-shaped object (`ws`, uWS, Express/Fastify WS plugins).
- `session.handle(frame, onEvent)` — the lower-level per-frame pump.
- Throw `Rejected` from a handler to decline as a business rule.
- `generateTypes(registry, target)` / `npx cleverhans-codegen` — registry →
  typed TS/Python/Rust modules, no Rust toolchain.
