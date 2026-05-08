"""Public sandbox API package and request/result models."""

from __future__ import annotations

from sandbox.contract import (
    ConflictInfo,
    EditFileRequest,
    EditFileResult,
    GuardedResultBase,
    RawExecResult,
    ReadFileRequest,
    ReadFileResult,
    SandboxCaller,
    SandboxResultBase,
    SearchReplaceEdit,
    ShellRequest,
    ShellResult,
    WriteFileRequest,
    WriteFileResult,
)
from sandbox.api.facade import SandboxClient

_client = SandboxClient()

create_sandbox = _client.create_sandbox
start_sandbox = _client.start_sandbox
stop_sandbox = _client.stop_sandbox
delete_sandbox = _client.delete_sandbox
ensure_sandbox_running = _client.ensure_sandbox_running
set_sandbox_labels = _client.set_sandbox_labels
get_sandbox = _client.get_sandbox
list_sandboxes = _client.list_sandboxes
list_snapshots = _client.list_snapshots
get_health = _client.get_health
get_signed_preview_url = _client.get_signed_preview_url
get_build_logs_url = _client.get_build_logs_url
context_preparer_for = _client.context_preparer_for
shell = _client.shell
raw_exec = _client.raw_exec
read_file = _client.read_file
write_file = _client.write_file
edit_file = _client.edit_file


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
    "SandboxClient",
    "ShellRequest",
    "ShellResult",
    "WriteFileRequest",
    "WriteFileResult",
    "context_preparer_for",
    "create_sandbox",
    "delete_sandbox",
    "edit_file",
    "ensure_sandbox_running",
    "get_build_logs_url",
    "get_health",
    "get_sandbox",
    "get_signed_preview_url",
    "list_sandboxes",
    "list_snapshots",
    "raw_exec",
    "read_file",
    "set_sandbox_labels",
    "shell",
    "start_sandbox",
    "stop_sandbox",
    "write_file",
]
