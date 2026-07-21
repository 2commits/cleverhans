"""CleverHans Python host — scaffolded starter. The agent never acts on your
system: it proposes actions from registry.json; the handlers below run only
after explicit user confirmation."""

from __future__ import annotations

import os
from pathlib import Path

import cleverhans_agent
from fastapi import FastAPI, WebSocket, WebSocketDisconnect

# Replace with your real data layer.
RECORDS = {"r-1": {"title": "First record", "archived": False}}


async def archive(params, principal):
    record = RECORDS.get(params["recordId"])
    if record is None:
        raise cleverhans_agent.Rejected("record not found")
    record["archived"] = True  # your app's normal execution path
    return {"archived": params["recordId"], "by": principal["user_id"]}


async def archive_dry_run(params, principal):
    record = RECORDS.get(params["recordId"])
    if record is None:
        raise cleverhans_agent.Rejected("record not found")
    return {
        "affected_count": 1,
        "sample_ids": [params["recordId"]],
        "summary": f"Archive “{record['title']}”",
    }


async def authorize(principal, action_id, params):
    return None if "editor" in principal["roles"] else "editors only"


def pick_llm():
    if key := os.environ.get("ANTHROPIC_API_KEY"):
        return {"provider": "anthropic", "api_key": key}
    if model := os.environ.get("OLLAMA_MODEL"):
        return {"provider": "ollama", "model": model}
    return {
        "provider": "scripted",
        "script": [[{"tool_call": {"name": "record.archive", "arguments": {}}}]],
    }


agent = cleverhans_agent.Agent(
    registry=Path(__file__).with_name("registry.json"),
    handlers={"record.archive": archive},
    dry_runs={"record.archive": archive_dry_run},
    slot_builders={
        "record.archive": lambda params, preview: {
            "title": "Archive record",
            **({"detail": preview["summary"]} if preview and preview.get("summary") else {}),
        }
    },
    authorize=authorize,
    llm=pick_llm(),
)

app = FastAPI()


def authenticate(websocket: WebSocket) -> dict:
    """Replace with your session-cookie / bearer-token auth (spec §10)."""
    return {"user_id": "demo", "roles": ["editor"]}


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
            if session.closed:  # init-first violation: close, don't zombie
                await websocket.close()
                return
    except WebSocketDisconnect:
        pass
