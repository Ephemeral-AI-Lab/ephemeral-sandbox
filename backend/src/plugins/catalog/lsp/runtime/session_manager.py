"""Layer-stack-root keyed cache of Pyright sessions rooted at leased overlays."""

from __future__ import annotations

import asyncio
import logging
import shutil
import time
from typing import Any

from plugins.catalog.lsp.runtime.pyright_session import (
    PyrightOverlayRefreshError,
    PyrightSession,
)

__all__ = ["get_session", "evict_all", "evict_for_root"]


logger = logging.getLogger(__name__)


_sessions: dict[str, PyrightSession] = {}
_locks: dict[str, asyncio.Lock] = {}
_event_tasks: dict[str, asyncio.Task[None]] = {}
_event_subscriptions: dict[str, tuple[Any, str]] = {}

# Token-bucket-ish: emit at most one degraded-dispatch warning per
# layer_stack_root per window so production callers still see signal without
# flooding logs when a misconfigured pipeline keeps falling back.
_DEGRADED_WARN_INTERVAL_S = 60.0
_degraded_warn_last: dict[str, float] = {}


async def get_session(ctx: Any) -> PyrightSession:
    """Return a Pyright session reconciled to the active layer-stack snapshot."""
    layer_stack_root = str(ctx.layer_stack_root)
    lock = _locks.setdefault(layer_stack_root, asyncio.Lock())
    async with lock:
        active_key = await _active_manifest_key(ctx)
        cached = _sessions.get(layer_stack_root)
        if cached is not None and cached.manifest_key != active_key:
            if _session_owns_overlay_handle(cached):
                workspace_root = _declared_workspace_root(ctx)
                if cached.workspace_root == workspace_root:
                    refreshed = await _refresh_owned_session(
                        ctx,
                        cached,
                        active_key=active_key,
                    )
                    if not refreshed:
                        cached = None
                else:
                    logger.info(
                        "pyright session workspace root changed; restarting",
                        extra={
                            "old_workspace_root": cached.workspace_root,
                            "new_workspace_root": workspace_root,
                        },
                    )
                    await cached.evict()
                    _sessions.pop(layer_stack_root, None)
                    cached = None
            else:
                await cached.refresh_manifest(manifest_key=active_key)
        if cached is not None and not _session_owns_overlay_handle(cached):
            workspace_root = _declared_workspace_root(ctx)
            if workspace_root and cached.workspace_root != workspace_root:
                logger.info(
                    "pyright session workspace root changed; restarting",
                    extra={
                        "old_workspace_root": cached.workspace_root,
                        "new_workspace_root": workspace_root,
                    },
                )
                await cached.evict()
                _sessions.pop(layer_stack_root, None)
                cached = None
        if cached is not None:
            _ensure_event_subscription(
                layer_stack_root,
                getattr(ctx, "overlay", None),
                cached,
            )
            return cached

        session_view = _acquire_session_view(ctx, active_key=active_key)
        session = PyrightSession(
            manifest_key=session_view.manifest_key,
            workspace_root=session_view.workspace_root,
            overlay_handle=session_view.handle,
        )
        _sessions[layer_stack_root] = session
        _ensure_event_subscription(
            layer_stack_root,
            getattr(ctx, "overlay", None),
            session,
        )
        return session


async def evict_for_root(layer_stack_root: str) -> None:
    task = _event_tasks.pop(layer_stack_root, None)
    if task is not None:
        task.cancel()
    subscription = _event_subscriptions.pop(layer_stack_root, None)
    if subscription is not None:
        overlay, subscriber_id = subscription
        unsubscribe = getattr(overlay, "unsubscribe_workspace_changes", None)
        if callable(unsubscribe):
            unsubscribe(subscriber_id)
    cached = _sessions.pop(layer_stack_root, None)
    if cached is not None:
        await cached.evict()


async def evict_all() -> None:
    for root in list(_sessions.keys()):
        await evict_for_root(root)


async def _active_manifest_key(ctx: Any) -> str:
    projection = getattr(ctx, "projection", None)
    if projection is not None and hasattr(projection, "active_manifest_key"):
        return str(projection.active_manifest_key())
    overlay = getattr(ctx, "overlay", None)
    if overlay is not None and hasattr(overlay, "ensure_current"):
        metadata = getattr(ctx, "metadata", None) or {}
        op_name = str(metadata.get("op_name", "tool"))
        return str(await overlay.ensure_current(reason=f"lsp:{op_name}:enter"))
    return "workspace@0"


