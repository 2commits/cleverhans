# Node.js quickstart

```sh
npm install @cleverhans/node ws
```

Prebuilt binaries per platform — no Rust toolchain. The validation pipeline,
state machine, and agent loop are the same Rust core every binding shares.

```ts
import { Agent, Rejected, bindAgentSocket } from "@cleverhans/node";
import { WebSocketServer } from "ws";

const agent = new Agent({
  registry: "registry.json",   // path, JSON string, or object — see the Rust
                               // quickstart for the document format
  handlers: {
    "doc.publish": async (params, principal) => {
      const doc = DOCS.get(params.docId);
      if (!doc) throw new Rejected("document not found"); // business decline
      doc.published = true;
      return { published: params.docId };
    },
  },
  dryRuns: {
    "doc.publish": async (params) => ({
      affected_count: 1,
      sample_ids: [params.docId],
      summary: "Publish the selected document",
    }),
  },
  slotBuilders: {
    // declarative slot tables: {const}, {param}, or {preview}
    "doc.publish": { title: { const: "Publish document" } },
  },
  authorize: (principal, actionId) =>
    principal.roles.includes("editor") ? null : "editors only",
  llm: { provider: "anthropic", api_key: process.env.ANTHROPIC_API_KEY },
  // or { provider: "ollama", model: "qwen3" }, an OpenAI-compatible server
  // such as vLLM: { provider: "openai", model, base_url:
  // "http://localhost:8000/v1", api_key? }, a {provider: "scripted"}
  // script for tests, or an async callback for a custom gateway
});

const wss = new WebSocketServer({ port: 8788, path: "/agent" });
wss.on("connection", (ws, request) => {
  const principal = authenticate(request); // your auth, before any frame
  ws.on("message", bindAgentSocket(agent, principal, ws));
});
```

`bindAgentSocket` is the whole transport glue: it opens one session under the
principal, pumps frames, and — when a non-recoverable error ends the session
(spec §6.3: `init_required`, `unsupported_spec_version`) — closes the socket
with code `1008` and the error code as the reason, mirroring the Rust WS
binding. Anything socket-shaped works (`ws`, uWS, Express/Fastify WS
plugins). For finer control use `agent.session(principal)` +
`session.handle(frame, onEvent)` directly; `handle` resolves `true` once the
session is over, and `session.closeCode` / `session.closeReason` tell you how
to close.

Your server answers the WebSocket upgrade, so it also selects the
subprotocol. `@cleverhans/react` offers `cleverhans.v0.1` (exported here as
`SUBPROTOCOL`) first whenever the frontend passes `protocols`, for example
a token. The `ws` library's `WebSocketServer` selects the first offered value
by default, which is that name, so the code above works as is; with your own
`handleProtocols`, return `SUBPROTOCOL` when `protocols.has(SUBPROTOCOL)`,
and read the token from `request.headers["sec-websocket-protocol"]` in
`authenticate`.

Runnable reference: [`typescript/node-demo`](../typescript/node-demo)
(`node src/server.ts`, plus a no-network scripted flow in `src/smoke.ts`).

Generate typed action IDs / params for your frontend from the same registry:

```sh
npx cleverhans-codegen --schema registry.json --ts src/generated/registry.ts
```
