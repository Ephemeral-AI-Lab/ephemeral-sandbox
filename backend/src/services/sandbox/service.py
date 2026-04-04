"""Sandbox service — Daytona sandbox lifecycle management.

Wraps the Daytona SDK to provide create/start/stop/delete/list operations
with error handling, git bootstrapping, and optional CI warmup hooks.
"""

from __future__ import annotations

import logging
import os
import threading
from typing import Any, Callable, Awaitable

from ephemeralos.services.sandbox.types import (
    CreateSandboxRequest,
    SandboxHealthResponse,
    SandboxInfo,
    SandboxState,
    SnapshotInfo,
)

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Git bootstrap script — installs git if missing
# ---------------------------------------------------------------------------

_GIT_BOOTSTRAP = r"""
set -e
if command -v git >/dev/null 2>&1; then exit 0; fi
echo "[sandbox] Installing git..."
if command -v apt-get >/dev/null 2>&1; then
    apt-get update -qq && apt-get install -y -qq git
elif command -v apk >/dev/null 2>&1; then
    apk add --no-cache git
elif command -v microdnf >/dev/null 2>&1; then
    microdnf install -y git
elif command -v dnf >/dev/null 2>&1; then
    dnf install -y git
elif command -v yum >/dev/null 2>&1; then
    yum install -y git
else
    echo "[sandbox] No package manager found — git not installed" >&2
    exit 1
fi
echo "[sandbox] git installed"
"""

# ---------------------------------------------------------------------------
# Labels
# ---------------------------------------------------------------------------

_LABEL_MANAGED_BY = "managed_by"
_LABEL_MANAGED_BY_VALUE = "ephemeralos"
_LABEL_CREATED_VIA = "created_via"
_LABEL_IMAGE = "ephemeralos_image"
_LABEL_SNAPSHOT = "ephemeralos_snapshot"
_LABEL_PROJECT_DIR = "project_dir"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

_client_lock = threading.Lock()
_cached_client: Any | None = None
_cached_client_key: tuple[str, str, str] | None = None


def _require_settings() -> tuple[str, str, str]:
    """Return (api_key, api_url, target) from settings or env."""
    try:
        from ephemeralos.config import load_settings
        settings = load_settings()
        api_key = (settings.daytona_api_key or "").strip()
        api_url = (settings.daytona_api_url or "").strip()
        target = (settings.daytona_target or "").strip()
    except Exception:
        api_key = api_url = target = ""

    if not api_key:
        api_key = os.environ.get("DAYTONA_API_KEY", "").strip()
    if not api_url:
        api_url = os.environ.get("DAYTONA_API_URL", "").strip()
    if not target:
        target = os.environ.get("DAYTONA_TARGET", "").strip()

    return api_key, api_url, target


def _get_daytona_client() -> Any:
    """Return a cached Daytona client, creating one if config changed."""
    global _cached_client, _cached_client_key

    api_key, api_url, target = _require_settings()
    if not api_key or not api_url:
        raise RuntimeError(
            "Daytona is not configured. Set DAYTONA_API_KEY and DAYTONA_API_URL."
        )
    current_key = (api_key, api_url, target)

    with _client_lock:
        if _cached_client is not None and _cached_client_key == current_key:
            return _cached_client

        try:
            from daytona_sdk import Daytona, DaytonaConfig
        except ImportError as exc:
            raise RuntimeError(
                "Daytona SDK not installed. Run: pip install daytona-sdk"
            ) from exc

        cfg_kwargs: dict[str, str] = {"api_key": api_key, "api_url": api_url}
        if target:
            cfg_kwargs["target"] = target
        cfg = DaytonaConfig(**cfg_kwargs)
        _cached_client = Daytona(cfg)
        _cached_client_key = current_key
        logger.info("Daytona client created (api_url=%s)", api_url)
        return _cached_client


# ---------------------------------------------------------------------------
# SandboxService
# ---------------------------------------------------------------------------

# Type for optional CI warmup callback
OnSandboxReady = Callable[[str, Any], Awaitable[None]]  # (sandbox_id, sandbox_obj)


