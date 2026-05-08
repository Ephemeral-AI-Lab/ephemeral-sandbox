"""Provider-neutral post-create / post-start setup orchestration.

The runtime-bundle upload runs concurrently with whatever else the create flow
does (today: ``ensure_git`` from :mod:`sandbox.host.git`). Both depend
only on the sandbox existing; sequencing them serially leaves wall-clock time
on the table.

Bodies lifted from the deleted lifecycle helpers and rewritten against the
provider-neutral sandbox API.
"""

from __future__ import annotations

import concurrent.futures
import logging

from sandbox.host.git import ensure_git

logger = logging.getLogger(__name__)

_BUNDLE_UPLOAD_THREAD_PREFIX = "eos-runtime-upload"
_BUNDLE_UPLOAD_JOIN_TIMEOUT_S = 60.0


async def bootstrap_in_sandbox_runtime(
    sandbox_id: str,
) -> None:
    """Upload the runtime command bundle during sandbox lifecycle events.

    Short-circuits as a no-op when ``sandbox_id`` is empty. Raises when the
    runtime bundle cannot be prepared.
    """
    if not sandbox_id:
        return

    from sandbox.host.runtime_bundle import ensure_runtime_uploaded

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

    from sandbox.runtime.async_bridge import run_sync

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

    from sandbox.runtime.async_bridge import run_sync
    from sandbox.host.daemon_client import call_daemon_api

    run_sync(
        call_daemon_api(
            sandbox_id,
            "api.ensure_workspace_base",
            {"workspace_root": workspace},
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
    """Kick off the runtime-bundle upload in a background thread.

    Designed to overlap with the ~7 s ``ensure_git`` step in the create
    pipeline. Returns a future the caller MUST drain via
    :func:`finish_runtime_bundle_upload` before invoking
    :func:`run_runtime_bootstrap`. Returns ``None`` when there is no
    sandbox id or project_dir.

    Best-effort by design: the matching join helper swallows errors and
    timeouts so the sequential bootstrap can retry from scratch.
    """
    workspace = (workspace_root or "").strip()
    if not workspace or not sandbox_id:
        return None

    from sandbox.runtime.async_bridge import run_sync

    def _do_upload() -> None:
        run_sync(
            bootstrap_in_sandbox_runtime(
                sandbox_id=sandbox_id,
            )
        )

    pool = concurrent.futures.ThreadPoolExecutor(
        max_workers=1, thread_name_prefix=_BUNDLE_UPLOAD_THREAD_PREFIX,
    )
    try:
        future = pool.submit(_do_upload)
    finally:
        pool.shutdown(wait=False)
    return future


def finish_runtime_bundle_upload(
    future: concurrent.futures.Future[None] | None,
    sandbox_id: str,
) -> None:
    """Join the background bundle-upload future. Errors do not propagate.

    A failed background upload is recoverable: the subsequent sequential
    :func:`run_runtime_bootstrap` call will re-run
    ``ensure_runtime_uploaded`` and either find the bundle in place or
    retry the upload. Surfacing background failures here would mask that
    retry path.
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
    except Exception:
        logger.warning(
            "sandbox-runtime bundle upload failed for sandbox %s; "
            "sequential bootstrap will retry",
            sandbox_id,
            exc_info=True,
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


def setup_after_create(sandbox_id: str, workspace_root: str | None) -> None:
    """Post-create hook: ensure_git, runtime bootstrap, and workspace base.

    1. Start the bundle upload in the background (overlaps with ensure_git).
    2. Run ensure_git synchronously — installs git in minimal images that
       don't have it.
    3. Join the upload future (errors swallowed; sequential bootstrap retries).
    4. Run the sequential runtime bootstrap.
    5. Bind the assigned workspace and build its layer-stack base.
    """
    upload_future = start_runtime_bundle_upload(sandbox_id, workspace_root)
    ensure_git(sandbox_id)
    finish_runtime_bundle_upload(upload_future, sandbox_id)
    run_runtime_bootstrap(sandbox_id, workspace_root)
    ensure_workspace_base(sandbox_id, workspace_root)


def setup_after_start(sandbox_id: str, workspace_root: str | None) -> None:
    """Post-start hook: same setup sequence as create."""
    upload_future = start_runtime_bundle_upload(sandbox_id, workspace_root)
    ensure_git(sandbox_id)
    finish_runtime_bundle_upload(upload_future, sandbox_id)
    run_runtime_bootstrap(sandbox_id, workspace_root)
    ensure_workspace_base(sandbox_id, workspace_root)


__all__ = [
    "bootstrap_in_sandbox_runtime",
    "ensure_workspace_base",
    "finish_runtime_bundle_upload",
    "run_runtime_bootstrap",
    "setup_after_create",
    "setup_after_start",
    "start_runtime_bundle_upload",
]