class _SessionView:
    def __init__(
        self,
        *,
        manifest_key: str,
        workspace_root: str,
        handle: Any | None,
    ) -> None:
        self.manifest_key = manifest_key
        self.workspace_root = workspace_root
        self.handle = handle


def _acquire_session_view(ctx: Any, *, active_key: str) -> _SessionView:
    workspace_root = _declared_workspace_root(ctx)
    view = _dispatch_lsp_overlay_acquire(
        ctx, invocation_id=_invocation_id_for_ctx(ctx), workspace_root=workspace_root
    )
    if view is None:
        _warn_degraded_lsp_dispatch(ctx)
    return view or _SessionView(
        manifest_key=active_key, workspace_root=workspace_root, handle=None
    )


def _invocation_id_for_ctx(ctx: Any) -> str:
    metadata = getattr(ctx, "metadata", None) or {}
    op_name = str(metadata.get("op_name", "lsp"))
    return f"lsp-session:{op_name}"


def _dispatch_lsp_overlay_acquire(
    ctx: Any,
    *,
    invocation_id: str,
    workspace_root: str,
) -> _SessionView | None:
    """Acquire an LSP-session overlay handle across all pipeline shapes.

    Three shapes:
    * ``EphemeralPipeline`` daemon ctx → ``ctx.overlay.acquire_operation_overlay``.
    * ``IsolatedPipeline`` projection ctx → ``ctx.projection.acquire_overlay``.
    * Degraded ctx (legacy projection stubs) → ``ctx.projection.acquire``.
    """
    overlay = getattr(ctx, "overlay", None)
    acquire_operation_overlay = getattr(overlay, "acquire_operation_overlay", None)
    if callable(acquire_operation_overlay):
        view = _session_view_from(
            acquire_operation_overlay(
                invocation_id=invocation_id,
                workspace_root=workspace_root,
            ),
            workspace_root=workspace_root,
        )
        if view is not None:
            return view

    projection = getattr(ctx, "projection", None)
    acquire_overlay = getattr(projection, "acquire_overlay", None)
    if callable(acquire_overlay):
        view = _session_view_from(
            acquire_overlay(invocation_id, workspace_root=workspace_root),
            workspace_root=workspace_root,
        )
        if view is not None:
            return view

    acquire = getattr(projection, "acquire", None)
    if callable(acquire):
        view = _session_view_from(
            acquire("lsp-session"),
            workspace_root=workspace_root,
        )
        if view is not None:
            return view

    return None


def _session_view_from(handle: Any, *, workspace_root: str) -> _SessionView | None:
    if getattr(handle, "layer_paths", None):
        return _SessionView(
            manifest_key=handle.manifest_key,
            workspace_root=workspace_root,
            handle=handle,
        )
    _release_handle(handle)
    return None


def _warn_degraded_lsp_dispatch(ctx: Any) -> None:
    """Rate-limited warning: no pipeline shape produced an overlay handle."""
    layer_stack_root = str(getattr(ctx, "layer_stack_root", "") or "unknown")
    now = time.monotonic()
    last = _degraded_warn_last.get(layer_stack_root, 0.0)
    if now - last < _DEGRADED_WARN_INTERVAL_S:
        return
    _degraded_warn_last[layer_stack_root] = now
    overlay = getattr(ctx, "overlay", None)
    projection = getattr(ctx, "projection", None)
    logger.warning(
        "lsp overlay dispatch degraded: pyright session running without a leased "
        "overlay snapshot; check pipeline wiring",
        extra={
            "layer_stack_root": layer_stack_root,
            "overlay_kind": type(overlay).__name__ if overlay is not None else "",
            "projection_kind": type(projection).__name__ if projection is not None else "",
        },
    )


def _declared_workspace_root(ctx: Any) -> str:
    overlay = getattr(ctx, "overlay", None)
    metadata = getattr(ctx, "metadata", None) or {}
    return str(
        metadata.get("workspace_root")
        or getattr(overlay, "workspace_root", "")
        or "/testbed"
    )


