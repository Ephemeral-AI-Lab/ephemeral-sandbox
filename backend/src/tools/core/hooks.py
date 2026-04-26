"""Tool-specific pre/post hook contracts and validation helpers."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Generic, Literal, Protocol, TypeVar

from pydantic import BaseModel

from tools.core.base import ToolExecutionContextService, ToolResult


HookStatus = Literal["pass", "fail"]
TInput = TypeVar("TInput", bound=BaseModel)
TInput_contra = TypeVar("TInput_contra", bound=BaseModel, contravariant=True)
TValue = TypeVar("TValue")


@dataclass(frozen=True)
class HookResult(Generic[TValue]):
    """Result returned by a tool hook.

    A passing hook may return ``value`` to replace the input/result it was
    given. A failing hook must include ``reason`` so the execution pipeline can
    return a structured hook-failure result to the agent.
    """

    status: HookStatus
    value: TValue | None = None
    reason: str = ""
    message: str = ""
    metadata: dict[str, object] = field(default_factory=dict)

    @classmethod
    def pass_(
        cls,
        value: TValue | None = None,
        *,
        message: str = "",
        metadata: dict[str, object] | None = None,
    ) -> HookResult[TValue]:
        return cls(
            status="pass",
            value=value,
            message=message,
            metadata=dict(metadata or {}),
        )

    @classmethod
    def fail(
        cls,
        reason: str,
        *,
        message: str = "",
        metadata: dict[str, object] | None = None,
    ) -> HookResult[TValue]:
        return cls(
            status="fail",
            reason=reason,
            message=message,
            metadata=dict(metadata or {}),
        )


class ToolPreHook(Protocol[TInput_contra]):
    """Tool-specific hook that may mutate validated tool input."""

    target_tool: str

    async def run(
        self,
        tool_input: TInput_contra,
        context: ToolExecutionContextService,
    ) -> HookResult[Any]:
        """Return pass/fail and optionally a replacement tool input."""


class ToolPostHook(Protocol[TInput_contra]):
    """Tool-specific hook that may mutate a tool result."""

    target_tool: str

    async def run(
        self,
        tool_input: TInput_contra,
        result: ToolResult,
        context: ToolExecutionContextService,
    ) -> HookResult[ToolResult]:
        """Return pass/fail and optionally a replacement tool result."""


def hook_name(hook: object) -> str:
    """Return a stable display name for a hook instance."""

    explicit = getattr(hook, "name", None)
    if isinstance(explicit, str) and explicit.strip():
        return explicit.strip()
    return hook.__class__.__name__


def validate_hook_targets(
    *,
    tool_name: str,
    pre_hooks: tuple[object, ...] = (),
    post_hooks: tuple[object, ...] = (),
) -> None:
    """Reject missing/mismatched hook targets so hooks stay tool-specific."""

    for phase, hooks in (("pre", pre_hooks), ("post", post_hooks)):
        for hook in hooks:
            target = getattr(hook, "target_tool", None)
            if target != tool_name:
                raise ValueError(
                    f"{phase} hook {hook_name(hook)!r} targets {target!r}; "
                    f"expected target_tool={tool_name!r}."
                )
