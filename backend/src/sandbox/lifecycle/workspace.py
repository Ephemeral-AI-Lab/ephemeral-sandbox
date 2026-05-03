"""Sandbox workspace discovery — resolve cwd and inject code intelligence services."""

from __future__ import annotations

import inspect
import logging
import os
from typing import Any

from config.defaults import DEFAULT_SANDBOX_CI_ROOT

logger = logging.getLogger(__name__)


def _ci_in_sandbox_enabled() -> bool:
    """Return True when ``EOS_CI_IN_SANDBOX=1`` (the daemon migration flag)."""
    return os.environ.get("EOS_CI_IN_SANDBOX") == "1"


async def bootstrap_in_sandbox_ci_runtime(
    sandbox_id: str,
    workspace_root: str,
    *,
    transport: Any,
) -> None:
    """Eager CI bootstrap — uploads the runtime bundle and starts the daemon.

    Called by ``SandboxService.create_sandbox`` and ``start_sandbox`` after
    the underlying Daytona sandbox is provisioned/resumed. Phase 2 makes this
    hook daemon-only; the Phase 1 indexer still runs from
    ``DaemonBackend.ensure_initialized`` when callers need symbol data.

    Short-circuits as a no-op when ``EOS_CI_IN_SANDBOX`` != ``"1"``,
    when ``transport`` is ``None``, or when ``workspace_root`` is empty.
    Raises when the daemon cannot be spawned or its socket never appears.

    This helper is intentionally distinct from
    :func:`ensure_code_intelligence_runtime`, which owns the orchestrator-side
    context preparation path. The two run in different contexts and must
    not collide.
    """
    if not _ci_in_sandbox_enabled():
        return
    if transport is None or not sandbox_id or not str(workspace_root or "").strip():
        return

    from sandbox.code_intelligence.daemon.launcher import DaemonLauncher

    logger.info(
        "eager CI daemon bootstrap starting for sandbox %s at %s",
        sandbox_id,
        workspace_root,
    )
    await DaemonLauncher(transport, sandbox_id, workspace_root).ensure_daemon()
    logger.info(
        "eager CI daemon bootstrap completed for sandbox %s at %s",
        sandbox_id,
        workspace_root,
    )


