"""In-sandbox plugin op registry.

The :func:`register_plugin_op` decorator records ``(plugin_name, op_name,
handler)`` triples at module import time. :func:`flush_plugin_registrations`
hands them off to the daemon dispatcher under the public op name
``plugin.<plugin>.<op>``.

The decorator enforces the namespace rule from
``docs/architecture/plugins-refactor.md`` §2: a module that calls
``register_plugin_op('lsp', 'hover')`` MUST be importable as
``plugins.catalog.lsp.runtime.<something>``. The check walks live frames
directly so wrapper functions cannot hide a caller outside the plugin
namespace.
"""

from __future__ import annotations

import inspect
import re
from collections.abc import Awaitable, Callable, Iterable
from dataclasses import dataclass
from typing import Any

from sandbox.plugin.op_context import PluginOpContext

__all__ = [
    "ContextFactory",
    "DispatcherHandler",
    "PluginOpConflictError",
    "PluginOpHandler",
    "PluginOpRegistrationError",
    "clear_plugin_registrations",
    "flush_plugin_registrations",
    "pending_plugin_registrations",
    "register_plugin_op",
]


PluginOpHandler = Callable[..., Awaitable[Any]]
DispatcherHandler = Callable[[dict[str, Any]], Awaitable[Any]]
ContextFactory = Callable[
    [dict[str, Any], str, str],
    Awaitable[PluginOpContext],
]
DispatchRunner = Callable[
    [PluginOpHandler, dict[str, Any], PluginOpContext, str, str],
    Awaitable[Any],
]


class PluginOpRegistrationError(RuntimeError):
    """Raised when register_plugin_op is invoked from a forbidden module."""


class PluginOpConflictError(RuntimeError):
    """Raised when two distinct handlers try to register the same op."""


@dataclass(frozen=True)
class _PendingRegistration:
    plugin_name: str
    op_name: str
    handler: PluginOpHandler
    auto_workspace_overlay: bool = True


_PENDING: dict[tuple[str, str], _PendingRegistration] = {}
_MAX_CALLER_FRAMES = 16
_PLUGIN_NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def register_plugin_op(
    plugin_name: str,
    op_name: str,
    *,
    auto_workspace_overlay: bool = True,
) -> Callable[[PluginOpHandler], PluginOpHandler]:
    """Decorator that records a plugin op handler.

    Identical re-registration (same plugin/op/handler) is a no-op. Conflicting
    registration with a different handler raises ``PluginOpConflictError``.
    By default, daemon dispatch runs handlers inside an automatic per-operation
    workspace overlay. Stateful runtimes that already manage their own overlay
    lifecycle can pass ``auto_workspace_overlay=False``.
    """
    plugin_name = (plugin_name or "").strip()
    op_name = (op_name or "").strip()
    if _PLUGIN_NAME_RE.fullmatch(plugin_name) is None or not op_name:
        raise PluginOpRegistrationError(
            "register_plugin_op requires a valid plugin_name and non-empty op_name"
        )
    _validate_plugin_caller(plugin_name, "register_plugin_op")

    def decorator(handler: PluginOpHandler) -> PluginOpHandler:
        key = (plugin_name, op_name)
        existing = _PENDING.get(key)
        if existing is not None:
            if existing.handler is handler:
                return handler
            raise PluginOpConflictError(
                f"plugin op {plugin_name!r}.{op_name!r} already has a "
                f"different handler registered"
            )
        _PENDING[key] = _PendingRegistration(
            plugin_name=plugin_name,
            op_name=op_name,
            handler=handler,
            auto_workspace_overlay=bool(auto_workspace_overlay),
        )
        return handler

    return decorator


def pending_plugin_registrations(
    plugin_name: str | None = None,
) -> tuple[_PendingRegistration, ...]:
    """Return pending registrations, filtered by plugin name when provided."""
    if plugin_name is None:
        return tuple(_PENDING.values())
    return tuple(
        entry
        for entry in _PENDING.values()
        if entry.plugin_name == plugin_name
    )


