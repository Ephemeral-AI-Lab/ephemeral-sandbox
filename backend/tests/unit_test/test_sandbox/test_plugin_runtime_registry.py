"""Unit tests for sandbox.plugin.op_registry."""

from __future__ import annotations

import asyncio
from collections.abc import Iterator
from types import SimpleNamespace

import pytest

from sandbox.plugin import op_registry as registry_mod
from sandbox.plugin.op_registry import (
    PluginOpConflictError,
    PluginOpRegistrationError,
    flush_plugin_registrations,
    pending_plugin_registrations,
    register_plugin_op,
)


@pytest.fixture(autouse=True)
def _clear_pending() -> Iterator[None]:
    registry_mod._PENDING.clear()
    yield
    registry_mod._PENDING.clear()


def _exec_in_plugin_namespace(plugin_name: str, code: str) -> dict[str, object]:
    """Execute *code* with __name__ set to a plugin runtime module name.

    register_plugin_op uses inspect.stack to read the caller frame's
    __name__; exec() lets us simulate any module name without writing temp
    files to disk.
    """
    namespace: dict[str, object] = {
        "__name__": f"plugins.catalog.{plugin_name}.runtime.synthetic_module",
        "register_plugin_op": register_plugin_op,
    }
    exec(code, namespace)
    return namespace


def test_register_and_flush_happy_path() -> None:
    namespace = _exec_in_plugin_namespace(
        "demo",
        """
async def hover_handler(args):
    return {"ok": True, "args": args}

decorated = register_plugin_op("demo", "hover")(hover_handler)
        """.strip(),
    )

    pending = pending_plugin_registrations("demo")
    assert len(pending) == 1
    assert pending[0].plugin_name == "demo"
    assert pending[0].op_name == "hover"
    assert pending[0].handler is namespace["hover_handler"]

    registered: dict[str, object] = {}

    def fake_dispatcher(op: str, handler: object) -> None:
        registered[op] = handler

    keys = flush_plugin_registrations(
        "demo",
        fake_dispatcher,
        trusted_caller=True,
    )
    assert keys == ["plugin.demo.hover"]
    assert registered == {"plugin.demo.hover": namespace["hover_handler"]}
    assert pending_plugin_registrations("demo") == ()


def test_namespace_mismatch_rejected_loudly() -> None:
    with pytest.raises(
        PluginOpRegistrationError, match="only modules under"
    ):
        # Called from this test module — __name__ is the test, not a plugin.
        register_plugin_op("demo", "hover")


def test_identical_re_registration_is_silent_noop() -> None:
    namespace = _exec_in_plugin_namespace(
        "demo",
        """
async def handler(args):
    return {}

register_plugin_op("demo", "hover")(handler)
register_plugin_op("demo", "hover")(handler)
        """.strip(),
    )
    assert len(pending_plugin_registrations("demo")) == 1
    assert namespace["handler"] is pending_plugin_registrations("demo")[0].handler


def test_conflicting_handler_under_same_op_raises() -> None:
    with pytest.raises(PluginOpConflictError, match="already has a different"):
        _exec_in_plugin_namespace(
            "demo",
            """
async def first(args):
    return {}

async def second(args):
    return {}

register_plugin_op("demo", "hover")(first)
register_plugin_op("demo", "hover")(second)
            """.strip(),
        )


def test_flush_only_targets_named_plugin() -> None:
    _exec_in_plugin_namespace(
        "alpha",
        """
async def alpha_handler(args):
    return {}

register_plugin_op("alpha", "ping")(alpha_handler)
        """.strip(),
    )
    _exec_in_plugin_namespace(
        "beta",
        """
async def beta_handler(args):
    return {}

register_plugin_op("beta", "ping")(beta_handler)
        """.strip(),
    )

    seen: list[str] = []
    flush_plugin_registrations(
        "alpha",
        lambda op, _h: seen.append(op),
        trusted_caller=True,
    )
    assert seen == ["plugin.alpha.ping"]
    # beta still pending
    assert any(
        entry.plugin_name == "beta"
        for entry in pending_plugin_registrations()
    )


def test_register_requires_non_empty_names() -> None:
    with pytest.raises(PluginOpRegistrationError, match="non-empty"):
        register_plugin_op("", "hover")
    with pytest.raises(PluginOpRegistrationError, match="non-empty"):
        register_plugin_op("demo", "")


def test_untrusted_flush_requires_plugin_namespace() -> None:
    _exec_in_plugin_namespace(
        "demo",
        """
async def handler(args):
    return {}

register_plugin_op("demo", "hover")(handler)
        """.strip(),
    )

    with pytest.raises(PluginOpRegistrationError, match="only modules under"):
        flush_plugin_registrations("demo", lambda _op, _h: None)


def test_context_wrapper_uses_dispatch_runner_for_auto_overlay_ops() -> None:
    _exec_in_plugin_namespace(
        "demo",
        """
async def handler(args, ctx):
    return {"ctx": ctx.marker, "args": args}

register_plugin_op("demo", "run")(handler)
        """.strip(),
    )
    registered: dict[str, object] = {}
    calls: list[tuple[str, str]] = []

    async def context_factory(args, plugin_name, op_name):
        del args, plugin_name, op_name
        return SimpleNamespace(marker="ctx")

    async def dispatch_runner(plugin_handler, args, ctx, plugin_name, op_name):
        calls.append((plugin_name, op_name))
        return await plugin_handler(args, ctx)

    flush_plugin_registrations(
        "demo",
        lambda op, handler: registered.setdefault(op, handler),
        context_factory=context_factory,
        dispatch_runner=dispatch_runner,
        trusted_caller=True,
    )

    result = asyncio.run(registered["plugin.demo.run"]({"value": 1}))

    assert result == {"ctx": "ctx", "args": {"value": 1}}
    assert calls == [("demo", "run")]


def test_context_wrapper_skips_dispatch_runner_when_op_opts_out() -> None:
    _exec_in_plugin_namespace(
        "demo",
        """
async def handler(args, ctx):
    return {"ctx": ctx.marker, "args": args}

register_plugin_op("demo", "run", auto_workspace_overlay=False)(handler)
        """.strip(),
    )
    registered: dict[str, object] = {}
    calls: list[tuple[str, str]] = []

    async def context_factory(args, plugin_name, op_name):
        del args, plugin_name, op_name
        return SimpleNamespace(marker="ctx")

    async def dispatch_runner(plugin_handler, args, ctx, plugin_name, op_name):
        del plugin_handler, args, ctx
        calls.append((plugin_name, op_name))
        return {}

    flush_plugin_registrations(
        "demo",
        lambda op, handler: registered.setdefault(op, handler),
        context_factory=context_factory,
        dispatch_runner=dispatch_runner,
        trusted_caller=True,
    )

    result = asyncio.run(registered["plugin.demo.run"]({"value": 1}))

    assert result == {"ctx": "ctx", "args": {"value": 1}}
    assert calls == []
