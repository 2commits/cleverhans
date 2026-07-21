# CleverHans — Python host

Scaffolded starter: `registry.json` (the closed action contract),
`agent.py` (agent + FastAPI WebSocket mount), `eval-cases.json`.

```sh
pip install cleverhans-hitl fastapi "uvicorn[standard]"
ANTHROPIC_API_KEY=sk-... uvicorn cleverhans.agent:app --port 8789   # or OLLAMA_MODEL=qwen3
```

(Adjust the module path to wherever you scaffolded; `agent:app` is a normal
FastAPI app you can mount into your existing one instead.)

Next steps:

1. Replace the in-memory store in `agent.py` with your real execution path,
   and `authenticate()` with your session/token auth.
2. Wire your frontend at `ws://<host>:8789/agent` —
   `@cleverhans/react`'s `createWebSocketTransport` speaks this wire as-is.
3. Add actions: edit `registry.json`, add handler + dry-run entries, then
   regenerate types: `python -c "import cleverhans_agent; print(cleverhans_agent.generate_types('cleverhans/registry.json'))"`.
4. Keep `eval-cases.json` growing with every action (happy path, no
   selection, near-boundary decline).

Guides: https://github.com/2commits/cleverhans/tree/main/docs
