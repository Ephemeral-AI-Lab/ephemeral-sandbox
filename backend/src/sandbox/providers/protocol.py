"""Provider-neutral sandbox interfaces.

After the provider-agnostic lifecycle refactor, ``ProviderAdapter`` is the
single Protocol every provider implements. It owns connection + ``exec`` +
the full primitive surface (container CRUD, snapshots, preview URLs, build
logs).

Orchestration (setup, ensure_git, ensure_running, workspace discovery, context
preparation) is built on top of these primitives in
:mod:`sandbox.control.ops` — never inside the provider package.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Protocol

if TYPE_CHECKING:
    from sandbox.api import RawExecResult


class ProviderAdapter(Protocol):
    """Container CRUD + exec primitives implemented by each provider."""

    name: str

    # -- Health / discovery ---------------------------------------------------

    def get_health(self) -> dict[str, Any]: ...
    def list_snapshots(self) -> list[dict[str, Any]]: ...

    # -- Container CRUD -------------------------------------------------------

    def create(
        self,
        *,
        name: str,
        snapshot: str | None = None,
        image: str | None = None,
        language: str = "python",
        env_vars: dict[str, str] | None = None,
        labels: dict[str, str] | None = None,
    ) -> dict[str, Any]: ...
    def get(self, sandbox_id: str) -> dict[str, Any]: ...
    def list(self) -> list[dict[str, Any]]: ...
    def start(self, sandbox_id: str) -> dict[str, Any]: ...
    def stop(self, sandbox_id: str) -> dict[str, Any]: ...
    def delete(self, sandbox_id: str) -> None: ...

    # -- Preview / observability ---------------------------------------------

    def get_signed_preview_url(self, sandbox_id: str, port: int) -> dict[str, Any]: ...
    def get_build_logs_url(self, sandbox_id: str) -> str | None: ...

    # -- Exec ----------------------------------------------------------------

    async def exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> "RawExecResult": ...


class SandboxContextPreparer(Protocol):
    """Provider-owned context hook used by agent runtime setup.

    Will move to :mod:`sandbox.control.ops.context` in a follow-up step. Kept
    here for now so :mod:`sandbox.lifecycle.context` (still alive as a
    transitional shim) keeps importing this from a stable location.
    """

    def prepare_context(self, context: Any) -> None: ...
    async def prepare_context_async(self, context: Any) -> None: ...


__all__ = [
    "ProviderAdapter",
    "SandboxContextPreparer",
]
