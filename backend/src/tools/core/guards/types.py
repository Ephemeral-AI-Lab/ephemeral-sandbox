"""Outcome types and guard protocols for the tool-guard pipeline."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Awaitable, Protocol, Union

from pydantic import BaseModel

if TYPE_CHECKING:
    from tools.core.base import ToolExecutionContext, ToolResult


@dataclass(frozen=True)
class Allow:
    """Continue the pipeline with no effect."""


@dataclass(frozen=True)
class Deny:
    """Halt the pre-phase pipeline; produce an error ``ToolResult``.

    Ignored in post-phase (post-phase is advisory-only today).
    """

    message: str
    is_error: bool = True


@dataclass(frozen=True)
class MutateArgs:
    """Replace the parsed tool arguments before ``tool.execute`` is called.

    Subsequent guards in the pre-phase see ``new_args``. Post-phase
    mutations are ignored.
    """

    new_args: BaseModel
    warnings: tuple[str, ...] = field(default_factory=tuple)


@dataclass(frozen=True)
class Advisory:
    """Accumulate warnings without changing the control flow."""

    warnings: tuple[str, ...] = field(default_factory=tuple)
    category: str = ""


GuardOutcome = Union[Allow, Deny, MutateArgs, Advisory]


class PreToolGuard(Protocol):
    """Pre-phase guard callable.

    Guards may be ``async`` or plain callables returning a ``GuardOutcome``.
    The pipeline awaits the result if it is awaitable.
    """

    def __call__(
        self,
        tool_name: str,
        args: BaseModel,
        context: "ToolExecutionContext",
    ) -> GuardOutcome | Awaitable[GuardOutcome]: ...


class PostToolGuard(Protocol):
    """Post-phase guard callable. Advisory-only (today)."""

    def __call__(
        self,
        tool_name: str,
        args: BaseModel,
        context: "ToolExecutionContext",
        result: "ToolResult",
    ) -> GuardOutcome | Awaitable[GuardOutcome]: ...