class SandboxService:
    """Manages Daytona sandbox lifecycle.

    Thread-safe. The Daytona client is cached and reused across calls.
    An optional ``on_sandbox_ready`` async callback can be registered
    to warm up external services (e.g. code intelligence) when a sandbox
    becomes available.
    """

    def __init__(self, on_sandbox_ready: OnSandboxReady | None = None) -> None:
        self._on_sandbox_ready = on_sandbox_ready

    # -- Health ---------------------------------------------------------------

    async def get_health(self) -> SandboxHealthResponse:
        """Check Daytona availability and configuration."""
        api_key, api_url, target = _require_settings()
        if not api_key or not api_url:
            return SandboxHealthResponse(
                available=False,
                error="Daytona is not configured",
            )
        try:
            client = _get_daytona_client()
            sandboxes = client.list() or []
            return SandboxHealthResponse(
                available=True,
                api_url=api_url,
                target=target,
                sandbox_count=len(sandboxes),
            )
        except Exception as exc:
            return SandboxHealthResponse(
                available=False,
                api_url=api_url,
                target=target,
                error=str(exc),
            )

    # -- List -----------------------------------------------------------------

    async def list_sandboxes(self) -> list[SandboxInfo]:
        """List all sandboxes managed by EphemeralOS."""
        client = _get_daytona_client()
        raw = client.list() or []
        results: list[SandboxInfo] = []
        for sb in raw:
            info = SandboxInfo.from_sdk(sb)
            if info.labels.get(_LABEL_MANAGED_BY) == _LABEL_MANAGED_BY_VALUE:
                results.append(info)
        return results

    async def get_sandbox(self, sandbox_id: str) -> SandboxInfo:
        """Get a single sandbox by ID."""
        client = _get_daytona_client()
        sb = client.get(sandbox_id)
        if sb is None:
            raise ValueError(f"Sandbox '{sandbox_id}' not found")
        return SandboxInfo.from_sdk(sb)

    async def get_sandbox_object(self, sandbox_id: str) -> Any:
        """Return the raw Daytona SDK sandbox object."""
        client = _get_daytona_client()
        sb = client.get(sandbox_id)
        if sb is None:
            raise ValueError(f"Sandbox '{sandbox_id}' not found")
        return sb

    # -- Lifecycle ------------------------------------------------------------

    async def create_sandbox(self, request: CreateSandboxRequest) -> SandboxInfo:
        """Create a new sandbox."""
        client = _get_daytona_client()

        labels = {
            _LABEL_MANAGED_BY: _LABEL_MANAGED_BY_VALUE,
            _LABEL_CREATED_VIA: "api",
            **request.labels,
        }
        if request.image:
            labels[_LABEL_IMAGE] = request.image
        if request.snapshot:
            labels[_LABEL_SNAPSHOT] = request.snapshot
        if request.project_dir:
            labels[_LABEL_PROJECT_DIR] = request.project_dir

        create_kwargs: dict[str, Any] = {"labels": labels}
        if request.image:
            create_kwargs["image"] = request.image
        if request.snapshot:
            create_kwargs["snapshot"] = request.snapshot
        if request.env_vars:
            create_kwargs["env_vars"] = request.env_vars

        try:
            sb = client.create(**create_kwargs)
        except Exception as exc:
            raise RuntimeError(f"Failed to create sandbox: {exc}") from exc

        # Git bootstrap
        await self._ensure_git(sb)

        # Optional CI warmup
        if self._on_sandbox_ready:
            try:
                await self._on_sandbox_ready(sb.id, sb)
            except Exception:
                logger.warning("CI warmup failed for sandbox %s", sb.id, exc_info=True)

        return SandboxInfo.from_sdk(sb)

    async def start_sandbox(self, sandbox_id: str) -> SandboxInfo:
        """Start a stopped sandbox."""
        client = _get_daytona_client()
        sb = client.get(sandbox_id)
        if sb is None:
            raise ValueError(f"Sandbox '{sandbox_id}' not found")

        state = getattr(sb, "state", "")
        if isinstance(state, str) and state.lower() == "started":
            return SandboxInfo.from_sdk(sb)

        sb.start(timeout=180)

        if self._on_sandbox_ready:
            try:
                await self._on_sandbox_ready(sandbox_id, sb)
            except Exception:
                logger.warning("CI warmup failed for sandbox %s", sandbox_id, exc_info=True)

        return SandboxInfo.from_sdk(sb)

    async def stop_sandbox(self, sandbox_id: str) -> SandboxInfo:
        """Stop a running sandbox."""
        client = _get_daytona_client()
        sb = client.get(sandbox_id)
        if sb is None:
            raise ValueError(f"Sandbox '{sandbox_id}' not found")
        sb.stop(timeout=60)
        return SandboxInfo.from_sdk(sb)

    async def delete_sandbox(self, sandbox_id: str) -> None:
        """Delete a sandbox."""
        client = _get_daytona_client()
        sb = client.get(sandbox_id)
        if sb is None:
            raise ValueError(f"Sandbox '{sandbox_id}' not found")
        sb.delete(timeout=60)
        logger.info("Sandbox deleted: %s", sandbox_id)

    async def ensure_sandbox_exists(self, sandbox_id: str) -> SandboxInfo:
        """Verify a sandbox exists and is accessible."""
        return await self.get_sandbox(sandbox_id)

    # -- File operations ------------------------------------------------------

    async def list_files_recursive(
        self,
        sandbox_id: str,
        path: str = "/workspace",
        max_depth: int = 10,
        max_items: int = 10_000,
    ) -> list[str]:
        """List files recursively in a sandbox."""
        sb = await self.get_sandbox_object(sandbox_id)
        try:
            cmd = (
                f"find {path} -maxdepth {max_depth} "
                f"\\( -name .git -o -name node_modules -o -name __pycache__ "
                f"-o -name .venv -o -name venv \\) -prune -o -print "
                f"| head -n {max_items}"
            )
            response = sb.process.exec(cmd, cwd=path, timeout=30)
            result = response.result or ""
            return [line for line in result.strip().splitlines() if line]
        except Exception as exc:
            logger.warning("list_files_recursive failed: %s", exc)
            return []

    # -- Preview URLs ---------------------------------------------------------

    async def get_preview_url(self, sandbox_id: str, port: int) -> str:
        """Get a signed preview URL for a sandbox port."""
        sb = await self.get_sandbox_object(sandbox_id)
        try:
            return sb.get_preview_url(port)
        except Exception as exc:
            raise RuntimeError(f"Failed to get preview URL: {exc}") from exc

    # -- Snapshots ------------------------------------------------------------

    async def list_snapshots(self) -> list[SnapshotInfo]:
        """List available Daytona snapshots."""
        client = _get_daytona_client()
        try:
            raw = client.list_snapshots() if hasattr(client, "list_snapshots") else []
            return [
                SnapshotInfo(
                    id=getattr(s, "id", ""),
                    name=getattr(s, "name", ""),
                    created_at=str(getattr(s, "created_at", "")),
                    size=str(getattr(s, "size", "")),
                )
                for s in (raw or [])
            ]
        except Exception:
            return []

    # -- Workspace root -------------------------------------------------------

    async def resolve_workspace_root(self, sandbox_id: str) -> str:
        """Resolve the workspace root directory for a sandbox."""
        info = await self.get_sandbox(sandbox_id)
        if info.project_dir:
            return info.project_dir
        # Try to detect from sandbox
        sb = await self.get_sandbox_object(sandbox_id)
        project_dir = getattr(sb, "project_dir", None)
        if project_dir:
            return project_dir
        return "/workspace"

    # -- Internal helpers -----------------------------------------------------

    async def _ensure_git(self, sandbox: Any) -> None:
        """Install git in the sandbox if missing."""
        try:
            response = sandbox.process.exec(
                "command -v git >/dev/null 2>&1 && echo ok || echo missing",
                timeout=10,
            )
            if "ok" in (response.result or ""):
                return
            sandbox.process.exec(_GIT_BOOTSTRAP, timeout=120)
        except Exception:
            logger.warning("Git bootstrap failed for sandbox %s", getattr(sandbox, "id", "?"))
