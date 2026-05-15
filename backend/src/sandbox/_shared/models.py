"""Shared request/result models for sandbox APIs and provider seams.

This module is type-only domain structure. It must not import from provider,
runtime, OCC, or overlay internals.
"""

from __future__ import annotations

from dataclasses import dataclass, field, fields


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
    task_center_mission_id: str = ""
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

    def default_description(self, fallback: str) -> str:
        return self.description or fallback


@dataclass(frozen=True, kw_only=True)
class SandboxResultBase:
    """Base result shape for public sandbox operations."""

    success: bool = True
    timings: dict[str, float] = field(default_factory=dict)


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


@dataclass(frozen=True, kw_only=True)
class ShellResult(GuardedResultBase):
    exit_code: int
    stdout: str
    stderr: str = ""
    warnings: tuple[str, ...] = ()


__all__ = [
    "ConflictInfo",
    "EditFileRequest",
    "EditFileResult",
    "GuardedResultBase",
    "RawExecResult",
    "ReadFileRequest",
    "ReadFileResult",
    "SandboxCaller",
    "SandboxRequestBase",
    "SandboxResultBase",
    "SearchReplaceEdit",
    "ShellRequest",
    "ShellResult",
    "WriteFileRequest",
    "WriteFileResult",
]