async def bootstrap_upload_runtime_bundle(
    sandbox_id: str,
    workspace_root: str,
    *,
    transport: Any,
) -> None:
    """Upload-only phase of the eager bootstrap.

    Performs the chunked bundle upload without spawning the daemon. The
    create-sandbox path runs this concurrently with ``ensure_git`` (which
    is the other long pre-bootstrap step), then defers to the regular
    :func:`bootstrap_in_sandbox_ci_runtime` afterwards — that call finds
    the bundle already in place via ``.bundle-hash`` and only spawns the
    daemon. Net effect: the upload's wall time overlaps with ``ensure_git``
    instead of stacking on top of it.

    Same gating as :func:`bootstrap_in_sandbox_ci_runtime`. Raises on
    upload failure; callers running this in a background thread are
    expected to swallow and let the sequential bootstrap retry.
    """
    if not _ci_in_sandbox_enabled():
        return
    if transport is None or not sandbox_id or not str(workspace_root or "").strip():
        return

    from sandbox.code_intelligence.daemon.launcher import ensure_runtime_uploaded

    logger.info(
        "eager CI bundle upload (background) starting for sandbox %s",
        sandbox_id,
    )
    await ensure_runtime_uploaded(transport, sandbox_id)
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

    Async sandbox warmup must stay lazy. Eager CI/LSP warmup against an async
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
    some CI/LSP warmup paths are synchronous and may survive across loop
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
    *,
    transport: Any | None = None,
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
        resolved_transport = (
            transport
            or context.get("sandbox_transport")
            or _build_sandbox_transport(sandbox_id)
        )
        method = service.code_intelligence_for
        kwargs: dict[str, Any] = {
            "workspace_root": ci_workspace_root,
            "sandbox": ci_sandbox,
        }
        if (
            resolved_transport is not None
            and "transport" in inspect.signature(method).parameters
        ):
            kwargs["transport"] = resolved_transport
        svc = method(sandbox_id, **kwargs)
        try:
            if eager_warmup_safe:
                if str(ci_workspace_root or "").strip():
                    svc.ensure_initialized(wait=True)
                else:
                    svc.lsp_client.ensure_ready(install_missing=False)
            else:
                logger.debug(
                    "Skipping eager CI warmup for async sandbox %s because "
                    "no sync handle was available; starting background "
                    "symbol index build",
                    sandbox_id,
                )
                # Full ensure_initialized is unsafe (LSP bootstrap may
                # corrupt the async event loop), but the symbol index
                # build runs in its own daemon thread and is safe to
                # start eagerly.  This gives the index a head start so
                # it is more likely to be ready by the time the first
                # ci_query_symbol call arrives.
                try:
                    svc.symbol_index.ensure_built(wait=False)
                except Exception:
                    logger.debug(
                        "Background symbol index start failed for %s",
                        sandbox_id,
                        exc_info=True,
                    )
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
    """Inject Daytona runtime metadata and attach code intelligence if available.

    This is the shared boundary for Daytona-backed tools. Callers may discover
    ``workspace_root`` differently (sync context prepare, async context prepare,
    lazy attach), but this helper owns the metadata contract and CI attachment.

    CI services are obtained through :class:`sandbox.service.SandboxService`.

    This constructs the provider-neutral :class:`SandboxApi` /
    :class:`CodeIntelligenceApi` / :class:`SandboxTransport` surface and
    attaches them to the context. Sandbox and CI tools consume these directly;
    the provider-specific handles remain available for runtime construction
    paths that still own the concrete Daytona sandbox object.
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
    if sandbox_id and not context.get("skip_code_intelligence"):
        transport = _build_sandbox_transport(sandbox_id)
        if transport is not None:
            context["sandbox_transport"] = transport
        _attach_code_intelligence(context, sandbox_id, sandbox, ci_root)
        _attach_provider_neutral_api(context, sandbox_id, sandbox, transport=transport)


def _build_sandbox_transport(sandbox_id: str) -> Any | None:
    try:
        from sandbox.daytona.transport import DaytonaTransport

        return DaytonaTransport()
    except Exception:
        logger.debug(
            "Sandbox transport attachment failed for sandbox %s",
            sandbox_id,
            exc_info=True,
        )
        return None


def _attach_provider_neutral_api(
    context: Any,
    sandbox_id: str,
    sandbox: Any,
    *,
    transport: Any | None = None,
) -> None:
    """Attach the Phase-1 ``SandboxApi`` / ``CodeIntelligenceApi`` surface.

    Constructs one :class:`DaytonaTransport`, one :class:`AuditedSandboxApi`,
    and one :class:`SvcCodeIntelligence` per context. Failures are swallowed
    so provider-neutral attachment does not widen the runtime preparation
    failure surface.
    """
    try:
        from sandbox.api.audited_sandbox_api import AuditedSandboxApi
        from sandbox.api.code_intelligence_impl import SvcCodeIntelligence

        svc = context.get("ci_service")
        resolved_transport = (
            transport
            or context.get("sandbox_transport")
            or _build_sandbox_transport(sandbox_id)
        )
        if svc is None or resolved_transport is None:
            return
        context["sandbox_transport"] = resolved_transport
        if sandbox is not None:
            context["sandbox_api"] = AuditedSandboxApi(
                transport=resolved_transport, svc=svc, sandbox=sandbox,
            )
        context["code_intelligence_api"] = SvcCodeIntelligence(svc)
    except Exception:
        logger.debug(
            "Provider-neutral API attachment failed for sandbox %s",
            sandbox_id,
            exc_info=True,
        )
