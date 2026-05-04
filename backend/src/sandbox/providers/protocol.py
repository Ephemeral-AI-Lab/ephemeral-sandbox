"""Provider-neutral sandbox interfaces."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Protocol

if TYPE_CHECKING:
    from sandbox.api import RawExecResult


class ProviderAdapter(Protocol):
    """Minimal provider primitive used by raw runtime/setup paths."""

    name: str

    async def exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult: ...


class SandboxLifecycleProvider(Protocol):
    """Provider-owned sandbox lifecycle surface exposed through sandbox facades."""

    def get_health(self) -> dict[str, Any]: ...
    def list_sandboxes(self) -> list[dict[str, Any]]: ...
    def get_sandbox(self, sandbox_id: str) -> dict[str, Any]: ...
    def get_sandbox_object(self, sandbox_id: str) -> Any: ...
    def get_build_logs_url(self, sandbox_id: str) -> str | None: ...
    def create_sandbox(
        self,
        *,
        name: str,
        snapshot: str | None = None,
        image: str | None = None,
        language: str = "python",
        env_vars: dict[str, str] | None = None,
        labels: dict[str, str] | None = None,
    ) -> dict[str, Any]: ...
    def start_sandbox(self, sandbox_id: str) -> dict[str, Any]: ...
    def stop_sandbox(self, sandbox_id: str) -> dict[str, Any]: ...
    def ensure_sandbox_running(self, sandbox_id: str) -> dict[str, Any]: ...
    def delete_sandbox(self, sandbox_id: str) -> None: ...
    def list_snapshots(self) -> list[dict[str, Any]]: ...
    def get_signed_preview_url(self, sandbox_id: str, port: int) -> dict[str, Any]: ...
    def list_files_recursive(
        self,
        sandbox_id: str,
        root: str = "/workspace",
        max_depth: int = 10,
        max_items: int = 10_000,
    ) -> list[dict[str, Any]]: ...


class SandboxContextPreparer(Protocol):
    """Provider-owned context hook used by agent runtime setup."""

    def prepare_context(self, context: Any) -> None: ...
    async def prepare_context_async(self, context: Any) -> None: ...


__all__ = [
    "ProviderAdapter",
    "SandboxContextPreparer",
    "SandboxLifecycleProvider",
]
