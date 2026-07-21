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
)

app = FastAPI()

@app.websocket("/agent")
async def agent_ws(websocket: WebSocket) -> None:
    principal = authenticate(websocket)  # your auth, before any frame
    await websocket.accept()
    session = agent.session(principal)
    try:
        while True:
            frame = await websocket.receive_text()
            async for event in session.handle(frame):
                await websocket.send_json(event)
            if session.closed:           # init-first violation: close, don't zombie
                await websocket.close()
                return
    except WebSocketDisconnect:
        pass
```

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
