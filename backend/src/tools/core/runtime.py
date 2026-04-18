"""Typed runtime metadata threaded through tool execution.

``ExecutionMetadata`` replaces the ad-hoc ``dict[str, Any]`` that used to
ride along on ``ToolExecutionContext``. It exposes the well-known fields
as typed attributes (so tools can read them with IDE support) while
still behaving like a mutable mapping so the engine can keep using
``metadata.get("key")``, ``metadata["key"] = value``, and
``{**metadata, ...}`` without a big-bang rewrite.

Unknown keys land in ``extras`` so third-party toolkits can still add
their own plumbing without touching this file.
"""

from __future__ import annotations

from collections.abc import Callable, Iterator, Mapping
from dataclasses import dataclass, field, replace
from typing import Any, ClassVar

MERGED_RUNTIME_METADATA_KEYS: tuple[str, ...] = (
    "scope_packet",
    "coherence_token",
    "_loaded_skill_references_by_skill_this_turn",
    "checked_context_freshness",
    "freshness_checked_at",
    "task_summary",
    "task_summary_type",
    "resolved_plan",
    "plan_is_replan",
)


@dataclass
class ExecutionMetadata:
    """Typed bag of runtime metadata passed to tool executions.

    Known fields have typed accessors. Unknown keys (e.g. toolkit-specific
    values) are stored in :attr:`extras` and accessed via the mapping
    interface.
    """

    # Session/agent context plumbed at spawn time.
    session_config: Any | None = None
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

    # Daytona sandbox plumbing, injected by ``DaytonaToolkit.prepare_context``.
    daytona_sandbox: Any | None = None
    # Deprecated compatibility alias for older callers. New code should use
    # ``repo_root`` and ``exec_cwd`` instead.
    daytona_cwd: str | None = None
    ci_service: Any | None = None
    arbiter: Any | None = None

    # Per-call tool id (set by the streaming executor so progress events
    # can be attributed back to their originating tool use).
    tool_id: str | None = None

    # Team-mode plumbing. ``team_run_id`` lets tools that need run-scoped
    # state (e.g. ``share_briefing``) look up their owning ``TeamRun`` via
    # the in-process registry without holding a hard reference.
    team_run_id: str | None = None
    work_item_id: str | None = None

    # Escape hatch for toolkit-specific values the engine does not know
    # about. Prefer adding a typed field above when a value is used by
    # more than one toolkit.
    extras: dict[str, Any] = field(default_factory=dict)

    _TYPED_FIELDS: ClassVar[frozenset[str]] = frozenset(
        {
            "session_config",
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
            "daytona_cwd",
            "ci_service",
            "arbiter",
            "tool_id",
            "team_run_id",
            "work_item_id",
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

    def keys(self) -> Iterator[str]:  # type: ignore[override]
        return iter(self)

    def items(self) -> Iterator[tuple[str, Any]]:  # type: ignore[override]
        for key in self:
            yield key, self[key]

    def values(self) -> Iterator[Any]:  # type: ignore[override]
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


def merge_runtime_metadata(
    *,
    original: ExecutionMetadata | None,
    updated: ExecutionMetadata,
    result_metadata: dict[str, Any] | None = None,
) -> None:
    """Propagate selected tool metadata back to the live metadata bag."""
    if original is None:
        return
    for key, value in updated.extras.items():
        if key.startswith("submitted_") and value is not None:
            original[key] = value
    for key in MERGED_RUNTIME_METADATA_KEYS:
        value = updated.extras.get(key)
        if value is None and isinstance(result_metadata, dict):
            value = result_metadata.get(key)
        if value is not None:
            original[key] = value
