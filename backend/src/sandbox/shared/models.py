"""Shared request/result models for sandbox APIs and provider seams.

This module is type-only domain structure. It must not import from provider,
runtime, OCC, or overlay internals.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field, fields
from enum import Enum
from typing import Literal, TypeAlias


class Intent(str, Enum):
    """High-level execution intent for a foreground sandbox tool call."""

    READ_ONLY = "read_only"
    WRITE_ALLOWED = "write_allowed"
    LIFECYCLE = "lifecycle"


@dataclass(frozen=True, kw_only=True)
class SandboxCaller:
    """Caller identity threaded onto every audit-aware request."""

    agent_id: str
    run_id: str = ""
    agent_run_id: str = ""
    task_id: str = ""
    task_center_run_id: str = ""
    task_center_task_id: str = ""
    task_center_attempt_id: str = ""
    task_center_workflow_id: str = ""
    task_center_request_id: str = ""
    tool_name: str = ""
    tool_id: str = ""

    def audit_fields(self) -> dict[str, str]:
        """Return daemon-facing audit fields, preserving required empty IDs."""
        required = {"agent_id", "run_id", "agent_run_id", "task_id"}
        envelope: dict[str, str] = {}
        for field_info in fields(self):
            key = field_info.name
            value = getattr(self, key)
            if key in required or value:
                envelope[key] = str(value)
        return envelope


@dataclass(frozen=True, kw_only=True)
class SandboxRequestBase:
    """Base request shape for audit-aware public sandbox operations."""

    caller: SandboxCaller
    description: str = ""
    invocation_id: str = ""

    def default_description(self, fallback: str) -> str:
        return self.description or fallback


@dataclass(frozen=True, kw_only=True)
class SandboxResultBase:
    """Base result shape for public sandbox operations."""

    success: bool = True
    workspace: Literal["ephemeral", "isolated"] = "ephemeral"
    timings: dict[str, float] = field(default_factory=dict)
    conflict: "ConflictInfo | None" = None
    conflict_reason: str | None = None
    changed_paths: list[str] | tuple[str, ...] = field(default_factory=list)
    error: dict[str, object] | None = None


ToolCallResult: TypeAlias = dict[str, object]


@dataclass(frozen=True, kw_only=True)
class ToolCallRequest:
    """One tool invocation routed through a workspace pipeline."""

    invocation_id: str
    agent_id: str
    verb: str
    intent: Intent
    args: Mapping[str, object]
    background: bool = False

    def to_payload(self) -> dict[str, object]:
        return {
            "invocation_id": self.invocation_id,
            "agent_id": self.agent_id,
            "verb": self.verb,
            "intent": self.intent.value,
            "args": dict(self.args),
            "background": self.background,
        }

    @classmethod
    def from_payload(cls, payload: Mapping[str, object]) -> "ToolCallRequest":
        args_raw = payload.get("args") or {}
        if not isinstance(args_raw, Mapping):
            raise ValueError("tool-call payload args must be an object")
        return cls(
            invocation_id=str(payload.get("invocation_id") or ""),
            agent_id=str(payload.get("agent_id") or ""),
            verb=str(payload.get("verb") or ""),
            intent=Intent(str(payload.get("intent") or Intent.READ_ONLY.value)),
            args={str(key): value for key, value in args_raw.items()},
            background=bool(payload.get("background", False)),
        )


@dataclass(frozen=True, kw_only=True)
class ConflictInfo:
    """Structured guarded-operation conflict details."""

    reason: str
    conflict_file: str | None = None
    message: str = ""

    @classmethod
    def rejected(cls, *, reason: str = "rejected", message: str = "") -> ConflictInfo:
        return cls(reason=reason, message=message)

    @classmethod
    def overlap(cls, *, path: str, message: str) -> ConflictInfo:
        return cls(
            reason="aborted_overlap",
            conflict_file=path,
            message=message,
        )


@dataclass(frozen=True, kw_only=True)
class GuardedResultBase(SandboxResultBase):
    """Base result for OCC/overlay-guarded operations."""

    changed_paths: tuple[str, ...] = ()
    changed_path_kinds: dict[str, str] = field(default_factory=dict)
    mutation_source: str = ""
    status: str = ""
    conflict: ConflictInfo | None = None
    conflict_reason: str | None = None


@dataclass(frozen=True, kw_only=True)
class RawExecResult(SandboxResultBase):
    """Result of a one-shot raw provider exec call."""

    exit_code: int
    stdout: str
    stderr: str = ""


@dataclass(frozen=True, kw_only=True)
class ReadFileRequest(SandboxRequestBase):
    path: str


@dataclass(frozen=True, kw_only=True)
class ReadFileResult(SandboxResultBase):
    content: str
    exists: bool = True
    encoding: str = "utf-8"


@dataclass(frozen=True, kw_only=True)
class WriteFileRequest(SandboxRequestBase):
    path: str
    content: str
    overwrite: bool = True


@dataclass(frozen=True, kw_only=True)
class WriteFileResult(GuardedResultBase):
    pass


@dataclass(frozen=True, kw_only=True)
class SearchReplaceEdit:
    """One exact-match replacement applied as part of an ``EditFileRequest``."""

    old_text: str
    new_text: str
    replace_all: bool = False


@dataclass(frozen=True, kw_only=True)
class EditFileRequest(SandboxRequestBase):
    path: str
    edits: tuple[SearchReplaceEdit, ...]


@dataclass(frozen=True, kw_only=True)
class EditFileResult(GuardedResultBase):
    applied_edits: int = 0


@dataclass(frozen=True, kw_only=True)
class ShellRequest(SandboxRequestBase):
    command: str
    cwd: str | None = None
    timeout: int | None = None
    stdin: str | None = None
    # Metadata only: the engine owns background lifecycle and still dispatches
    # one api.v1.shell request for both foreground and background calls.
    background: bool = False


@dataclass(frozen=True, kw_only=True)
class ShellResult(GuardedResultBase):
    exit_code: int
    stdout: str
    stderr: str = ""
    warnings: tuple[str, ...] = ()


@dataclass(frozen=True, kw_only=True)
class GlobRequest(SandboxRequestBase):
    pattern: str
    path: str | None = None


@dataclass(frozen=True, kw_only=True)
class GlobResult(SandboxResultBase):
    filenames: tuple[str, ...] = ()
    num_files: int = 0
    truncated: bool = False


@dataclass(frozen=True, kw_only=True)
class GrepRequest(SandboxRequestBase):
    pattern: str
    path: str | None = None
    glob_filter: str | None = None
    output_mode: str = "files_with_matches"
    head_limit: int | None = None
    offset: int = 0
    case_insensitive: bool = False
    line_numbers: bool = False
    multiline: bool = False


@dataclass(frozen=True, kw_only=True)
class GrepResult(SandboxResultBase):
    output_mode: str = "files_with_matches"
    filenames: tuple[str, ...] = ()
    content: str = ""
    num_files: int = 0
    num_lines: int = 0
    num_matches: int = 0
    applied_limit: int | None = None
    applied_offset: int = 0
    truncated: bool = False


@dataclass(frozen=True, kw_only=True)
class LifecycleError:
    """Categorical isolated-workspace lifecycle error."""

    kind: str
    message: str = ""
    details: dict[str, str] = field(default_factory=dict)


@dataclass(frozen=True, kw_only=True)
class LifecycleResultBase:
    """Base result for lifecycle operations, separate from OCC conflicts."""

    success: bool = True
    timings: dict[str, float] = field(default_factory=dict)
    error: LifecycleError | None = None


@dataclass(frozen=True, kw_only=True)
class EnterIsolatedWorkspaceRequest(SandboxRequestBase):
    layer_stack_root: str


@dataclass(frozen=True, kw_only=True)
class EnterIsolatedWorkspaceResult(LifecycleResultBase):
    manifest_version: str = ""
    manifest_root_hash: str = ""


@dataclass(frozen=True, kw_only=True)
class ExitIsolatedWorkspaceRequest(SandboxRequestBase):
    grace_s: float = 5.0


@dataclass(frozen=True, kw_only=True)
class ExitIsolatedWorkspaceResult(LifecycleResultBase):
    evicted_upperdir_bytes: int = 0
    lifetime_s: float = 0.0
    phases_ms: dict[str, float] = field(default_factory=dict)


__all__ = [
    "ConflictInfo",
    "EditFileRequest",
    "EditFileResult",
    "EnterIsolatedWorkspaceRequest",
    "EnterIsolatedWorkspaceResult",
    "ExitIsolatedWorkspaceRequest",
    "ExitIsolatedWorkspaceResult",
    "GlobRequest",
    "GlobResult",
    "GrepRequest",
    "GrepResult",
    "GuardedResultBase",
    "Intent",
    "LifecycleError",
    "LifecycleResultBase",
    "RawExecResult",
    "ReadFileRequest",
    "ReadFileResult",
    "SandboxCaller",
    "SandboxRequestBase",
    "SandboxResultBase",
    "SearchReplaceEdit",
    "ShellRequest",
    "ShellResult",
    "ToolCallRequest",
    "ToolCallResult",
    "WriteFileRequest",
    "WriteFileResult",
]
