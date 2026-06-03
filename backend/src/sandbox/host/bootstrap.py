"""Provider-neutral sandbox lifecycle bootstrap and recovery."""

from __future__ import annotations

import atexit
import concurrent.futures
import logging
from typing import Any, Literal

from sandbox._shared.async_bridge import run_sync
from sandbox.host.daemon_client import _DaemonDispatchError, call_daemon_api
from sandbox.host.runtime_bundle import ensure_runtime_uploaded
from sandbox.provider.registry import get_adapter

logger = logging.getLogger(__name__)

_BUNDLE_UPLOAD_THREAD_PREFIX = "eos-runtime-upload"
_BUNDLE_UPLOAD_JOIN_TIMEOUT_S = 60.0
_BUNDLE_UPLOAD_EXECUTOR = concurrent.futures.ThreadPoolExecutor(
    max_workers=4,
    thread_name_prefix=_BUNDLE_UPLOAD_THREAD_PREFIX,
)
atexit.register(_BUNDLE_UPLOAD_EXECUTOR.shutdown, wait=False, cancel_futures=True)
LifecyclePhase = Literal["create", "start"]


async def bootstrap_in_sandbox_runtime(
    sandbox_id: str,
) -> None:
    """Upload the runtime command bundle. No-op when sandbox_id is empty."""
    if not sandbox_id:
        return

    logger.info(
        "sandbox-runtime bootstrap starting for sandbox %s",
        sandbox_id,
    )
    await ensure_runtime_uploaded(sandbox_id)
    logger.info(
        "sandbox-runtime bootstrap completed for sandbox %s",
        sandbox_id,
    )


def run_runtime_bootstrap(
    sandbox_id: str,
    workspace_root: str | None,
) -> None:
    """Run the sequential runtime bootstrap when sandbox workspace is ready."""
    workspace = (workspace_root or "").strip()
    if not workspace or not sandbox_id:
        logger.debug(
            "sandbox-runtime bootstrap skipped for sandbox %s: no project_dir",
            sandbox_id,
        )
        return

    run_sync(
        bootstrap_in_sandbox_runtime(
            sandbox_id=sandbox_id,
        )
    )


def ensure_workspace_base(
    sandbox_id: str,
    workspace_root: str | None,
) -> None:
    """Bind the assigned workspace and build its layer-stack base once."""
    workspace = (workspace_root or "").strip()
    if not workspace or not sandbox_id:
        logger.debug(
            "layer-stack workspace base skipped for sandbox %s: no project_dir",
            sandbox_id,
        )
        return

    try:
        run_sync(
            call_daemon_api(
                sandbox_id,
                "api.ensure_workspace_base",
                {"workspace_root": workspace},
                timeout=180,
            )
        )
    except _DaemonDispatchError as exc:
        if not _is_workspace_binding_mismatch(exc):
            raise
        logger.info(
            "rebuilding sandbox workspace base for %s after binding mismatch: %s",
            sandbox_id,
            exc.message,
        )
        run_sync(
            call_daemon_api(
                sandbox_id,
                "api.build_workspace_base",
                {"workspace_root": workspace, "reset": True},
                timeout=180,
            )
        )
    readiness = run_sync(
        call_daemon_api(
            sandbox_id,
            "api.runtime.ready",
            {},
            timeout=60,
        )
    )
    _require_workspace_base_ready(readiness)


def start_runtime_bundle_upload(
    sandbox_id: str,
    workspace_root: str | None,
) -> concurrent.futures.Future[None] | None:
    """Start the bundle upload in a background thread to overlap with ensure_git.

    Best-effort: the matching join helper swallows errors so the sequential
    bootstrap can retry. Caller MUST drain via finish_runtime_bundle_upload.
    """
    workspace = (workspace_root or "").strip()
    if not workspace or not sandbox_id:
        return None

    def _do_upload() -> None:
        run_sync(
            bootstrap_in_sandbox_runtime(
                sandbox_id=sandbox_id,
            )
        )

    future = _BUNDLE_UPLOAD_EXECUTOR.submit(_do_upload)
    future.add_done_callback(_log_background_upload_exception)
    return future


def finish_runtime_bundle_upload(
    future: concurrent.futures.Future[None] | None,
    sandbox_id: str,
) -> None:
    """Join the background bundle-upload future. Errors swallowed by design:
    run_runtime_bootstrap will retry; surfacing errors here would mask that.
    """
    if future is None:
        return
    try:
        future.result(timeout=_BUNDLE_UPLOAD_JOIN_TIMEOUT_S)
        logger.info(
            "sandbox-runtime bundle upload joined for sandbox %s",
            sandbox_id,
        )
    except concurrent.futures.TimeoutError:
        logger.warning(
            "sandbox-runtime bundle upload did not complete within %.0fs "
            "for sandbox %s; sequential bootstrap will retry",
            _BUNDLE_UPLOAD_JOIN_TIMEOUT_S,
            sandbox_id,
        )
    except RuntimeError:
        logger.error(
            "sandbox-runtime bundle upload failed for sandbox %s; "
            "sequential bootstrap will retry",
            sandbox_id,
            exc_info=True,
        )


def _log_background_upload_exception(
    future: concurrent.futures.Future[None],
) -> None:
    if future.cancelled():
        return
    try:
        exc = future.exception()
    except concurrent.futures.CancelledError:
        return
    except Exception:
        logger.warning(
            "sandbox-runtime background upload future failed before join",
            exc_info=True,
        )
        return
    if exc is not None:
        logger.debug(
            "sandbox-runtime background upload future completed with error",
            exc_info=(type(exc), exc, exc.__traceback__),
        )


