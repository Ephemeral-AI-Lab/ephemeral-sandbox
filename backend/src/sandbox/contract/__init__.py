"""Request/result models and shared data types for the sandbox API.

This module is the contract surface. It must not import from provider,
runtime, OCC, or overlay internals.
"""

from __future__ import annotations

from dataclasses import dataclass, field


# -- Shared identity --------------------------------------------------------

@dataclass(frozen=True, kw_only=True)
class SandboxCaller:
    """Caller identity threaded onto every audit-aware request.

    ``agent_id`` is the ledger identity label and is the only required
    field; the others are populated when the runtime knows them. Keeping
    the optional fields defaulted lets call sites that have only an agent
    name still construct a valid caller identity.
    """

    agent_id: str
    run_id: str = ""
    agent_run_id: str = ""
    task_id: str = ""


# -- Result hierarchy -------------------------------------------------------

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


# -- Public file I/O --------------------------------------------------------

@dataclass(frozen=True, kw_only=True)
class ReadFileRequest:
    path: str
    caller: SandboxCaller


@dataclass(frozen=True, kw_only=True)
class ReadFileResult(SandboxResultBase):
    content: str
    exists: bool = True
    encoding: str = "utf-8"


@dataclass(frozen=True, kw_only=True)
class WriteFileRequest:
    path: str
    content: str
    caller: SandboxCaller
    description: str = ""
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
class EditFileRequest:
    path: str
    edits: tuple[SearchReplaceEdit, ...]
    caller: SandboxCaller
    description: str = ""


@dataclass(frozen=True, kw_only=True)
class EditFileResult(GuardedResultBase):
    applied_edits: int = 0


# -- Public search ----------------------------------------------------------

# -- Public shell -----------------------------------------------------------

@dataclass(frozen=True, kw_only=True)
class ShellRequest:
    command: str
    caller: SandboxCaller
    cwd: str | None = None
    timeout: int | None = None
    stdin: str | None = None
    description: str = ""


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
    "SandboxResultBase",
    "SearchReplaceEdit",
    "ShellRequest",
    "ShellResult",
    "WriteFileRequest",
    "WriteFileResult",
]
