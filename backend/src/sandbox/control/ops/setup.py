"""Provider-neutral post-create / post-start setup orchestration.

The runtime-bundle upload runs concurrently with whatever else the create flow
does (today: ``ensure_git`` from :mod:`sandbox.control.ops.git`). Both depend
only on the sandbox existing; sequencing them serially leaves wall-clock time
on the table.

Bodies lifted from the deleted lifecycle helpers and rewritten against the
provider-neutral sandbox API.
"""

from __future__ import annotations

import concurrent.futures
import logging

from sandbox.control.ops.git import ensure_git

logger = logging.getLogger(__name__)

_BUNDLE_UPLOAD_THREAD_PREFIX = "eos-runtime-upload"
_BUNDLE_UPLOAD_JOIN_TIMEOUT_S = 60.0


async def bootstrap_in_sandbox_runtime(
    sandbox_id: str,
    workspace_root: str,
) -> None:
    """Upload the runtime command bundle during sandbox lifecycle events.

    Short-circuits as a no-op only when ``sandbox_id`` or ``workspace_root`` is
    empty. Raises when the runtime bundle cannot be prepared.
    """
    if not sandbox_id or not str(workspace_root or "").strip():
        return

    from sandbox.control.daemon.bundle import ensure_runtime_uploaded

    logger.info(
        "sandbox-runtime bootstrap starting for sandbox %s at %s",
        sandbox_id,
        workspace_root,
    )
    await ensure_runtime_uploaded(sandbox_id)
    logger.info(
        "sandbox-runtime bootstrap completed for sandbox %s at %s",
        sandbox_id,
        workspace_root,
    )


async def bootstrap_upload_runtime_bundle(
    sandbox_id: str,
    workspace_root: str,
) -> None:
    """Upload-only phase of the runtime bootstrap.

    Performs the chunked bundle upload without spawning the daemon. The
    create-sandbox path runs this concurrently with ``ensure_git`` (which
    is the other long pre-bootstrap step), then defers to the regular
    :func:`bootstrap_in_sandbox_runtime` afterwards. That call finds the
    bundle already in place via ``.bundle-hash``.

    Same input validation as :func:`bootstrap_in_sandbox_runtime`. Raises on upload
    failure; callers running this in a background thread are expected to
    swallow and let the sequential bootstrap retry.
    """
    if not sandbox_id or not str(workspace_root or "").strip():
        return

    from sandbox.control.daemon.bundle import ensure_runtime_uploaded

    logger.info(
        "sandbox-runtime bundle upload starting for sandbox %s",
        sandbox_id,
    )
    await ensure_runtime_uploaded(sandbox_id)
    logger.info(
        "sandbox-runtime bundle upload completed for sandbox %s",
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
            workspace_root=workspace,
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

    from sandbox.api.tool._runtime import call_runtime_api
    from sandbox.runtime.async_bridge import run_sync

    run_sync(
        call_runtime_api(
            sandbox_id,
            "api.ensure_workspace_base",
            {"workspace_root": workspace},
            timeout=180,
        )
    )


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
            bootstrap_upload_runtime_bundle(
                sandbox_id=sandbox_id,
                workspace_root=workspace,
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
    "bootstrap_upload_runtime_bundle",
    "ensure_workspace_base",
    "finish_runtime_bundle_upload",
    "run_runtime_bootstrap",
    "setup_after_create",
    "setup_after_start",
    "start_runtime_bundle_upload",
]
