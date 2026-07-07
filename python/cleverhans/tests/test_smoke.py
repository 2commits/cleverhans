"""Bridge smoke tests beyond the shared vectors: exception mapping, custom
LLM callables, sync callables, concurrent sessions."""

from __future__ import annotations

import asyncio

import cleverhans

REGISTRY = {
    "spec_version": "0.1",
    "blocks": [
        {
            "block_type": "confirm",
            "slots": [{"name": "title", "type": "string", "required": False}],
        }
    ],
    "actions": [
        {
            "id": "record.touch",
            "description": "Touch the selected record",
            "params": [
                {"name": "recordId", "type": "string", "source": "context", "required": True}
            ],
            "block_type": "confirm",
            "mutates": True,
            "authz_key": "record.touch",
        }
    ],
    "context_params": {"recordId": "selected_record_id"},
}

INIT = {
    "type": "init",
    "spec_version": "0.1.0-draft",
    "context": {"route": "/records/r-1", "selected_record_id": "r-1"},
}
ASK = {"type": "user_message", "text": "touch it", "client_msg_id": "c-1"}
TOOL_CALL_SCRIPT = [[{"tool_call": {"name": "record.touch", "arguments": {}}}]]


def make_agent(handler, *, llm=None, authorize=None):
    async def dry_run(params, principal):
        return {"affected_count": 1, "sample_ids": [params["recordId"]]}

    return cleverhans.Agent(
        registry=REGISTRY,
        handlers={"record.touch": handler},
        dry_runs={"record.touch": dry_run},
        authorize=authorize,
        llm=llm or {"provider": "scripted", "script": TOOL_CALL_SCRIPT},
    )


async def propose_and_confirm(agent, principal):
    session = agent.session(principal)
    await session.handle_collect(INIT)
    events = await session.handle_collect(ASK)
    proposals = [event for event in events if event["type"] == "action_proposal"]
    assert proposals, f"no proposal in {events!r}"
    confirm = {"type": "confirm_action", "proposal_id": proposals[0]["proposal_id"]}
    return await session.handle_collect(confirm)


def test_handler_sees_principal_and_result_flows_back():
    async def handler(params, principal):
        return {"touched": params["recordId"], "by": principal["user_id"]}

    events = asyncio.run(propose_and_confirm(make_agent(handler), {"user_id": "alex"}))

    assert events[-1]["state"] == "executed"
    assert events[-1]["result"] == {"touched": "r-1", "by": "alex"}


def test_rejected_exception_maps_to_failed_with_reason():
    async def handler(params, principal):
        raise cleverhans.Rejected("record is locked")

    events = asyncio.run(propose_and_confirm(make_agent(handler), {"user_id": "alex"}))

    assert events[-1]["state"] == "failed"
    assert "record is locked" in events[-1]["reason"]


def test_authorize_deny_string_declines_without_proposal():
    async def handler(params, principal):
        raise AssertionError("must never run")

    async def authorize(principal, action_id, params):
        return "editors only"

    async def main():
        agent = make_agent(handler, authorize=authorize)
        session = agent.session({"user_id": "alex"})
        await session.handle_collect(INIT)
        return await session.handle_collect(ASK)

    events = asyncio.run(main())

    assert all(event["type"] != "action_proposal" for event in events), events
    assert events[-1] == {**events[-1], "type": "chat_message", "done": True}


def test_custom_llm_callable_drives_a_turn():
    async def llm(request):
        assert request["messages"][0]["role"] == "system"
        assert request["tools"][0]["name"] == "record.touch"
        return [{"tool_call": {"name": "record.touch", "arguments": {}}}]

    async def handler(params, principal):
        return {"ok": True}

    events = asyncio.run(propose_and_confirm(make_agent(handler, llm=llm), {"user_id": "a"}))

    assert events[-1]["state"] == "executed"


def test_sync_handler_works_too():
    def handler(params, principal):
        return {"sync": True}

    events = asyncio.run(propose_and_confirm(make_agent(handler), {"user_id": "a"}))

    assert events[-1]["result"] == {"sync": True}


def test_rejected_subclass_still_maps_to_failed():
    class DocumentLocked(cleverhans.Rejected):
        pass

    async def handler(params, principal):
        raise DocumentLocked("locked by another user")

    events = asyncio.run(propose_and_confirm(make_agent(handler), {"user_id": "alex"}))

    assert events[-1]["state"] == "failed"
    assert "locked by another user" in events[-1]["reason"]


def test_unrelated_exception_named_rejected_is_internal():
    class Rejected(Exception):  # a user's own class, NOT cleverhans.Rejected
        pass

    async def handler(params, principal):
        raise Rejected("secret internal detail")

    events = asyncio.run(propose_and_confirm(make_agent(handler), {"user_id": "alex"}))

    assert events[-1]["state"] == "failed"
    assert events[-1]["reason"].startswith("internal"), events[-1]


def test_undrained_concurrent_handles_keep_frame_order():
    async def handler(params, principal):
        return {"ok": True}

    async def main():
        agent = make_agent(handler)
        session = agent.session({"user_id": "a"})
        # Queue init and user_message WITHOUT draining the first turn: the
        # session worker must still process them in call order, so init
        # lands first and the session never sees an init-first violation.
        first = session.handle_collect(INIT)
        second = session.handle_collect(ASK)
        events_first, events_second = await asyncio.gather(first, second)
        return events_first, events_second, session.closed

    events_first, events_second, closed = asyncio.run(main())

    assert not closed, "frame order was violated: session closed by init_required"
    assert events_first == []
    assert any(event["type"] == "action_proposal" for event in events_second), events_second


def test_session_closed_after_init_first_violation():
    async def handler(params, principal):
        return {}

    async def main():
        agent = make_agent(handler)
        session = agent.session({"user_id": "a"})
        events = await session.handle_collect(ASK)  # no init first
        later = await session.handle_collect(INIT)  # post-close: nothing
        return events, later, session.closed

    events, later, closed = asyncio.run(main())

    assert closed
    assert events[-1] == {**events[-1], "code": "init_required", "recoverable": False}
    assert later == []


def test_concurrent_sessions_do_not_interfere():
    async def handler(params, principal):
        await asyncio.sleep(0.01)
        return {"by": principal["user_id"]}

    async def main():
        agent_a = make_agent(handler)
        agent_b = make_agent(handler)
        return await asyncio.gather(
            propose_and_confirm(agent_a, {"user_id": "a"}),
            propose_and_confirm(agent_b, {"user_id": "b"}),
        )

    events_a, events_b = asyncio.run(main())

    assert events_a[-1]["result"] == {"by": "a"}
    assert events_b[-1]["result"] == {"by": "b"}
