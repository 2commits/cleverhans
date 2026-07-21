# CleverHans — React frontend wiring

`AgentWidget.tsx` is a drop-in client component over the styled chat.

```sh
npm install @cleverhans/react @cleverhans/ui
```

1. Move `AgentWidget.tsx` into your component tree and point the transport
   URL at your backend's `/agent` mount.
2. Call `session.updateContext(...)` wherever navigation/selection changes —
   the agent targets whatever the context says the user is standing on.
3. Generate typed slots/action ids from the registry:
   `npx cleverhans-codegen --schema cleverhans/registry.json --ts src/generated/registry.ts`
   then key custom blocks with `BlockComponents<BlockSlotsMap>`.

Headless instead (your own UI): see
https://github.com/nordalf/cleverhans/blob/main/docs/quickstart-react.md
