"""Sandbox API facade implementation."""

from __future__ import annotations

from typing import TYPE_CHECKING
from typing import Any

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

if TYPE_CHECKING:
    from audit.base import AuditSink


class SandboxClient:
    """Single auditable call surface for sandbox status and tool verbs."""

    def __init__(self, *, audit_sink: AuditSink | None = None) -> None:
        self._audit_sink = audit_sink

    def create_sandbox(
        self,
        *,
        name: str,
        snapshot: str | None = None,
        image: str | None = None,
        language: str = "python",
        env_vars: dict[str, str] | None = None,
        labels: dict[str, str] | None = None,
    ) -> dict[str, Any]:
        from sandbox.api import status

        return status.create_sandbox(
            name=name,
            snapshot=snapshot,
            image=image,
            language=language,
            env_vars=env_vars,
            labels=labels,
        )

    def start_sandbox(self, sandbox_id: str) -> dict[str, Any]:
        from sandbox.api import status

        return status.start_sandbox(sandbox_id)

    def stop_sandbox(self, sandbox_id: str) -> dict[str, Any]:
        from sandbox.api import status

        return status.stop_sandbox(sandbox_id)

    def delete_sandbox(self, sandbox_id: str) -> None:
        from sandbox.api import status

        status.delete_sandbox(sandbox_id)

    def ensure_sandbox_running(self, sandbox_id: str) -> dict[str, Any]:
        from sandbox.api import status

        return status.ensure_sandbox_running(sandbox_id)

    def set_sandbox_labels(
        self,
        sandbox_id: str,
        labels: dict[str, str],
    ) -> dict[str, Any]:
        from sandbox.api import status

        return status.set_sandbox_labels(sandbox_id, labels)

    def get_sandbox(self, sandbox_id: str) -> dict[str, Any]:
        from sandbox.api import status

        return status.get_sandbox(sandbox_id)

    def list_sandboxes(self) -> list[dict[str, Any]]:
        from sandbox.api import status

        return status.list_sandboxes()

    def list_snapshots(self) -> list[dict[str, Any]]:
        from sandbox.api import status

        return status.list_snapshots()

    def get_health(self) -> dict[str, Any]:
        from sandbox.api import status

        return status.get_health()

    def get_signed_preview_url(
        self,
        sandbox_id: str,
        port: int,
    ) -> dict[str, Any]:
        from sandbox.api import status

        return status.get_signed_preview_url(sandbox_id, port)

    def get_build_logs_url(self, sandbox_id: str) -> str | None:
        from sandbox.api import status

        return status.get_build_logs_url(sandbox_id)

    def context_preparer_for(self, sandbox_id: str) -> Any:
        from sandbox.host.context import context_preparer_for

        return context_preparer_for(sandbox_id)

    async def shell(
        self,
        sandbox_id: str,
        request: ShellRequest,
        *,
        audit_sink: AuditSink | None = None,
    ) -> ShellResult:
        from sandbox.api.tool import shell as shell_module

        sink = self._audit_sink if audit_sink is None else audit_sink
        kwargs = {"audit_sink": sink} if sink is not None else {}
        return await shell_module.shell(sandbox_id, request, **kwargs)

    async def raw_exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
        audit_sink: AuditSink | None = None,
    ) -> RawExecResult:
        from sandbox.api.tool import raw_exec as raw_exec_module

        sink = self._audit_sink if audit_sink is None else audit_sink
        kwargs = {"audit_sink": sink} if sink is not None else {}
        return await raw_exec_module.raw_exec(
            sandbox_id,
            command,
            cwd=cwd,
            timeout=timeout,
            **kwargs,
        )

    async def read_file(
        self,
        sandbox_id: str,
        request: ReadFileRequest,
        *,
        audit_sink: AuditSink | None = None,
    ) -> ReadFileResult:
        from sandbox.api.tool import read as read_module

        sink = self._audit_sink if audit_sink is None else audit_sink
        kwargs = {"audit_sink": sink} if sink is not None else {}
        return await read_module.read_file(sandbox_id, request, **kwargs)

    async def write_file(
        self,
        sandbox_id: str,
        request: WriteFileRequest,
        *,
        audit_sink: AuditSink | None = None,
    ) -> WriteFileResult:
        from sandbox.api.tool import write as write_module

        sink = self._audit_sink if audit_sink is None else audit_sink
        kwargs = {"audit_sink": sink} if sink is not None else {}
        return await write_module.write_file(sandbox_id, request, **kwargs)

    async def edit_file(
        self,
        sandbox_id: str,
        request: EditFileRequest,
        *,
        audit_sink: AuditSink | None = None,
    ) -> EditFileResult:
        from sandbox.api.tool import edit as edit_module

        sink = self._audit_sink if audit_sink is None else audit_sink
        kwargs = {"audit_sink": sink} if sink is not None else {}
        return await edit_module.edit_file(sandbox_id, request, **kwargs)


__all__ = ["SandboxClient"]
