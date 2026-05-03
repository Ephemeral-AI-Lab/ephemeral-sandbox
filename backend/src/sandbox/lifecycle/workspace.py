"""Sandbox workspace discovery — resolve cwd and inject code intelligence services."""

from __future__ import annotations

import inspect
import logging
import os
from typing import Any

from config.defaults import DEFAULT_SANDBOX_CI_ROOT

logger = logging.getLogger(__name__)


def _ci_in_sandbox_enabled() -> bool:
    """Return True when ``EOS_CI_IN_SANDBOX=1``."""
    return os.environ.get("EOS_CI_IN_SANDBOX") == "1"


async def bootstrap_in_sandbox_ci_runtime(
    sandbox_id: str,
    workspace_root: str,
) -> None:
    """Eager CI bootstrap — uploads the runtime command bundle.

    Called by ``SandboxService.create_sandbox`` and ``start_sandbox`` after
    the underlying Daytona sandbox is provisioned/resumed.

    Short-circuits as a no-op when ``EOS_CI_IN_SANDBOX`` != ``"1"``,
    when ``sandbox_id`` or ``workspace_root`` is empty. Raises when the runtime
    bundle cannot be prepared.

    This helper is intentionally distinct from
    :func:`ensure_code_intelligence_runtime`, which owns the orchestrator-side
    context preparation path. The two run in different contexts and must
    not collide.
    """
    if not _ci_in_sandbox_enabled():
        return
    if not sandbox_id or not str(workspace_root or "").strip():
        return

    from sandbox.runtime.bundle import ensure_runtime_uploaded

    logger.info(
        "eager CI command bootstrap starting for sandbox %s at %s",
        sandbox_id,
        workspace_root,
    )
    await ensure_runtime_uploaded(sandbox_id)
    logger.info(
        "eager CI command bootstrap completed for sandbox %s at %s",
        sandbox_id,
        workspace_root,
    )


async def bootstrap_upload_runtime_bundle(
    sandbox_id: str,
    workspace_root: str,
) -> None:
    """Upload-only phase of the eager bootstrap.

    Performs the chunked bundle upload without spawning the daemon. The
    create-sandbox path runs this concurrently with ``ensure_git`` (which
    is the other long pre-bootstrap step), then defers to the regular
    :func:`bootstrap_in_sandbox_ci_runtime` afterwards. That call finds
    the bundle already in place via ``.bundle-hash``. Net effect: the
    upload's wall time overlaps with ``ensure_git``
    instead of stacking on top of it.

    Same gating as :func:`bootstrap_in_sandbox_ci_runtime`. Raises on upload
    failure; callers running this in a background thread are expected to
    swallow and let the sequential bootstrap retry.
    """
    if not _ci_in_sandbox_enabled():
        return
    if not sandbox_id or not str(workspace_root or "").strip():
        return

    from sandbox.runtime.bundle import ensure_runtime_uploaded

    logger.info(
        "eager CI bundle upload (background) starting for sandbox %s",
        sandbox_id,
    )
    await ensure_runtime_uploaded(sandbox_id)
    logger.info(
        "eager CI bundle upload (background) completed for sandbox %s",
        sandbox_id,
    )


def _sandbox_project_root(sandbox: Any) -> str | None:
    project_dir = getattr(sandbox, "project_dir", None)
    if isinstance(project_dir, str) and project_dir.strip():
        return project_dir.strip()
    labels = getattr(sandbox, "labels", None)
    if isinstance(labels, dict):
        label_dir = labels.get("project_dir")
        if isinstance(label_dir, str) and label_dir.strip():
            return label_dir.strip()
    return None


def _ci_workspace_root(workspace_root: str, sandbox: Any) -> str:
    resolved = _sandbox_project_root(sandbox)
    if resolved:
        return resolved
    return str(workspace_root or "").strip()


def _sandbox_exec_is_async(sandbox: Any) -> bool:
    """Best-effort detection for async Daytona sandbox wrappers.

    Async sandbox warmup must stay lazy. Eager CI warmup against an async
    sandbox can corrupt the shared aiohttp client across loop boundaries,
    which then breaks later sandbox tool calls with
    ``RuntimeError('Event loop is closed')``.
    """
    process = getattr(sandbox, "process", None)
    exec_fn = getattr(process, "exec", None)
    return bool(exec_fn) and inspect.iscoroutinefunction(exec_fn)


