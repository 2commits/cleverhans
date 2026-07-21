# CleverHans — Node host

Scaffolded starter: `registry.json` (the closed action contract),
`agent.mjs` (agent + WebSocket mount), `eval-cases.json`.

```sh
npm install @cleverhans/node ws
ANTHROPIC_API_KEY=sk-... node cleverhans/agent.mjs   # or OLLAMA_MODEL=qwen3
```

Next steps:

1. Replace the in-memory store in `agent.mjs` with your real execution path,
   and `authenticate()` with your session/token auth.
2. Wire your frontend at `ws://<host>:8788/agent` —
   `@cleverhans/react`'s `createWebSocketTransport` speaks this wire as-is.
3. Add actions: edit `registry.json`, add handler + dry-run entries, then
   regenerate types: `npx cleverhans-codegen --schema cleverhans/registry.json --ts src/generated/registry.ts`.
4. Keep `eval-cases.json` growing with every action (happy path, no
   selection, near-boundary decline).

Guides: https://github.com/2commits/cleverhans/tree/main/docs
