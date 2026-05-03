"""Request/result models and shared data types for the sandbox API.

This module is the contract surface. It must not import from provider,
runtime, OCC, or overlay internals.
"""

from __future__ import annotations

from dataclasses import dataclass


# -- Shared identity --------------------------------------------------------

@dataclass(frozen=True, kw_only=True)
class RequestActor:
    """Caller identity threaded onto every audit-aware request.

    ``agent_id`` is the ledger attribution label and is the only required
    field; the others are populated when the runtime knows them. Keeping
    the optional fields defaulted lets call sites that have only an agent
    name still construct a valid actor.
    """

    agent_id: str
    run_id: str = ""
    agent_run_id: str = ""
    task_id: str = ""


# -- Transport-level primitives --------------------------------------------

@dataclass(frozen=True, kw_only=True)
class RawExecResult:
    """Result of a one-shot ``SandboxTransport.exec`` call."""

    exit_code: int
    stdout: str
    stderr: str = ""


@dataclass(frozen=True, kw_only=True)
class CheckedWriteSpec:
    """One file slot in a transport-level checked apply.

    ``content`` semantics:
      - ``bytes``: write or overwrite ``path`` with this payload.
      - ``None``: delete ``path``. The apply still verifies the
        expected hash before unlinking, so a delete of a file that was
        modified concurrently fails with a ``base_mismatch`` reason.

    ``expected_sha`` semantics:
      - ``str``: the file's prior content hash that the caller observed.
      - ``None``: assert the file does not exist (create-only).
    """

    path: str
    content: bytes | None
    expected_sha: str | None


@dataclass(frozen=True, kw_only=True)
class CheckedWriteResult:
    """Outcome of ``SandboxTransport.apply_diff_batch_checked``."""

    success: bool
    written_paths: tuple[str, ...] = ()
    conflict_paths: tuple[str, ...] = ()
    conflict_reason: str | None = None


# -- SandboxApi: file I/O ---------------------------------------------------

@dataclass(frozen=True, kw_only=True)
class ReadFileRequest:
    path: str
    actor: RequestActor


@dataclass(frozen=True, kw_only=True)
class ReadFileResult:
    content: str
    exists: bool = True
    encoding: str = "utf-8"


@dataclass(frozen=True, kw_only=True)
class WriteFileRequest:
    path: str
    content: str
    actor: RequestActor
    description: str = ""
    overwrite: bool = True


@dataclass(frozen=True, kw_only=True)
class WriteFileResult:
    success: bool
    changed_paths: tuple[str, ...] = ()
    conflict_reason: str | None = None


@dataclass(frozen=True, kw_only=True)
class SearchReplaceEdit:
    """One exact-match replacement applied as part of an ``EditFileRequest``."""

    old_text: str
    new_text: str


@dataclass(frozen=True, kw_only=True)
class EditFileRequest:
    path: str
    edits: tuple[SearchReplaceEdit, ...]
    actor: RequestActor
    description: str = ""


@dataclass(frozen=True, kw_only=True)
class EditFileResult:
    success: bool
    changed_paths: tuple[str, ...] = ()
    applied_edits: int = 0
    conflict_reason: str | None = None


# -- SandboxApi: search -----------------------------------------------------

# -- SandboxApi: shell ------------------------------------------------------

@dataclass(frozen=True, kw_only=True)
class ShellRequest:
    command: str
    actor: RequestActor
    cwd: str | None = None
    timeout: int | None = None
    stdin: str | None = None
    description: str = ""
    attribute_changes: bool = True


@dataclass(frozen=True, kw_only=True)
class ShellResult:
    exit_code: int
    stdout: str
    stderr: str = ""
    success: bool = True
    changed_paths: tuple[str, ...] = ()
    conflict_reason: str | None = None
    warnings: tuple[str, ...] = ()


__all__ = [
    "CheckedWriteResult",
    "CheckedWriteSpec",
    "EditFileRequest",
    "EditFileResult",
    "RawExecResult",
    "ReadFileRequest",
    "ReadFileResult",
    "RequestActor",
    "SearchReplaceEdit",
    "ShellRequest",
    "ShellResult",
    "WriteFileRequest",
    "WriteFileResult",
]