def clear_plugin_registrations(plugin_name: str) -> None:
    """Drop pending registrations for one plugin before a runtime reload."""
    plugin_name = (plugin_name or "").strip()
    for key in [
        key for key, entry in _PENDING.items()
        if entry.plugin_name == plugin_name
    ]:
        _PENDING.pop(key, None)


def flush_plugin_registrations(
    plugin_name: str,
    dispatcher_register_op: Callable[[str, DispatcherHandler], None],
    *,
    context_factory: ContextFactory | None = None,
    dispatch_runner: DispatchRunner | None = None,
    trusted_caller: bool = False,
) -> list[str]:
    """Flush pending registrations for *plugin_name* into the dispatcher.

    When ``context_factory`` is provided, each plugin handler is wrapped so
    the dispatcher receives a 1-argument coroutine (``args -> response``)
    while the underlying plugin handler is invoked as
    ``await handler(args, ctx)``. Without a factory, raw handlers are
    registered (used by tests that call handlers directly with mocked args).
    """
    plugin_name = (plugin_name or "").strip()
    if not plugin_name:
        raise PluginOpRegistrationError(
            "flush_plugin_registrations requires a non-empty plugin_name"
        )
    if _PLUGIN_NAME_RE.fullmatch(plugin_name) is None:
        raise PluginOpRegistrationError(
            "flush_plugin_registrations requires a valid plugin_name"
        )
    if not trusted_caller:
        _validate_plugin_caller(plugin_name, "flush_plugin_registrations")
    registered: list[str] = []
    for entry in _filter_pending(plugin_name):
        public_op = f"plugin.{entry.plugin_name}.{entry.op_name}"
        if context_factory is None:
            handler: DispatcherHandler = entry.handler
        else:
            handler = _wrap_with_context(
                entry.handler,
                context_factory=context_factory,
                plugin_name=entry.plugin_name,
                op_name=entry.op_name,
                dispatch_runner=dispatch_runner if entry.auto_workspace_overlay else None,
            )
        dispatcher_register_op(public_op, handler)
        registered.append(public_op)
        _PENDING.pop((entry.plugin_name, entry.op_name), None)
    return registered


def _wrap_with_context(
    plugin_handler: PluginOpHandler,
    *,
    context_factory: ContextFactory,
    plugin_name: str,
    op_name: str,
    dispatch_runner: DispatchRunner | None = None,
) -> DispatcherHandler:
    async def dispatcher_handler(args: dict[str, Any]) -> Any:
        ctx = await context_factory(args, plugin_name, op_name)
        if dispatch_runner is not None:
            return await dispatch_runner(
                plugin_handler,
                args,
                ctx,
                plugin_name,
                op_name,
            )
        return await plugin_handler(args, ctx)

    return dispatcher_handler


def _filter_pending(plugin_name: str) -> Iterable[_PendingRegistration]:
    return [
        entry
        for entry in _PENDING.values()
        if entry.plugin_name == plugin_name
    ]


def _validate_plugin_caller(plugin_name: str, operation: str) -> None:
    expected_module_prefix = f"plugins.catalog.{plugin_name}."
    here = __name__
    frame = inspect.currentframe()
    try:
        if frame is None:
            caller_module = ""
            raise _registration_error(operation, plugin_name, caller_module)
        for _ in range(_MAX_CALLER_FRAMES):
            frame = frame.f_back
            if frame is None:
                break
            mod_name = frame.f_globals.get("__name__", "")
            if not mod_name or mod_name == here:
                continue
            caller_module = str(mod_name)
            if caller_module.startswith(expected_module_prefix):
                return
            raise _registration_error(operation, plugin_name, caller_module)
        raise _registration_error(operation, plugin_name, "")
    finally:
        del frame


def _registration_error(
    operation: str,
    plugin_name: str,
    caller_module: str,
) -> PluginOpRegistrationError:
    expected_module_prefix = f"plugins.catalog.{plugin_name}."
    return PluginOpRegistrationError(
        f"{operation}({plugin_name!r}) called from {caller_module!r}; "
        f"only modules under {expected_module_prefix}* may register or flush "
        "ops for this plugin"
    )
