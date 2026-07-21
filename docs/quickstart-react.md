# React quickstart

```sh
npm install @cleverhans/react @cleverhans/ui   # ui is optional
```

## Styled path (10 minutes)

```tsx
import { AgentSession, createWebSocketTransport } from "@cleverhans/react";
import { FloatingChat } from "@cleverhans/ui";
import "@cleverhans/ui/styles.css";

const session = new AgentSession(
  createWebSocketTransport("wss://your-app/agent"),
  { context: { route: "/documents", selected_record_id: null, view_type: "list" } },
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

Create the session once (`useMemo` / module scope); the app owns its
lifetime. Auth rides your transport (cookie, WS subprotocol token, ticket
URL) — the envelope never carries credentials.

**Keep context synced with navigation.** The agent targets whatever the
context says the user is standing on, and navigating expires pending
proposals:

```tsx
useEffect(() => {
  session.updateContext({ route: `/documents/${id}`, selected_record_id: id, view_type: "detail" });
}, [session, id]);
```

## Headless path (your own UI)

```tsx
import { AgentProvider, BlockRouter, PendingProposals, useAgentSession } from "@cleverhans/react";
import type { BlockSlotsMap } from "./generated/registry"; // npx cleverhans-codegen

const components: BlockComponents<BlockSlotsMap> = { confirm: MyConfirmCard };

<AgentProvider session={session}>
  <PendingProposals components={components} />
</AgentProvider>;
```

`useAgentSession()` gives `{ snapshot, sendMessage, updateContext, confirm,
reject }`; `confirm(id)` / `reject(id, reason?)` are the only proposal writes
a frontend can perform. Executed results arrive as proposal state changes —
fold `snapshot.proposals` into app state as a pure derivation (see
[`typescript/playground/src/App.tsx`](../typescript/playground/src/App.tsx)).

SSR: both packages ship `"use client"`, and the WebSocket transport is inert
when no `WebSocket` global exists, so server renders are safe. Theme the ui
package via the `--ch-*` custom properties in `styles.css`.
