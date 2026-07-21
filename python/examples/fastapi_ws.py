"""Hosting CleverHans from an existing FastAPI app — the WebSocket recipe.

The envelope is transport-agnostic JSON frames; `Session.handle` is an async
iterator, so the whole binding to an ASGI WebSocket is the loop at the bottom.
The same wire format serves `@cleverhans/react`'s `createWebSocketTransport`
and the Rust `cleverhans-ws` binding.

Run (needs `pip install fastapi "uvicorn[standard]"` in the venv — the
`standard` extra pulls the WebSocket library):

    ANTHROPIC_API_KEY=sk-... uvicorn fastapi_ws:app --port 8789
    OLLAMA_MODEL=qwen3       uvicorn fastapi_ws:app --port 8789

then point a client at ws://127.0.0.1:8789/agent. Without a provider the
scripted model proposes `doc.publish` for every message.
"""

from __future__ import annotations

import os

from fastapi import FastAPI, WebSocket, WebSocketDisconnect

import cleverhans_agent

# Shared with the scripted demo: one registry document, one handler set.
from demo import DOCS, REGISTRY, authorize, publish, publish_dry_run


def pick_llm():
    if key := os.environ.get("ANTHROPIC_API_KEY"):
        return {"provider": "anthropic", "api_key": key}
    if model := os.environ.get("OLLAMA_MODEL"):
        return {"provider": "ollama", "model": model}
    return {
        "provider": "scripted",
        "script": [[{"tool_call": {"name": "doc.publish", "arguments": {}}}]] * 8,
    }


# One agent for the whole app — stateless and shared. Sessions are per
# connection.
agent = cleverhans_agent.Agent(
    registry=REGISTRY,
    handlers={"doc.publish": publish},
    dry_runs={"doc.publish": publish_dry_run},
    slot_builders={
        "doc.publish": lambda params, preview: {
            "title": "Publish document",
            **({"detail": preview["summary"]} if preview and preview.get("summary") else {}),
        }
    },
    authorize=authorize,
    llm=pick_llm(),
)

app = FastAPI()


def authenticate(websocket: WebSocket) -> dict:
    """Your real app maps its session cookie / bearer token here (spec §10)
    and rejects the connection when unauthenticated. The returned blob is
    the principal every handler and authz check runs under."""
    return {"user_id": "demo", "roles": ["editor"]}


@app.get("/")
def index():
    return {"docs": DOCS, "agent": "ws://this-host/agent"}


@app.websocket("/agent")
async def agent_ws(websocket: WebSocket) -> None:
    principal = authenticate(websocket)
    await websocket.accept()
    session = agent.session(principal)
    try:
        while True:
            frame = await websocket.receive_text()
            async for event in session.handle(frame):
                await websocket.send_json(event)
            # Init-first violation (spec §6.1): mirror the Rust WS binding
            # and close instead of leaving a zombie stream.
            if session.closed:
                await websocket.close()
                return
    except WebSocketDisconnect:
        pass
