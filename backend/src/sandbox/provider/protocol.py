"""Provider-neutral sandbox interfaces.

After the provider-agnostic lifecycle refactor, ``ProviderAdapter`` is the
single Protocol every provider implements. It owns connection + ``exec`` +
the full primitive surface (container CRUD, snapshots, preview URLs, build
logs).

Orchestration (setup, ensure_git, ensure_running, workspace discovery, context
preparation) is built on top of these primitives in
:mod:`sandbox.host` — never inside the provider package.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Protocol

if TYPE_CHECKING:
    from sandbox._shared.models import RawExecResult


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
    def set_labels(self, sandbox_id: str, labels: dict[str, str]) -> dict[str, Any]: ...

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
    ) -> RawExecResult: ...

    # -- Context preparation -------------------------------------------------

    def context_preparer(self, sandbox_id: str) -> Any:
        """Return a provider-specific context-preparer object for *sandbox_id*.

        The returned object implements ``prepare_context(context)`` and
        ``prepare_context_async(context)`` so sandbox tools can inject
        provider-aware runtime metadata without importing provider modules.
        Promoted from the duck-typed ``getattr(adapter, "context_preparer", ...)``
        call at ``sandbox/api/provider_control.py`` so static analysis has a
        fixed point for every concrete adapter.
        """
        ...


__all__ = [
    "ProviderAdapter",
]
