"""CleverHans propose-only HITL agent — Python host binding.

The agent never acts on your system: it proposes actions from your closed
registry; your handlers execute only after explicit user confirmation, under
the principal you bound to the session. One validation pipeline (the Rust
core) serves every language binding.

Usage sketch::

    agent = cleverhans.Agent(
        registry="registry.json",             # path, JSON string, or dict
        handlers={"doc.publish": publish},    # async (params, principal)
        dry_runs={"doc.publish": publish_dry_run},
        authorize=authorize,                  # None/True allow, str deny
        llm={"provider": "anthropic", "api_key": ...},
    )
    session = agent.session({"user_id": "u-1", "roles": ["editor"]})
    async for event in session.handle(frame):   # dicts, one per ServerEvent
        await ws.send(json.dumps(event))
"""

from __future__ import annotations

import json
from collections.abc import AsyncIterator, Callable, Mapping
from pathlib import Path
from typing import Any

from cleverhans._native import PyAgent

__all__ = ["Agent", "Rejected", "Session"]


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
        """True after an init-first violation closed the session (spec
        §6.1). Further frames yield no events; close your transport."""
        return bool(self._native.closed)

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
    construction errors raise.
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
