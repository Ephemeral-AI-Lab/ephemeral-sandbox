"""SandboxService — Daytona sandbox lifecycle orchestration."""

from __future__ import annotations

import concurrent.futures
import logging
from typing import TYPE_CHECKING, Any

from sandbox.client.sync import (
    _APP_CREATED_VIA,
    _APP_MANAGED_BY,
    _IMAGE_LABEL,
    _LIST_PAGE_LIMIT,
    _SANDBOX_TIMEOUT_SECONDS,
    _SNAPSHOT_LABEL,
    _SNAPSHOT_PAGE_LIMIT,
    _daytona_classes,
    _normalize_dict,
    _normalize_optional_text,
    _paginate_all,
    acquire_client,
    fetch_sandbox,
)
from sandbox.client.credentials import load_credentials
from sandbox.lifecycle.proxy import SandboxProxy
from sandbox.lifecycle.workspace import (
    _ci_in_sandbox_enabled,
    _sandbox_project_root,
    bootstrap_in_sandbox_ci_runtime,
    bootstrap_upload_runtime_bundle,
)

if TYPE_CHECKING:
    from sandbox.runtime.service import CodeIntelligenceService

logger = logging.getLogger(__name__)


def _register_daytona_provider_adapter(sandbox_id: str) -> None:
    """Register the provider adapter for a Daytona-backed sandbox."""
    if not sandbox_id:
        return
    try:
        from sandbox.providers.daytona.adapter import DaytonaProviderAdapter
        from sandbox.providers.registry import register_adapter

        register_adapter(sandbox_id, DaytonaProviderAdapter())
    except Exception:
        logger.debug(
            "Provider adapter registration failed for sandbox %s",
            sandbox_id,
            exc_info=True,
        )


def _dispose_provider_adapter(sandbox_id: str) -> None:
    if not sandbox_id:
        return
    try:
        from sandbox.providers.registry import dispose_adapter

        dispose_adapter(sandbox_id)
    except Exception:
        logger.debug(
            "Provider adapter disposal failed for sandbox %s",
            sandbox_id,
            exc_info=True,
        )


def _maybe_run_eager_ci_bootstrap(raw_sandbox: Any, sandbox_id: str) -> None:
    """Best-effort eager-CI bootstrap on ``create``/``start``.

    No-op when the ``EOS_CI_IN_SANDBOX`` flag is unset. Uses the registered
    provider adapter and the workspace from :func:`_sandbox_project_root`.
    Bootstrap failures intentionally propagate so the caller sees the indexer
    error.
    """
    if not _ci_in_sandbox_enabled():
        return
    workspace_root = _sandbox_project_root(raw_sandbox) or ""
    if not workspace_root:
        logger.debug(
            "eager CI bootstrap skipped for sandbox %s — no project_dir on handle",
            sandbox_id,
        )
        return

    from sandbox.client.async_bridge import run_sync

    run_sync(
        bootstrap_in_sandbox_ci_runtime(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root,
        )
    )


_BUNDLE_UPLOAD_THREAD_PREFIX = "eos-ci-upload"
_BUNDLE_UPLOAD_JOIN_TIMEOUT_S = 60.0


def _maybe_start_eager_ci_bundle_upload(
    raw_sandbox: Any, sandbox_id: str
) -> concurrent.futures.Future[None] | None:
    """Kick off the runtime-bundle upload in a background thread.

    Designed to overlap with the ~7 s ``sb.ensure_git()`` step in the create
    pipeline. Returns a future the caller MUST drain via
    :func:`_finish_eager_ci_bundle_upload` before invoking
    :func:`_maybe_run_eager_ci_bootstrap`. Returns ``None`` when the
    background path is not enabled — same gating as the sequential
    bootstrap (flag off, no project_dir).

    Best-effort by design: the matching join helper swallows errors and
    timeouts so the sequential bootstrap can retry from scratch.
    """
    if not _ci_in_sandbox_enabled():
        return None
    workspace_root = _sandbox_project_root(raw_sandbox) or ""
    if not workspace_root or not sandbox_id:
        return None

    from sandbox.client.async_bridge import run_sync

    def _do_upload() -> None:
        run_sync(
            bootstrap_upload_runtime_bundle(
                sandbox_id=sandbox_id,
                workspace_root=workspace_root,
            )
        )

    pool = concurrent.futures.ThreadPoolExecutor(
        max_workers=1, thread_name_prefix=_BUNDLE_UPLOAD_THREAD_PREFIX,
    )
    try:
        future = pool.submit(_do_upload)
    finally:
        # The pool is single-shot; shutdown(wait=False) lets the worker
        # finish without keeping the executor alive.
        pool.shutdown(wait=False)
    return future


