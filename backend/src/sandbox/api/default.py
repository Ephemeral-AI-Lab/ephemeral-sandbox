"""Package-level default sandbox API client wrappers."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from sandbox.api import _control as control_module
from sandbox.api._impl import edit as edit_module
from sandbox.api._impl import raw_exec as raw_exec_module
from sandbox.api._impl import read as read_module
from sandbox.api._impl import shell as shell_module
from sandbox.api._impl import write as write_module
from sandbox.host.context_preparer import (
    context_preparer_for as default_context_preparer_for,
)

if TYPE_CHECKING:
    from audit.base import AuditSink
    from sandbox.models import (
        EditFileRequest,
        EditFileResult,
        RawExecResult,
        ReadFileRequest,
        ReadFileResult,
        ShellRequest,
        ShellResult,
        WriteFileRequest,
        WriteFileResult,
    )


def create_sandbox(
    *,
    name: str,
    snapshot: str | None = None,
    image: str | None = None,
    language: str = "python",
    env_vars: dict[str, str] | None = None,
    labels: dict[str, str] | None = None,
) -> dict[str, Any]:
    return control_module.create_sandbox(
        name=name,
        snapshot=snapshot,
        image=image,
        language=language,
        env_vars=env_vars,
        labels=labels,
    )


def start_sandbox(sandbox_id: str) -> dict[str, Any]:
    return control_module.start_sandbox(sandbox_id)


def stop_sandbox(sandbox_id: str) -> dict[str, Any]:
    return control_module.stop_sandbox(sandbox_id)


def delete_sandbox(sandbox_id: str) -> None:
    control_module.delete_sandbox(sandbox_id)


def ensure_sandbox_running(sandbox_id: str) -> dict[str, Any]:
    return control_module.ensure_sandbox_running(sandbox_id)


def set_sandbox_labels(sandbox_id: str, labels: dict[str, str]) -> dict[str, Any]:
    return control_module.set_sandbox_labels(sandbox_id, labels)


def get_sandbox(sandbox_id: str) -> dict[str, Any]:
    return control_module.get_sandbox(sandbox_id)


def list_sandboxes() -> list[dict[str, Any]]:
    return control_module.list_sandboxes()


def list_snapshots() -> list[dict[str, Any]]:
    return control_module.list_snapshots()


def get_health() -> dict[str, Any]:
    return control_module.get_health()


def get_signed_preview_url(sandbox_id: str, port: int) -> dict[str, Any]:
    return control_module.get_signed_preview_url(sandbox_id, port)


def get_build_logs_url(sandbox_id: str) -> str | None:
    return control_module.get_build_logs_url(sandbox_id)


def context_preparer_for(sandbox_id: str) -> Any:
    return default_context_preparer_for(sandbox_id)


async def shell(
    sandbox_id: str,
    request: ShellRequest,
    *,
    audit_sink: AuditSink | None = None,
) -> ShellResult:
    return await shell_module.shell(sandbox_id, request, audit_sink=audit_sink)


async def raw_exec(
    sandbox_id: str,
    command: str,
    *,
    cwd: str | None = None,
    timeout: int | None = None,
    audit_sink: AuditSink | None = None,
) -> RawExecResult:
    return await raw_exec_module.raw_exec(
        sandbox_id,
        command,
        cwd=cwd,
        timeout=timeout,
        audit_sink=audit_sink,
    )


async def read_file(
    sandbox_id: str,
    request: ReadFileRequest,
    *,
    audit_sink: AuditSink | None = None,
) -> ReadFileResult:
    return await read_module.read_file(sandbox_id, request, audit_sink=audit_sink)


async def write_file(
    sandbox_id: str,
    request: WriteFileRequest,
    *,
    audit_sink: AuditSink | None = None,
) -> WriteFileResult:
    return await write_module.write_file(sandbox_id, request, audit_sink=audit_sink)


async def edit_file(
    sandbox_id: str,
    request: EditFileRequest,
    *,
    audit_sink: AuditSink | None = None,
) -> EditFileResult:
    return await edit_module.edit_file(sandbox_id, request, audit_sink=audit_sink)


__all__ = [
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
