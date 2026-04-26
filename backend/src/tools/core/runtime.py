"""Typed runtime metadata threaded through tool execution.

``ExecutionMetadata`` is the shared mapping object used by runtime plumbing.
``ToolExecutionContextService`` unfolds it for tools by exposing well-known services and
identifiers directly while still behaving like a mutable mapping so the engine
can keep using
``metadata.get("key")``, ``metadata["key"] = value``, and
``{**metadata, ...}`` without a big-bang rewrite.

Unknown keys land in ``extras`` so third-party tools can still add
their own plumbing without touching this file.
"""

from __future__ import annotations

from collections.abc import Callable, Iterator, Mapping
from dataclasses import dataclass, field, replace
from typing import Any, ClassVar

@dataclass
class ExecutionMetadata:
    """Typed bag of runtime metadata passed to tool executions.

    Known fields have typed accessors. Unknown keys (e.g. tool-specific
    values) are stored in :attr:`extras` and accessed via the mapping
    interface.
    """

    # Runtime/agent context plumbed at spawn time.
    runtime_config: Any | None = None
    sandbox_id: str = ""
    agent_run_id: str | None = None
    agent_name: str = ""
    cwd: str = ""
    repo_root: str = ""
    exec_cwd: str = ""

    # Tool registry reference (used by tools that need to introspect the
    # broader tool surface, e.g. skills that can call sibling tools).
    tool_registry: Any | None = None

    # Background task plumbing, injected by the engine for background
    # dispatch. Running tools read ``on_progress_line`` to stream live
    # output back into the manager.
    background_task_manager: Any | None = None
    background_task_id: str | None = None
    on_progress_line: Callable[[str], None] | None = None

    # Daytona sandbox plumbing, injected by Daytona context preparation.
    daytona_sandbox: Any | None = None
    ci_service: Any | None = None

    # Per-call tool id (set by the streaming executor so progress events
    # can be attributed back to their originating tool use).
    tool_id: str | None = None

    # Optional notification service injected by the execution pipeline. Tools
    # and hooks use it through ToolExecutionContextService.notify_system().
    system_notification_service: Any | None = None

    # Escape hatch for tool-specific values the engine does not know
    # about. Prefer adding a typed field above when a value is used by
    # more than one tool.
    extras: dict[str, Any] = field(default_factory=dict)

    _TYPED_FIELDS: ClassVar[frozenset[str]] = frozenset(
        {
            "runtime_config",
            "sandbox_id",
            "agent_run_id",
            "agent_name",
            "cwd",
            "repo_root",
            "exec_cwd",
            "tool_registry",
            "background_task_manager",
            "background_task_id",
            "on_progress_line",
            "daytona_sandbox",
            "ci_service",
            "tool_id",
            "system_notification_service",
        }
    )

    # -- Mapping-style interface ------------------------------------------------

    def get(self, key: str, default: Any = None) -> Any:
        if key in self._TYPED_FIELDS:
            value = getattr(self, key)
            return value if value not in (None, "") else default
        return self.extras.get(key, default)

    def __getitem__(self, key: str) -> Any:
        if key in self._TYPED_FIELDS:
            value = getattr(self, key)
            if value in (None, ""):
                raise KeyError(key)
            return value
        return self.extras[key]

    def __setitem__(self, key: str, value: Any) -> None:
        if key in self._TYPED_FIELDS:
            setattr(self, key, value)
        else:
            self.extras[key] = value

    def __contains__(self, key: object) -> bool:
        if not isinstance(key, str):
            return False
        if key in self._TYPED_FIELDS:
            value = getattr(self, key)
            return value not in (None, "")
        return key in self.extras

    def __iter__(self) -> Iterator[str]:
        for name in self._TYPED_FIELDS:
            value = getattr(self, name)
            if value not in (None, ""):
                yield name
        yield from self.extras

    def keys(self) -> Iterator[str]:
        return iter(self)

    def items(self) -> Iterator[tuple[str, Any]]:
        for key in self:
            yield key, self[key]

    def values(self) -> Iterator[Any]:
        for key in self:
            yield self[key]

    def update(
        self,
        other: Mapping[str, Any] | ExecutionMetadata | None = None,
        /,
        **kwargs: Any,
    ) -> None:
        if other is not None:
            if isinstance(other, ExecutionMetadata):
                for name in self._TYPED_FIELDS:
                    value = getattr(other, name)
                    if value not in (None, ""):
                        setattr(self, name, value)
                self.extras.update(other.extras)
            else:
                for key, value in other.items():
                    self[key] = value
        for key, value in kwargs.items():
            self[key] = value

    def copy(self) -> ExecutionMetadata:
        """Return a shallow copy — safe to mutate without affecting original."""
        return replace(self, extras=dict(self.extras))

    def with_overrides(self, **overrides: Any) -> ExecutionMetadata:
        """Return a copy with the given fields overridden.

        Unknown keys land in ``extras``.
        """
        new = self.copy()
        for key, value in overrides.items():
            new[key] = value
        return new
