"""Sandbox workspace discovery — resolve cwd and inject code intelligence services."""

from __future__ import annotations

import inspect
import logging
from typing import Any

logger = logging.getLogger(__name__)


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
    which then breaks later ``daytona_*`` tool calls with
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
        from sandbox.service import SandboxService

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


def inject_code_intelligence(
    context: Any,
    sandbox_id: str | None,
    sandbox: Any,
    workspace_root: str,
) -> None:
    if sandbox_id and "ci_service" not in context.metadata:
        try:
            from code_intelligence.routing.service import get_code_intelligence

            ci_sandbox, eager_warmup_safe = _ci_sandbox_handle(sandbox_id, sandbox)
            ci_workspace_root = _ci_workspace_root(workspace_root, ci_sandbox)
            svc = get_code_intelligence(
                sandbox_id=sandbox_id,
                workspace_root=ci_workspace_root,
                sandbox=ci_sandbox,
            )
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
                    # ci_query_symbols call arrives.
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
                    "CI service warmup skipped for sandbox %s", sandbox_id, exc_info=True
                )
            context.metadata["ci_service"] = svc
        except Exception:
            logger.debug("CI service not available for sandbox %s", sandbox_id)
