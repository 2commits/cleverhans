# Python quickstart (FastAPI)

```sh
pip install cleverhans-hitl fastapi "uvicorn[standard]"
```

Prebuilt abi3 wheels (CPython ≥ 3.11) — no Rust toolchain. Handlers and the
optional LLM callback may be sync or `async def`; sessions bind to your
asyncio loop.

```python
import cleverhans_agent
from fastapi import FastAPI, WebSocket, WebSocketDisconnect

agent = cleverhans_agent.Agent(
    registry="registry.json",            # path, JSON string, or dict — see the
                                         # Rust quickstart for the document format
    handlers={"doc.publish": publish},   # async (params, principal)
    dry_runs={"doc.publish": publish_dry_run},
    slot_builders={"doc.publish": lambda params, preview: {"title": "Publish document"}},
    authorize=authorize,                 # None/True allow, str denies with reason
    llm={"provider": "anthropic", "api_key": os.environ["ANTHROPIC_API_KEY"]},
    # or your own OpenAI-compatible server (vLLM, SGLang, NIM, …):
    # llm={"provider": "openai", "model": "<model>",
    #      "base_url": "http://localhost:8000/v1"},   # + "api_key" if needed
)

app = FastAPI()

@app.websocket("/agent")
async def agent_ws(websocket: WebSocket) -> None:
    principal = authenticate(websocket)  # your auth, before any frame
    offered = websocket.scope.get("subprotocols", [])
    proto = cleverhans_agent.SUBPROTOCOL if cleverhans_agent.SUBPROTOCOL in offered else None
    await websocket.accept(subprotocol=proto)
    session = agent.session(principal)
    try:
        while True:
            frame = await websocket.receive_text()
            async for event in session.handle(frame):
                await websocket.send_json(event)
            if session.closed:           # a non-recoverable error ended the session
                await websocket.close(code=session.close_code, reason=session.close_reason)
                return
    except WebSocketDisconnect:
        pass
```

Two things the host's own WebSocket server owns, because the binding does not
answer the upgrade:

- **Subprotocol.** A client that offers subprotocols fails the handshake unless
  the server selects one. Select `cleverhans_agent.SUBPROTOCOL`
  (`"cleverhans.v0.1"`) when it is offered — `@cleverhans/react` offers it first
  whenever you pass `protocols`, for example to carry a token, which your
  `authenticate` then reads from `websocket.scope["subprotocols"]`.
- **The end of a session.** After an `error` with `recoverable: false`
  (`init_required`, `unsupported_spec_version`) the session is over:
  `session.closed` is true and further frames yield nothing. Close the socket
  with `session.close_code` (`1008`) and `session.close_reason` (the error code)
  so the browser sees why, instead of `1006`.

Raise `cleverhans_agent.Rejected("reason")` from a handler or dry-run to decline as
a business rule (the proposal reports `failed` with your reason); any other
exception maps to an internal error.

Runnable references in
[`python/examples/`](../python/examples): `fastapi_ws.py` (this recipe) and
`demo.py` (a no-network scripted flow — good first run).

Generate typed modules from the same registry without a Rust toolchain:

```python
cleverhans_agent.generate_types("registry.json", "python")   # or "typescript" / "rust"
```
