"""Type stub for the PyO3 extension module (`crates/cleverhans-py`).

The public API is the `cleverhans` package wrapper; these types exist so
type-checkers can follow the wrapper's calls into the native layer.
"""

from collections.abc import Callable, Mapping
from typing import Any

class PyEventStream:
    def __aiter__(self) -> PyEventStream: ...
    def __anext__(self) -> Any: ...

class PySession:
    @property
    def closed(self) -> bool: ...
    def handle(self, frame: str) -> PyEventStream: ...

def generate_types(registry_json: str, target: str) -> str: ...

class PyAgent:
    def __init__(
        self,
        registry_json: str,
        handlers: Mapping[str, Callable[..., Any]],
        rejected_class: type[Exception],
        dry_runs: Mapping[str, Callable[..., Any]] | None = None,
        slot_builders: Mapping[str, Callable[..., Any]] | None = None,
        authorize: Callable[..., Any] | None = None,
        llm_spec_json: str | None = None,
        llm_callable: Callable[..., Any] | None = None,
        resolve_context_param: Callable[..., Any] | None = None,
        config_json: str | None = None,
    ) -> None: ...
    def session(self, principal: Mapping[str, Any]) -> PySession: ...
