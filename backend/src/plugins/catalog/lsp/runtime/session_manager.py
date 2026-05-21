"""Layer-stack-root keyed cache of Pyright sessions rooted at leased overlays."""

from __future__ import annotations

import asyncio
import logging
from typing import Any

from sandbox.execution.overlay.capability import new_mount_api_supported
from sandbox.execution.strategies.namespace import detect_private_mount_namespace

from plugins.catalog.lsp.runtime.pyright_session import PyrightSession

__all__ = ["get_session", "evict_all", "evict_for_root"]


logger = logging.getLogger(__name__)


_sessions: dict[str, PyrightSession] = {}
_locks: dict[str, asyncio.Lock] = {}
_event_tasks: dict[str, asyncio.Task[None]] = {}
_event_subscriptions: dict[str, tuple[Any, str]] = {}


async def get_session(ctx: Any) -> PyrightSession:
    """Return a Pyright session reconciled to the active layer-stack snapshot."""
    layer_stack_root = str(ctx.layer_stack_root)
    lock = _locks.setdefault(layer_stack_root, asyncio.Lock())
    async with lock:
        active_key = await _active_manifest_key(ctx)
        cached = _sessions.get(layer_stack_root)
        if cached is not None and cached.manifest_key != active_key:
            if _session_owns_overlay_handle(cached):
                logger.info("pyright session snapshot changed; restarting")
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
        event_bus = getattr(overlay, "event_bus", None)
        unsubscribe = getattr(event_bus, "unsubscribe", None)
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
        return projection.active_manifest_key()
    overlay = getattr(ctx, "overlay", None)
    if overlay is not None and hasattr(overlay, "ensure_current"):
        metadata = getattr(ctx, "metadata", None) or {}
        op_name = str(metadata.get("op_name", "tool"))
        return await overlay.ensure_current(reason=f"lsp:{op_name}:enter")
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
    declared_workspace_root = _declared_workspace_root(ctx)
    acquire_operation_overlay = getattr(
        getattr(ctx, "overlay", None),
        "acquire_operation_overlay",
        None,
    )
    if callable(acquire_operation_overlay):
        metadata = getattr(ctx, "metadata", None) or {}
        op_name = str(metadata.get("op_name", "lsp"))
        use_namespace = _overlay_namespace_available()
        handle = acquire_operation_overlay(
            request_id=f"lsp-session:{op_name}",
            workspace_root=declared_workspace_root,
            materialize=not use_namespace,
        )
        if use_namespace and getattr(handle, "layer_paths", None):
            return _SessionView(
                manifest_key=handle.manifest_key,
                workspace_root=declared_workspace_root,
                handle=handle,
            )
        lowerdir = getattr(handle, "lowerdir", None)
        if lowerdir:
            return _SessionView(
                manifest_key=handle.manifest_key,
                workspace_root=str(lowerdir),
                handle=handle,
            )
        release = getattr(handle, "release", None)
        if callable(release):
            release()

    projection = getattr(ctx, "projection", None)
    acquire_overlay = getattr(projection, "acquire_overlay", None)
    if callable(acquire_overlay):
        metadata = getattr(ctx, "metadata", None) or {}
        op_name = str(metadata.get("op_name", "lsp"))
        use_namespace = _overlay_namespace_available()
        handle = acquire_overlay(
            f"lsp-session:{op_name}",
            workspace_root=declared_workspace_root,
            materialize=not use_namespace,
        )
        if use_namespace and getattr(handle, "layer_paths", None):
            return _SessionView(
                manifest_key=handle.manifest_key,
                workspace_root=declared_workspace_root,
                handle=handle,
            )
        lowerdir = getattr(handle, "lowerdir", None)
        if lowerdir:
            return _SessionView(
                manifest_key=handle.manifest_key,
                workspace_root=str(lowerdir),
                handle=handle,
            )
        release = getattr(handle, "release", None)
        if callable(release):
            release()

    acquire = getattr(projection, "acquire", None)
    if callable(acquire):
        handle = acquire("lsp-session", materialize=True)
        lowerdir = getattr(handle, "lowerdir", None)
        if lowerdir:
            return _SessionView(
                manifest_key=handle.manifest_key,
                workspace_root=str(lowerdir),
                handle=handle,
            )
        release = getattr(handle, "release", None)
        if callable(release):
            release()

    return _SessionView(
        manifest_key=active_key,
        workspace_root=declared_workspace_root,
        handle=None,
    )


def _declared_workspace_root(ctx: Any) -> str:
    overlay = getattr(ctx, "overlay", None)
    metadata = getattr(ctx, "metadata", None) or {}
    return str(
        metadata.get("workspace_root")
        or getattr(overlay, "workspace_root", "")
        or "/testbed"
    )


def _overlay_namespace_available() -> bool:
    return new_mount_api_supported() and detect_private_mount_namespace()


def _session_owns_overlay_handle(session: Any) -> bool:
    return getattr(session, "_overlay_handle", None) is not None


def _ensure_event_subscription(
    layer_stack_root: str,
    overlay: Any,
    session: PyrightSession,
) -> None:
    event_bus = getattr(overlay, "event_bus", None)
    subscribe = getattr(event_bus, "subscribe", None)
    if not callable(subscribe):
        return
    existing = _event_subscriptions.get(layer_stack_root)
    if existing is not None and existing[0] is overlay:
        return
    if existing is not None:
        existing_bus = getattr(existing[0], "event_bus", None)
        unsubscribe = getattr(existing_bus, "unsubscribe", None)
        if callable(unsubscribe):
            unsubscribe(existing[1])
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
        cached = _sessions.get(layer_stack_root)
        if cached is not session:
            return
        active_key = (
            overlay.active_manifest_key()
            if hasattr(overlay, "active_manifest_key")
            else session.manifest_key
        )
        if active_key != session.manifest_key:
            await session.evict()
            _sessions.pop(layer_stack_root, None)
            return
        if getattr(event, "changes", ()):
            await session.refresh_manifest(manifest_key=active_key)