def _finish_eager_ci_bundle_upload(
    future: concurrent.futures.Future[None] | None,
    sandbox_id: str,
) -> None:
    """Join the background bundle-upload future. Errors do not propagate.

    A failed background upload is recoverable: the subsequent sequential
    :func:`_maybe_run_eager_ci_bootstrap` call will re-run
    ``ensure_runtime_uploaded`` and either find the bundle in place
    (best case) or retry the upload (worst case). Surfacing background
    failures here would mask that retry path.
    """
    if future is None:
        return
    try:
        future.result(timeout=_BUNDLE_UPLOAD_JOIN_TIMEOUT_S)
        logger.info(
            "eager CI bundle upload (background) joined for sandbox %s",
            sandbox_id,
        )
    except concurrent.futures.TimeoutError:
        logger.warning(
            "eager CI bundle upload (background) did not complete within %.0fs "
            "for sandbox %s; sequential bootstrap will retry",
            _BUNDLE_UPLOAD_JOIN_TIMEOUT_S,
            sandbox_id,
        )
    except Exception:
        logger.warning(
            "eager CI bundle upload (background) failed for sandbox %s; "
            "sequential bootstrap will retry",
            sandbox_id,
            exc_info=True,
        )


class SandboxService:
    """Manages Daytona sandbox lifecycle.

    All public methods are synchronous and return plain dicts matching
    the API response shapes. The router wraps them with asyncio.to_thread
    when needed.
    """

    # -- Health ---------------------------------------------------------------

    def get_health(self) -> dict[str, Any]:
        """Check Daytona availability and configuration."""
        api_key, api_url, target = load_credentials()
        if not api_key or not api_url:
            return {
                "configured": False,
                "available": False,
                "api_url": api_url or None,
                "target": target or None,
                "detail": "Set DAYTONA_API_KEY and DAYTONA_API_URL to connect.",
                "default_image": None,
            }
        try:
            client = acquire_client()
            client.list(limit=1)
            return {
                "configured": True,
                "available": True,
                "api_url": api_url,
                "target": target or None,
                "detail": None,
                "default_image": None,
            }
        except Exception as exc:
            return {
                "configured": True,
                "available": False,
                "api_url": api_url,
                "target": target or None,
                "detail": str(exc),
                "default_image": None,
            }

    # -- List -----------------------------------------------------------------

    def list_sandboxes(self) -> list[dict[str, Any]]:
        """List all sandboxes (both managed and external)."""
        client = acquire_client()
        sandboxes = [
            SandboxProxy(sb).serialize() for sb in _paginate_all(client.list, _LIST_PAGE_LIMIT)
        ]
        sandboxes.sort(key=lambda item: item.get("created_at") or "", reverse=True)
        return sandboxes

    def _get_proxy(self, sandbox_id: str) -> SandboxProxy:
        """Fetch a sandbox by ID and return a typed proxy."""
        raw = fetch_sandbox(sandbox_id)
        return SandboxProxy(raw)

    def get_sandbox(self, sandbox_id: str) -> dict[str, Any]:
        """Get a single sandbox by ID."""
        return self._get_proxy(sandbox_id).serialize()

    def get_sandbox_object(self, sandbox_id: str) -> Any:
        """Return the raw Daytona SDK sandbox object."""
        return self._get_proxy(sandbox_id)._raw

    def get_build_logs_url(self, sandbox_id: str) -> str | None:
        """Return the Daytona build-logs URL for a sandbox when available."""
        raw = self.get_sandbox_object(sandbox_id)
        sandbox_api = getattr(raw, "_sandbox_api", None)
        if sandbox_api is None or not hasattr(sandbox_api, "get_build_logs_url"):
            return None
        try:
            result = sandbox_api.get_build_logs_url(sandbox_id)
        except Exception:
            logger.debug("Failed to fetch build logs URL for sandbox %s", sandbox_id, exc_info=True)
            return None
        url = getattr(result, "url", None)
        return str(url).strip() or None

    # -- Lifecycle ------------------------------------------------------------

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
        """Create a new sandbox."""
        normalized_name = _normalize_optional_text(name)
        normalized_snapshot = _normalize_optional_text(snapshot)
        normalized_image = _normalize_optional_text(image)
        if not normalized_name:
            raise ValueError("Sandbox name is required")
        if normalized_snapshot and normalized_image:
            raise ValueError("Pass either snapshot or image, not both.")

        clean_env = _normalize_dict(env_vars)
        clean_labels = _normalize_dict(labels)
        clean_labels["managed_by"] = _APP_MANAGED_BY
        clean_labels["created_via"] = _APP_CREATED_VIA
        if normalized_snapshot:
            clean_labels[_SNAPSHOT_LABEL] = normalized_snapshot
        if normalized_image:
            clean_labels[_IMAGE_LABEL] = normalized_image

        client = acquire_client()
        _, _, CreateSandboxFromSnapshotParams, CreateSandboxFromImageParams = _daytona_classes()

        if normalized_image:
            params = CreateSandboxFromImageParams(
                name=normalized_name,
                image=normalized_image,
                language=language,
                auto_stop_interval=0,
                env_vars=clean_env or None,
                labels=clean_labels,
                ephemeral=False,
            )
        else:
            params = CreateSandboxFromSnapshotParams(
                name=normalized_name,
                snapshot=normalized_snapshot,
                language=language,
                auto_stop_interval=0,
                env_vars=clean_env or None,
                labels=clean_labels,
                ephemeral=False,
            )

        logger.info("create_sandbox(%s): Daytona create starting", normalized_name)
        raw = client.create(params, timeout=_SANDBOX_TIMEOUT_SECONDS)
        logger.info("create_sandbox(%s): Daytona create returned", normalized_name)
        sb = SandboxProxy(raw)
        _register_daytona_provider_adapter(sb.id)
        logger.info("create_sandbox(%s): refresh starting", normalized_name)
        sb.refresh()
        # Kick off the runtime-bundle upload in a background thread so it
        # overlaps with `ensure_git`. Both depend only on the sandbox
        # existing; sequencing them serially leaves ~7s on the table.
        upload_future = _maybe_start_eager_ci_bundle_upload(sb._raw, sb.id)
        logger.info("create_sandbox(%s): ensure_git starting", normalized_name)
        sb.ensure_git()
        _finish_eager_ci_bundle_upload(upload_future, sb.id)
        logger.info(
            "create_sandbox(%s): eager CI bootstrap check starting for %s",
            normalized_name,
            sb.id,
        )

        _maybe_run_eager_ci_bootstrap(sb._raw, sb.id)
        logger.info("create_sandbox(%s): completed", normalized_name)

        return sb.serialize(assigned_agents=[])

    def start_sandbox(self, sandbox_id: str) -> dict[str, Any]:
        """Start a stopped sandbox."""
        sb = self._get_proxy(sandbox_id)
        if sb.state == "started":
            _register_daytona_provider_adapter(sb.id)
            return sb.serialize()

        sb._raw.start(timeout=_SANDBOX_TIMEOUT_SECONDS)
        sb.refresh()
        _register_daytona_provider_adapter(sb.id)
        # Same overlap as create_sandbox — bundle upload runs while the
        # sync ensure_git probe is in flight.
        upload_future = _maybe_start_eager_ci_bundle_upload(sb._raw, sb.id)
        sb.ensure_git()
        _finish_eager_ci_bundle_upload(upload_future, sb.id)
        sb.refresh()

        _maybe_run_eager_ci_bootstrap(sb._raw, sb.id)

        return sb.serialize()

    def stop_sandbox(self, sandbox_id: str) -> dict[str, Any]:
        """Stop a running sandbox."""
        sb = self._get_proxy(sandbox_id)
        sb._raw.stop(timeout=60)
        sb.refresh()
        return sb.serialize()

    def ensure_sandbox_running(self, sandbox_id: str) -> dict[str, Any]:
        """Best-effort recovery when a sandbox handle exists but execution fails.

        Some long-running benchmark runs observe a sandbox that still resolves by
        id yet whose backing container is gone or detached. In that state fresh
        workers degrade into misleading "no context" errors because sandbox
        preparation fails before tools run. Probe the sandbox directly and, when
        execution is unhealthy, try a targeted restart once.
        """
        sb = self._get_proxy(sandbox_id)
        _register_daytona_provider_adapter(sb.id)
        try:
            from sandbox.api.raw_exec import raw_exec
            from sandbox.client.async_bridge import run_sync

            resp = run_sync(raw_exec(sb.id, "pwd", timeout=10))
            exit_code = getattr(resp, "exit_code", 0)
            if exit_code in (None, 0):
                return sb.serialize()
        except Exception:
            logger.warning(
                "Sandbox %s probe failed; attempting restart recovery",
                sandbox_id,
                exc_info=True,
            )

        try:
            sb._raw.start(timeout=_SANDBOX_TIMEOUT_SECONDS)
        except Exception:
            logger.debug(
                "Sandbox %s start during recovery raised; continuing with refresh",
                sandbox_id,
                exc_info=True,
            )

        sb.refresh()
        _register_daytona_provider_adapter(sb.id)
        sb.ensure_git()
        sb.refresh()

        # Restart-recovery path: re-bootstrap CI so the in-sandbox index
        # tracks the post-restart workspace. The hook is a no-op when
        # ``EOS_CI_IN_SANDBOX`` is unset, so the cost only lands on
        # callers that actually opted into the daemon migration.
        _maybe_run_eager_ci_bootstrap(sb._raw, sb.id)

        return sb.serialize()

    def delete_sandbox(self, sandbox_id: str) -> None:
        """Delete a sandbox and dispose its code-intelligence service."""
        sb = self._get_proxy(sandbox_id)
        sb._raw.delete(timeout=_SANDBOX_TIMEOUT_SECONDS)
        # Dispose the per-sandbox CI service so it doesn't leak past the
        # underlying sandbox.
        self.dispose_code_intelligence(sandbox_id)
        _dispose_provider_adapter(sandbox_id)
        logger.info("Sandbox deleted: %s", sandbox_id)

    # -- Code Intelligence ----------------------------------------------------

    def code_intelligence_for(
        self,
        sandbox_id: str,
        *,
        workspace_root: str | None = None,
        sandbox: Any | None = None,
        transport: Any | None = None,
    ) -> CodeIntelligenceService:
        """Return the per-sandbox CI service, creating it lazily if needed.

        This is the only public way to obtain a :class:`CodeIntelligenceService`
        for code outside the ``sandbox`` package. The internal registry under
        :mod:`sandbox.runtime.registry` is reserved for whitebox
        tests; routers, benchmarks, and tool wiring must come through here.

        ``transport`` (Phase 1 Step 7) is optionally threaded through to the
        registry so downstream CI subsystems (overlay capture,
        ContentManager) take their Step 5 transport
        branches when invoked from production wiring.
        """
        from sandbox.runtime.registry import get_code_intelligence

        return get_code_intelligence(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root or "/workspace",
            sandbox=sandbox,
            transport=transport,
        )

    def code_intelligence_if_exists(
        self, sandbox_id: str
    ) -> CodeIntelligenceService | None:
        """Return the existing CI service for *sandbox_id*, or ``None``."""
        from sandbox.runtime.registry import (
            get_code_intelligence_if_exists,
        )

        return get_code_intelligence_if_exists(sandbox_id)

    def dispose_code_intelligence(self, sandbox_id: str) -> None:
        """Dispose the per-sandbox CI service. No-op if nothing exists."""
        from sandbox.runtime.registry import (
            dispose_code_intelligence as _dispose,
        )

        _dispose(sandbox_id)

    # -- Snapshots ------------------------------------------------------------

    def list_snapshots(self) -> list[dict[str, Any]]:
        """List available Daytona snapshots."""
        client = acquire_client()
        snapshot_api = getattr(client, "snapshot", None)
        if snapshot_api and hasattr(snapshot_api, "list"):
            items = _paginate_all(snapshot_api.list, _SNAPSHOT_PAGE_LIMIT)
        elif hasattr(client, "list_snapshots"):
            items = _paginate_all(client.list_snapshots, _SNAPSHOT_PAGE_LIMIT)
        else:
            logger.warning("Daytona client has no snapshot listing API")
            return []
        return [
            {
                "name": getattr(s, "name", ""),
                "state": str(getattr(s, "state", "unknown")),
                "image_name": getattr(s, "image_name", None),
            }
            for s in items
        ]

    # -- Preview URLs ---------------------------------------------------------

    def get_signed_preview_url(self, sandbox_id: str, port: int) -> dict[str, Any]:
        """Get a signed preview URL for a sandbox port."""
        sb = self.get_sandbox_object(sandbox_id)
        try:
            result = sb.create_signed_preview_url(port)
            return {
                "url": result.url,
                "token": result.token,
                "port": result.port,
            }
        except AttributeError:
            url = sb.get_preview_url(port)
            return {"url": url, "token": "", "port": port}

    # -- File operations ------------------------------------------------------

    def list_files_recursive(
        self,
        sandbox_id: str,
        root: str = "/workspace",
        max_depth: int = 10,
        max_items: int = 10_000,
    ) -> list[dict[str, Any]]:
        """List files recursively in a sandbox."""
        sb = self.get_sandbox_object(sandbox_id)
        fs = getattr(sb, "fs", None)
        list_files_fn = getattr(fs, "list_files", None)
        if not callable(list_files_fn):
            raise RuntimeError("Sandbox filesystem API is not available")

        import posixpath

        results: list[dict[str, Any]] = []
        pending: list[tuple[str, int]] = [(root, 0)]

        while pending:
            if len(results) >= max_items:
                break
            current, depth = pending.pop()
            entries = list_files_fn(current) or []
            for entry in entries:
                if len(results) >= max_items:
                    break
                name = getattr(entry, "name", None)
                if not isinstance(name, str) or not name or name in {".", ".."}:
                    continue
                child = posixpath.join(current, name)
                is_dir = bool(getattr(entry, "is_dir", False))
                results.append({"path": child, "name": name, "is_dir": is_dir})
                if is_dir and depth < max_depth:
                    pending.append((child, depth + 1))

        results.sort(key=lambda item: item["path"])
        return results


__all__ = ["SandboxService"]
