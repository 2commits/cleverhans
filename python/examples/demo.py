"""Minimal CleverHans host app in Python — executable documentation.

Run the self-contained scripted flow (no API key, no network):

    python demo.py

Or chat for real by exporting a provider first:

    ANTHROPIC_API_KEY=sk-... python demo.py "publish this document"
    OLLAMA_MODEL=qwen3       python demo.py "publish this document"
"""

from __future__ import annotations

import asyncio
import json
import os
import sys

import cleverhans_agent

# The declarative registry: pure data, the single source for validation,
# codegen, and the model-facing tool list (spec §4).
REGISTRY = {
    "spec_version": "0.1",
    "blocks": [
        {
            "block_type": "confirm",
            "slots": [
                {"name": "title", "type": "string", "required": True},
                {"name": "detail", "type": "string", "required": False},
            ],
        }
    ],
    "actions": [
        {
            "id": "doc.publish",
            "description": "Publish the currently open document so readers can see it.",
            "params": [
                {"name": "docId", "type": "string", "source": "context", "required": True}
            ],
            "block_type": "confirm",
            "mutates": True,
            "authz_key": "doc.publish",
        }
    ],
    "context_params": {"docId": "selected_record_id"},
}

# App state lives in closures — never in the principal.
DOCS = {"doc-7": {"title": "Q3 Roadmap", "published": False}}


async def publish(params, principal):
    doc = DOCS.get(params["docId"])
    if doc is None:
        raise cleverhans_agent.Rejected("document not found")
    doc["published"] = True
    return {"published": params["docId"], "by": principal["user_id"]}


async def publish_dry_run(params, principal):
    doc = DOCS.get(params["docId"])
    if doc is None:
        raise cleverhans_agent.Rejected("document not found")
    return {
        "affected_count": 1,
        "sample_ids": [params["docId"]],
        "summary": f"Publish “{doc['title']}”",
    }


async def authorize(principal, action_id, params):
    return None if "editor" in principal["roles"] else "editors only"


def pick_llm(utterance: str):
    if key := os.environ.get("ANTHROPIC_API_KEY"):
        return {"provider": "anthropic", "api_key": key}
    if model := os.environ.get("OLLAMA_MODEL"):
        return {"provider": "ollama", "model": model}
    print("(no provider configured — using the scripted model)\n")
    return {
        "provider": "scripted",
        "script": [[{"tool_call": {"name": "doc.publish", "arguments": {}}}]],
    }


async def main() -> None:
    utterance = sys.argv[1] if len(sys.argv) > 1 else "publish this document"
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
        llm=pick_llm(utterance),
    )

    # One session per authenticated stream, bound to the app's principal.
    session = agent.session({"user_id": "alex", "roles": ["editor"]})
    await session.handle_collect(
        {
            "type": "init",
            "spec_version": "0.1.0-draft",
            "context": {"route": "/docs/doc-7", "selected_record_id": "doc-7"},
        }
    )

    print(f"user: {utterance}")
    proposal = None
    async for event in session.handle(
        {"type": "user_message", "text": utterance, "client_msg_id": "c-1"}
    ):
        print(f"agent → {json.dumps(event)}")
        if event["type"] == "action_proposal":
            proposal = event

    if proposal is None:
        print("(no proposal — nothing to confirm)")
        return

    # The user is the executor: nothing ran yet (spec §12.3).
    print(f"\nuser confirms {proposal['proposal_id']}")
    for event in await session.handle_collect(
        {"type": "confirm_action", "proposal_id": proposal["proposal_id"]}
    ):
        print(f"agent → {json.dumps(event)}")
    print(f"\nstore says: {DOCS['doc-7']}")


if __name__ == "__main__":
    asyncio.run(main())