def _require_workspace_base_ready(readiness: dict[str, object]) -> None:
    control_plane = _runtime_probe(readiness, "control_plane")
    details = control_plane.get("details")
    detail_map = details if isinstance(details, dict) else {}
    manifest_version = int(detail_map.get("manifest_version") or 0)
    if (
        readiness.get("ready") is not True
        or control_plane.get("status") != "ok"
        or manifest_version < 1
    ):
        raise RuntimeError(f"sandbox runtime not ready after workspace base: {readiness}")


def _is_workspace_binding_mismatch(exc: _DaemonDispatchError) -> bool:
    return "workspace binding points at a different workspace" in exc.message


def _runtime_probe(
    readiness: dict[str, object],
    name: str,
) -> dict[str, object]:
    probes = readiness.get("probes")
    if not isinstance(probes, list):
        return {}
    for probe in probes:
        if isinstance(probe, dict) and probe.get("name") == name:
            return probe
    return {}


def ensure_git(sandbox_id: str) -> None:
    """Install git if missing. Best-effort: install failures are logged,
    adapter/config failures propagate (sandbox is broken)."""
    if not sandbox_id:
        return
    try:
        adapter = get_adapter(sandbox_id)
        logger.info("ensure_git(%s): probe starting", sandbox_id)
        resp = run_sync(
            adapter.exec(
                sandbox_id,
                "command -v git >/dev/null 2>&1 && echo ok || echo missing",
                timeout=10,
            )
        )
        if "ok" in (resp.stdout or ""):
            logger.info("ensure_git(%s): git already available", sandbox_id)
            return
        logger.info("ensure_git(%s): installing git", sandbox_id)
        install = run_sync(
            adapter.exec(sandbox_id, _install_git_script(), timeout=120)
        )
        if getattr(install, "exit_code", 1) not in (0, None):
            raise RuntimeError(
                getattr(install, "stderr", "")
                or getattr(install, "stdout", "")
                or "git install failed"
            )
        logger.info("ensure_git(%s): install completed", sandbox_id)
    except RuntimeError as exc:
        logger.warning("Git bootstrap failed for sandbox %s: %s", sandbox_id, exc)
    except Exception:
        logger.exception(
            "Git bootstrap unexpectedly failed for sandbox %s; propagating to caller",
            sandbox_id,
        )
        raise


def ensure_running(sandbox_id: str) -> dict[str, Any]:
    """Best-effort recovery: probe, restart on failure, re-run start bootstrap."""
    adapter = get_adapter(sandbox_id)
    info = adapter.get(sandbox_id)
    try:
        resp = run_sync(adapter.exec(sandbox_id, "pwd", timeout=10))
        exit_code = getattr(resp, "exit_code", 0)
        if exit_code in (None, 0):
            return info
    except Exception:
        logger.warning(
            "Sandbox %s probe failed; attempting restart recovery",
            sandbox_id,
            exc_info=True,
        )

    try:
        adapter.start(sandbox_id)
    except Exception:
        logger.debug(
            "Sandbox %s start during recovery raised; refreshing handle",
            sandbox_id,
            exc_info=True,
        )

    info = adapter.get(sandbox_id)
    workspace_root = info.get("project_dir") or ""
    setup_after_start(sandbox_id, workspace_root)
    return info


def _install_git_script() -> str:
    return """\
set -e
if command -v git >/dev/null 2>&1; then exit 0; fi
echo "[sandbox] Installing git..."
as_root() {
    if [ "$(id -u)" = "0" ]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo -n "$@"
    else
        return 1
    fi
}
if command -v apt-get >/dev/null 2>&1; then
    as_root mkdir -p /var/lib/apt/lists/partial
    as_root apt-get update -qq && as_root apt-get install -y -qq git
elif command -v apk >/dev/null 2>&1; then
    as_root apk add --no-cache git
elif command -v microdnf >/dev/null 2>&1; then
    as_root microdnf install -y git
elif command -v dnf >/dev/null 2>&1; then
    as_root dnf install -y git
elif command -v yum >/dev/null 2>&1; then
    as_root yum install -y git
else
    echo "[sandbox] No package manager found; git not installed" >&2
    exit 1
fi
echo "[sandbox] git installed"
"""


def setup_post_lifecycle(
    sandbox_id: str,
    workspace_root: str | None,
    *,
    phase: LifecyclePhase,
) -> None:
    """Run the shared post-create/post-start bootstrap sequence."""
    logger.debug("running sandbox post-%s bootstrap for %s", phase, sandbox_id)
    upload_future = start_runtime_bundle_upload(sandbox_id, workspace_root)
    ensure_git(sandbox_id)
    finish_runtime_bundle_upload(upload_future, sandbox_id)
    run_runtime_bootstrap(sandbox_id, workspace_root)
    ensure_workspace_base(sandbox_id, workspace_root)


def setup_after_create(sandbox_id: str, workspace_root: str | None) -> None:
    """Post-create hook."""
    setup_post_lifecycle(sandbox_id, workspace_root, phase="create")


def setup_after_start(sandbox_id: str, workspace_root: str | None) -> None:
    """Post-start hook: same setup sequence as create."""
    setup_post_lifecycle(sandbox_id, workspace_root, phase="start")


__all__ = [
    "bootstrap_in_sandbox_runtime",
    "ensure_git",
    "ensure_running",
    "ensure_workspace_base",
    "finish_runtime_bundle_upload",
    "run_runtime_bootstrap",
    "setup_post_lifecycle",
    "setup_after_create",
    "setup_after_start",
    "start_runtime_bundle_upload",
]
