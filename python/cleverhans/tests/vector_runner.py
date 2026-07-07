"""Python port of the conformance runner (spec/vectors/README.md).

Seam scripts are realized as *Python* callables so the vectors exercise the
PyO3 bridge itself, not just the Rust core underneath it.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import cleverhans

VECTORS = Path(__file__).resolve().parents[3] / "spec" / "vectors"


def load_dir(sub: str) -> list[dict[str, Any]]:
    return [
        json.loads(path.read_text(encoding="utf-8"))
        for path in sorted((VECTORS / sub).glob("*.json"))
    ]


# --- matcher (README "Matching semantics") -------------------------------


def _directive(value: Any) -> tuple[str, Any] | None:
    if isinstance(value, dict) and len(value) == 1:
        key = next(iter(value))
        if key.startswith("$"):
            return key, value[key]
    return None


def substitute(payload: Any, bindings: dict[str, Any]) -> Any:
    directive = _directive(payload)
    if directive and directive[0] == "$ref":
        return bindings[directive[1]]
    if isinstance(payload, dict):
        return {key: substitute(value, bindings) for key, value in payload.items()}
    if isinstance(payload, list):
        return [substitute(value, bindings) for value in payload]
    return payload


def match_value(expected: Any, actual: Any, bindings: dict[str, Any], path: str) -> None:
    directive = _directive(expected)
    if directive:
        key, arg = directive
        if key == "$bind":
            bindings[arg] = actual
            return
        if key == "$ref":
            assert bindings[arg] == actual, f"{path}: expected bound {arg}, got {actual!r}"
            return
        if key == "$exact":
            assert arg == actual, f"{path}: expected exactly {arg!r}, got {actual!r}"
            return
        if key == "$keys":
            assert isinstance(actual, dict), f"{path}: expected object, got {actual!r}"
            assert sorted(arg) == sorted(actual), (
                f"{path}: expected keys {sorted(arg)}, got {sorted(actual)}"
            )
            return
        if key == "$absent":
            assert actual is None, f"{path}: expected absent, got {actual!r}"
            return
        raise AssertionError(f"{path}: unknown directive {key}")
    if isinstance(expected, dict):
        assert isinstance(actual, dict), f"{path}: expected object, got {actual!r}"
        for key, want in expected.items():
            if _directive(want) == ("$absent", True):
                assert actual.get(key) is None, f"{path}.{key}: expected absent"
                continue
            assert key in actual, f"{path}.{key}: missing in {actual!r}"
            match_value(want, actual[key], bindings, f"{path}.{key}")
        return
    if isinstance(expected, list):
        assert isinstance(actual, list) and len(actual) == len(expected), (
            f"{path}: expected {len(expected)} element(s), got {actual!r}"
        )
        for index, (want, got) in enumerate(zip(expected, actual)):
            match_value(want, got, bindings, f"{path}[{index}]")
        return
    assert expected == actual, f"{path}: expected {expected!r}, got {actual!r}"


def match_events(expected: list[Any], actual: list[Any], bindings: dict[str, Any]) -> None:
    assert len(expected) == len(actual), (
        f"expected {len(expected)} event(s), got {len(actual)}: {actual!r}"
    )
    for index, (want, got) in enumerate(zip(expected, actual)):
        match_value(want, got, bindings, f"event[{index}]")


# --- scripted seams as Python callables ----------------------------------


def _behavior_at(script: dict[str, Any], call: int) -> Any:
    if "default" in script:
        return script["default"]
    sequence = script["sequence"]
    return sequence[call] if call < len(sequence) else script["then"]


def build_agent(fixture: dict[str, Any], vector: dict[str, Any]) -> tuple[cleverhans.Agent, list[dict[str, Any]]]:
    executions: list[dict[str, Any]] = []
    handlers: dict[str, Any] = {}
    dry_runs: dict[str, Any] = {}
    slot_builders: dict[str, Any] = {}

    for action in fixture["registry"]["actions"]:
        action_id = action["id"]
        script = fixture["scripts"][action_id]

        def handler(params, principal, *, _id=action_id, _script=script["handler"]):
            executions.append({"action_id": _id, "params": params})
            if "fail" in _script:
                raise cleverhans.Rejected(_script["fail"])
            return _script["return"]

        handlers[action_id] = handler

        if "dry_run" in script and script["dry_run"] is not None:
            calls = {"n": 0}

            def dry_run(params, principal, *, _script=script["dry_run"], _calls=calls):
                # _behavior_at handles both the single and the sequence form.
                behavior = _behavior_at(_script, _calls["n"]) if "sequence" in _script else _script
                _calls["n"] += 1
                if "fail" in behavior:
                    raise cleverhans.Rejected(behavior["fail"])
                return behavior["preview"]

            dry_runs[action_id] = dry_run

        if script.get("slots"):

            def slots(params, preview, *, _scripts=script["slots"]):
                # Mirrors the Rust DeclarativeSlots semantics exactly:
                # `preview` supports only the "summary" field, included by
                # presence (an empty-string summary still fills the slot).
                out = {}
                for name, source in _scripts.items():
                    if "const" in source:
                        out[name] = source["const"]
                    elif "param" in source and source["param"] in params:
                        out[name] = params[source["param"]]
                    elif (
                        source.get("preview") == "summary"
                        and preview
                        and preview.get("summary") is not None
                    ):
                        out[name] = preview["summary"]
                return out

            slot_builders[action_id] = slots

    authz_script = vector.get("authz", {"default": "allow"})
    authz_calls = {"n": 0}

    async def authorize(principal, action_id, params):
        behavior = _behavior_at(authz_script, authz_calls["n"])
        authz_calls["n"] += 1
        if behavior == "allow":
            return None
        return behavior["deny"]

    agent = cleverhans.Agent(
        registry=fixture["registry"],
        handlers=handlers,
        dry_runs=dry_runs,
        slot_builders=slot_builders,
        authorize=authorize,
        llm={"provider": "scripted", "script": vector.get("llm", [])},
    )
    return agent, executions


# --- the runner -----------------------------------------------------------


def normalize(events: list[dict[str, Any]], vector: dict[str, Any]) -> list[dict[str, Any]]:
    out = []
    for event in events:
        if event.get("type") in vector.get("ignore_types", []):
            continue
        if (
            not vector.get("keep_deltas", False)
            and event.get("type") == "chat_message"
            and event.get("done") is False
        ):
            continue
        out.append(event)
    return out


async def run_vector(fixture: dict[str, Any], vector: dict[str, Any]) -> None:
    agent, executions = build_agent(fixture, vector)
    session = agent.session({"vector": vector["name"]})
    bindings: dict[str, Any] = {}

    if vector["layer"] == "agent":
        buffer: list[dict[str, Any]] = []
        for index, step in enumerate(vector["steps"]):
            if "send" in step:
                payload = substitute(step["send"], bindings)
                buffer.extend(await session.handle_collect(payload))
            else:
                actual = normalize(buffer, vector)
                buffer = []
                match_events(step["expect"], actual, bindings)
        assert not normalize(buffer, vector), "trailing events after the last expect"
    else:  # binding layer: raw frames, flat expectation
        actual: list[dict[str, Any]] = []
        for frame in vector["frames"]:
            if session.closed:
                break  # the stream closed; a transport would read no further
            raw = frame["raw"] if "raw" in frame else json.dumps(frame["json"])
            actual.extend(await session.handle_collect(raw))
        expect_close = vector.get("expect_close", False)
        assert session.closed == expect_close, (
            f"expect_close: expected closed = {expect_close}, session closed = {session.closed}"
        )
        match_events(vector["expect"], normalize(actual, vector), bindings)

    if "executions" in vector and vector["executions"] is not None:
        assert executions == vector["executions"], (
            f"executions diverge: expected {vector['executions']!r}, got {executions!r}"
        )