def _ci_sandbox_handle(sandbox_id: str | None, sandbox: Any) -> tuple[Any, bool]:
    """Return a sandbox handle plus whether eager CI warmup is safe.

    Worker tools use an async sandbox so shell/file operations can be awaited
    and cancelled. CI warmup should not reuse that same async handle because
    some CI warmup paths are synchronous and may survive across loop
    boundaries. When we detect an async Daytona sandbox, resolve a separate
    sync handle for CI instead.

    If the sync handle cannot be resolved, we still return the original
    sandbox so the CI service can be attached lazily, but we mark eager
    warmup as unsafe. The loop-corruption risk comes from the warmup calls,
    not from storing the sandbox reference itself.
    """
    if not sandbox_id or sandbox is None or not _sandbox_exec_is_async(sandbox):
        return sandbox, True
    try:
        from sandbox.lifecycle.service import SandboxService

        return SandboxService().get_sandbox_object(sandbox_id), True
    except Exception:
        logger.debug(
            "Could not resolve sync sandbox handle for CI warmup on %s; "
            "using async handle without eager warmup",
            sandbox_id,
            exc_info=True,
        )
        return sandbox, False


def discover_workspace(sandbox: Any) -> str | None:
    project_dir = _sandbox_project_root(sandbox)
    if project_dir:
        return project_dir
    try:
        resp = sandbox.process.exec("pwd")
        if resp.exit_code == 0 and resp.result:
            return resp.result.strip()
    except Exception:
        pass
    return None


async def discover_workspace_async(sandbox: Any) -> str | None:
    project_dir = _sandbox_project_root(sandbox)
    if project_dir:
        return project_dir
    try:
        resp = await sandbox.process.exec("pwd")
        if resp.exit_code == 0 and resp.result:
            return resp.result.strip()
    except Exception:
        pass
    return None


def _attach_code_intelligence(
    context: Any,
    sandbox_id: str,
    sandbox: Any,
    workspace_root: str,
) -> None:
    """Internal helper — attach a CI service via SandboxService.

    Kept private to ``sandbox.workspace`` so external code does not call it
    directly. Callers must reach CI via :class:`sandbox.service.SandboxService`.
    """
    if context.get("ci_service") is not None:
        return
    try:
        from sandbox.lifecycle.service import SandboxService

        ci_sandbox, eager_warmup_safe = _ci_sandbox_handle(sandbox_id, sandbox)
        ci_workspace_root = _ci_workspace_root(workspace_root, ci_sandbox)
        service = SandboxService()
        method = service.code_intelligence_for
        kwargs: dict[str, Any] = {
            "workspace_root": ci_workspace_root,
            "sandbox": ci_sandbox,
        }
        svc = method(sandbox_id, **kwargs)
        try:
            if eager_warmup_safe:
                svc.ensure_initialized(wait=True)
        except Exception:
            logger.debug(
                "CI service warmup skipped for sandbox %s",
                sandbox_id,
                exc_info=True,
            )
        context["ci_service"] = svc
    except Exception:
        logger.debug("CI service not available for sandbox %s", sandbox_id)


def ensure_code_intelligence_runtime(
    context: Any,
    *,
    sandbox_id: str | None,
    sandbox: Any,
    workspace_root: str | None,
    default_ci_root: str = DEFAULT_SANDBOX_CI_ROOT,
) -> None:
    """Inject sandbox runtime metadata and attach code intelligence if available.

    This is the shared boundary for Daytona-backed tools. Callers may discover
    ``workspace_root`` differently (sync context prepare, async context prepare,
    lazy attach), but this helper owns the metadata contract and CI attachment.

    CI services are obtained through :class:`sandbox.service.SandboxService`.

    This registers the provider adapter and leaves guarded operations to the
    public ``sandbox.api`` verb modules.
    """
    if sandbox is not None:
        context["daytona_sandbox"] = sandbox

    repo_root = str(context.get("repo_root") or "").strip()
    if not repo_root:
        candidate = str(workspace_root or "").strip()
        if not candidate and sandbox is not None:
            candidate = _sandbox_project_root(sandbox) or ""
        if candidate:
            repo_root = candidate
            context["repo_root"] = repo_root

    if not context.get("exec_cwd") and repo_root:
        context["exec_cwd"] = repo_root

    ci_root = (
        str(context.get("ci_workspace_root") or "").strip()
        or repo_root
        or str(workspace_root or "").strip()
        or default_ci_root
    )
    if sandbox_id:
        _register_provider_adapter_if_missing(sandbox_id)
    if sandbox_id and not context.get("skip_code_intelligence"):
        _attach_code_intelligence(context, sandbox_id, sandbox, ci_root)


def _register_provider_adapter_if_missing(sandbox_id: str) -> None:
    if not sandbox_id:
        return
    try:
        from sandbox.providers.registry import get_adapter

        get_adapter(sandbox_id)
        return
    except KeyError:
        pass
    except Exception:
        logger.debug(
            "Provider adapter lookup failed for sandbox %s",
            sandbox_id,
            exc_info=True,
        )
        return
    try:
        from sandbox.providers.daytona.adapter import DaytonaProviderAdapter
        from sandbox.providers.registry import register_adapter

        register_adapter(sandbox_id, DaytonaProviderAdapter())
    except Exception:
        logger.debug(
            "Provider adapter attachment failed for sandbox %s",
            sandbox_id,
            exc_info=True,
        )
