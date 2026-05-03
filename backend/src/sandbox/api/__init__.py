"""Public sandbox API verbs and request/result models."""

from __future__ import annotations

from sandbox.api.models import (
    ConflictInfo,
    EditFileRequest,
    EditFileResult,
    GuardedResultBase,
    RawExecResult,
    ReadFileRequest,
    ReadFileResult,
    RequestActor,
    SandboxResultBase,
    SearchReplaceEdit,
    ShellRequest,
    ShellResult,
    WriteFileRequest,
    WriteFileResult,
)


def __getattr__(name: str) -> object:
    if name == "edit_file":
        from sandbox.api.edit import edit_file

        return edit_file
    if name == "raw_exec":
        from sandbox.api.raw_exec import raw_exec

        return raw_exec
    if name == "read_file":
        from sandbox.api.read import read_file

        return read_file
    if name == "shell":
        from sandbox.api.shell import shell

        return shell
    if name == "write_file":
        from sandbox.api.write import write_file

        return write_file
    raise AttributeError(name)

__all__ = [
    "ConflictInfo",
    "EditFileRequest",
    "EditFileResult",
    "GuardedResultBase",
    "RawExecResult",
    "ReadFileRequest",
    "ReadFileResult",
    "RequestActor",
    "SandboxResultBase",
    "SearchReplaceEdit",
    "ShellRequest",
    "ShellResult",
    "WriteFileRequest",
    "WriteFileResult",
    "edit_file",
    "raw_exec",
    "read_file",
    "shell",
    "write_file",
]