def _session_owns_overlay_handle(session: Any) -> bool:
    return getattr(session, "_overlay_handle", None) is not None


async def _refresh_owned_session(
    ctx: Any,
    session: PyrightSession,
    *,
    active_key: str,
) -> bool:
    layer_stack_root = str(ctx.layer_stack_root)
    session_view = _acquire_session_view(ctx, active_key=active_key)
    try:
        await session.refresh_manifest(
            manifest_key=session_view.manifest_key,
            overlay_handle=session_view.handle,
            workspace_root=session_view.workspace_root,
        )
        logger.info("pyright session snapshot refreshed in place")
        return True
    except (PyrightOverlayRefreshError, OSError, asyncio.TimeoutError):
        _release_handle(session_view.handle)
        logger.info("pyright session snapshot refresh failed; restarting")
        await session.evict()
        _sessions.pop(layer_stack_root, None)
        return False


def _ensure_event_subscription(
    layer_stack_root: str,
    overlay: Any,
    session: PyrightSession,
) -> None:
    subscribe = getattr(overlay, "subscribe_workspace_changes", None)
    if not callable(subscribe):
        return
    existing = _event_subscriptions.get(layer_stack_root)
    if existing is not None and existing[0] is overlay:
        return
    if existing is not None:
        existing_unsubscribe = getattr(
            existing[0], "unsubscribe_workspace_changes", None
        )
        if callable(existing_unsubscribe):
            existing_unsubscribe(existing[1])
    task = _event_tasks.pop(layer_stack_root, None)
    if task is not None:
        task.cancel()
    subscriber_id = f"lsp:{layer_stack_root}"
    queue = subscribe(subscriber_id)
    _event_subscriptions[layer_stack_root] = (overlay, subscriber_id)
    _event_tasks[layer_stack_root] = asyncio.create_task(
        _pump_workspace_events(layer_stack_root, overlay, session, queue)
    )


async def _pump_workspace_events(
    layer_stack_root: str,
    overlay: Any,
    session: PyrightSession,
    queue: asyncio.Queue[Any],
) -> None:
    while True:
        event = await queue.get()
        lock = _locks.setdefault(layer_stack_root, asyncio.Lock())
        async with lock:
            cached = _sessions.get(layer_stack_root)
            if cached is not session:
                return
            active_key = (
                overlay.active_manifest_key()
                if hasattr(overlay, "active_manifest_key")
                else session.manifest_key
            )
            if active_key != session.manifest_key:
                session_view = _acquire_session_view_from_overlay(
                    overlay,
                    active_key=active_key,
                    op_name="event",
                    workspace_root=session.workspace_root,
                )
                try:
                    await session.refresh_manifest(
                        manifest_key=session_view.manifest_key,
                        overlay_handle=session_view.handle,
                        workspace_root=session_view.workspace_root,
                    )
                except (PyrightOverlayRefreshError, OSError, asyncio.TimeoutError):
                    _release_handle(session_view.handle)
                    await session.evict()
                    _sessions.pop(layer_stack_root, None)
                    return
            elif getattr(event, "changes", ()):
                await session.refresh_manifest(manifest_key=active_key)


def _acquire_session_view_from_overlay(
    overlay: Any,
    *,
    active_key: str,
    op_name: str,
    workspace_root: str,
) -> _SessionView:
    acquire_operation_overlay = getattr(overlay, "acquire_operation_overlay", None)
    if callable(acquire_operation_overlay):
        handle = acquire_operation_overlay(
            invocation_id=f"lsp-session:{op_name}",
            workspace_root=workspace_root,
        )
        if getattr(handle, "layer_paths", None):
            return _SessionView(
                manifest_key=handle.manifest_key,
                workspace_root=workspace_root,
                handle=handle,
            )
        _release_handle(handle)
    return _SessionView(
        manifest_key=active_key,
        workspace_root=workspace_root,
        handle=None,
    )


def _release_handle(handle: Any | None) -> None:
    if handle is None:
        return
    release = getattr(handle, "release", None)
    if callable(release):
        release()
    run_dir = getattr(handle, "run_dir", None)
    if run_dir:
        shutil.rmtree(run_dir, ignore_errors=True)
