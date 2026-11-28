"""CleverHans propose-only HITL agent — Python host binding.

The agent never acts on your system: it proposes actions from your closed
registry; your handlers execute only after explicit user confirmation, under
the principal you bound to the session. One validation pipeline (the Rust
core) serves every language binding.

Usage sketch::

    agent = cleverhans_agent.Agent(
        registry="registry.json",             # path, JSON string, or dict
        handlers={"doc.publish": publish},    # async (params, principal)
        dry_runs={"doc.publish": publish_dry_run},
        authorize=authorize,                  # None/True allow, str deny
        llm={"provider": "anthropic", "api_key": ...},
    )
    session = agent.session({"user_id": "u-1", "roles": ["editor"]})
    async for event in session.handle(frame):   # dicts, one per ServerEvent
        await ws.send(json.dumps(event))
    if session.closed:                           # a non-recoverable error
        await ws.close(code=session.close_code, reason=session.close_reason)

A WebSocket host selects :data:`SUBPROTOCOL` when the client offers it
(``@cleverhans/react`` offers it first whenever it is given ``protocols``).
"""

from __future__ import annotations

import json
from collections.abc import AsyncIterator, Callable, Mapping
from pathlib import Path
from typing import Any

from cleverhans_agent._native import SUBPROTOCOL, PyAgent
from cleverhans_agent._native import generate_types as _generate_types

__all__ = ["SUBPROTOCOL", "Agent", "Rejected", "Session", "generate_types"]


class Rejected(Exception):
    """Raise from a handler or dry-run to decline as a business rule.

    Maps to ``HandlerError::Rejected``: the proposal reports ``failed`` with
    this message as the reason. Subclasses count (the mapping is
    ``isinstance``-based); any other exception maps to an internal error.
    """


def _registry_json(registry: str | Path | Mapping[str, Any]) -> str:
    if isinstance(registry, Mapping):
        return json.dumps(registry)
    text = str(registry)
    if text.lstrip().startswith("{"):
        return text
    return Path(text).read_text(encoding="utf-8")


def generate_types(
    registry: str | Path | Mapping[str, Any],
    target: str = "python",
) -> str:
    """Generates a typed module from a registry document (path, JSON string,
    or dict) — the registry codegen without a Rust toolchain. ``target`` is
    ``"python"``, ``"typescript"``, or ``"rust"``.
    """
    return _generate_types(_registry_json(registry), target)


class Session:
    """One envelope session, bound to one principal.

    Turns are processed strictly in :meth:`handle` call order (a single
    worker owns the session), so concurrent callers can never reorder
    frames. A turn whose iterator is abandoned still runs to completion —
    its side effects happen, matching stream-transport semantics.
    """

    def __init__(self, native: Any) -> None:
        self._native = native

    @property
    def closed(self) -> bool:
        """True once the session is over: the server sent an ``error`` with
        ``recoverable: false`` (spec §6.3), such as ``init_required`` or
        ``unsupported_spec_version``. Further frames yield no events; close
        your transport with :attr:`close_code` and :attr:`close_reason`."""
        return bool(self._native.closed)

    @property
    def close_code(self) -> int | None:
        """The WebSocket close code once the session is over (``1008``,
        policy violation); ``None`` while it is open."""
        code = self._native.close_code
        return None if code is None else int(code)

    @property
    def close_reason(self) -> str | None:
        """The WebSocket close reason once the session is over: the
        ``code`` of the error that ended it; ``None`` while it is open."""
        reason = self._native.close_reason
        return None if reason is None else str(reason)

    async def handle(self, frame: str | Mapping[str, Any]) -> AsyncIterator[dict[str, Any]]:
        """Handles one client event, yielding server events as dicts.

        Chat deltas stream live; the iterator ends when the turn completes.
        """
        raw = frame if isinstance(frame, str) else json.dumps(frame)
        async for event_json in self._native.handle(raw):
            yield json.loads(event_json)

    async def handle_collect(self, frame: str | Mapping[str, Any]) -> list[dict[str, Any]]:
        """Like :meth:`handle`, collecting the whole turn into a list."""
        return [event async for event in self.handle(frame)]


class Agent:
    """The propose-only agent over your registry and seams.

    Stateless and shared: open one :class:`Session` per authenticated
    stream. Protocol failures (malformed frames, init-first violations)
    surface as ``error`` events on the stream, never as exceptions; only
    construction errors raise. After an error with ``recoverable: false``
    the session is over (:attr:`Session.closed`).
    """

    def __init__(
        self,
        *,
        registry: str | Path | Mapping[str, Any],
        handlers: Mapping[str, Callable[..., Any]],
        llm: Mapping[str, Any] | Callable[..., Any],
        dry_runs: Mapping[str, Callable[..., Any]] | None = None,
        slot_builders: Mapping[str, Callable[..., Any]] | None = None,
        authorize: Callable[..., Any] | None = None,
        resolve_context_param: Callable[..., Any] | None = None,
        config: Mapping[str, Any] | None = None,
    ) -> None:
        llm_spec_json = json.dumps(llm) if isinstance(llm, Mapping) else None
        llm_callable = None if isinstance(llm, Mapping) else llm
        self._native = PyAgent(
            _registry_json(registry),
            dict(handlers),
            Rejected,
            dict(dry_runs) if dry_runs else None,
            dict(slot_builders) if slot_builders else None,
            authorize,
            llm_spec_json,
            llm_callable,
            resolve_context_param,
            json.dumps(config) if config else None,
        )

    def session(self, principal: Mapping[str, Any]) -> Session:
        """Opens a session as the authenticated principal (a JSON-able
        identity blob — live resources belong in handler closures)."""
        return Session(self._native.session(dict(principal)))
