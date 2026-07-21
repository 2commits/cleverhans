# @cleverhans/react

Headless React frontend for
[CleverHans](https://github.com/2commits/cleverhans) — session store, hooks,
block router, proposal primitives, and the WebSocket transport. No styles,
no components you didn't write; pair with
[`@cleverhans/ui`](https://www.npmjs.com/package/@cleverhans/ui) for the
styled 10-minute path.

```tsx
import { AgentProvider, AgentSession, createWebSocketTransport } from "@cleverhans/react";

const session = new AgentSession(
  createWebSocketTransport("wss://your-app/agent"),
  { context: { route: "/documents", selected_record_id: null } },
);

<AgentProvider session={session}>
  <YourChatSurface />
</AgentProvider>;
```

- `useAgentSession()` / `useAgentProposal(id)` — the live snapshot and
  per-proposal confirm/reject.
- `<BlockRouter components={...}>` — one component per registered block
  type; key it with your codegen'd `BlockSlotsMap` for typed slots.
- `session.updateContext(...)` — call on navigation; the agent always knows
  where the user is, and stale proposals expire.
- Transport auth is your app's (cookie, subprotocol token, ticket URL); the
  envelope never carries credentials.

SSR-safe: without a `WebSocket` global the transport is inert, and the
build carries `"use client"`. Generate `BlockSlotsMap`/`ActionParamsMap`
from your registry with `npx cleverhans-codegen` (ships with
`@cleverhans/node`).
