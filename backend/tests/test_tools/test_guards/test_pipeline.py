"""Unit tests for the tool-guard pipeline."""

from __future__ import annotations

from pathlib import Path

import pytest
from pydantic import BaseModel

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.guards import (
    Advisory,
    Allow,
    Deny,
    MutateArgs,
    ToolGuardRegistry,
    run_post,
    run_pre,
)

pytestmark = pytest.mark.asyncio


class _Args(BaseModel):
    value: str = ""


def _context() -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"))


async def test_empty_registry_is_noop():
    reg = ToolGuardRegistry()
    args = _Args(value="v")
    ctx = _context()

    pre = await run_pre("tool", args, ctx, registry=reg)
    post = await run_post("tool", args, ctx, ToolResult(output="ok"), registry=reg)

    assert pre.deny is None
    assert pre.args is args
    assert pre.warnings == []
    assert post.warnings == []


async def test_pre_advisory_accumulates_warnings():
    reg = ToolGuardRegistry()

    async def adv_a(tool_name, args, context):
        return Advisory(warnings=("a-warn",), category="scope")

    def adv_b(tool_name, args, context):  # sync guards allowed
        return Advisory(warnings=("b-warn",))

    reg.register("*", "pre", 10, adv_a, name="a")
    reg.register("*", "pre", 20, adv_b, name="b")

    pre = await run_pre("tool", _Args(), _context(), registry=reg)

    assert pre.deny is None
    assert pre.warnings == ["a-warn", "b-warn"]


async def test_pre_mutate_threads_new_args():
    reg = ToolGuardRegistry()
    seen: list[str] = []

    async def first(tool_name, args, context):
        return MutateArgs(new_args=_Args(value="mutated"), warnings=("m-warn",))

    async def second(tool_name, args, context):
        seen.append(args.value)
        return Allow()

    reg.register("*", "pre", 10, first, name="first")
    reg.register("*", "pre", 20, second, name="second")

    pre = await run_pre("tool", _Args(value="orig"), _context(), registry=reg)

    assert pre.deny is None
    assert pre.args.value == "mutated"
    assert seen == ["mutated"]
    assert pre.warnings == ["m-warn"]


async def test_pre_deny_short_circuits():
    reg = ToolGuardRegistry()
    called_after = False

    async def blocker(tool_name, args, context):
        return Deny(message="blocked")

    async def later(tool_name, args, context):
        nonlocal called_after
        called_after = True
        return Allow()

    reg.register("*", "pre", 10, blocker, name="blocker")
    reg.register("*", "pre", 20, later, name="later")

    pre = await run_pre("tool", _Args(), _context(), registry=reg)

    assert pre.deny is not None
    assert pre.deny.message == "blocked"
    assert pre.deny.is_error is True
    assert called_after is False


async def test_pre_deny_keeps_warnings_collected_so_far():
    reg = ToolGuardRegistry()

    async def adv(tool_name, args, context):
        return Advisory(warnings=("w1",))

    async def blocker(tool_name, args, context):
        return Deny(message="no")

    reg.register("*", "pre", 10, adv, name="adv")
    reg.register("*", "pre", 20, blocker, name="blocker")

    pre = await run_pre("tool", _Args(), _context(), registry=reg)

    assert pre.deny is not None
    assert pre.warnings == ["w1"]


async def test_post_advisory_accumulates_warnings():
    reg = ToolGuardRegistry()

    async def p1(tool_name, args, context, result):
        return Advisory(warnings=("p1",))

    async def p2(tool_name, args, context, result):
        return Advisory(warnings=("p2a", "p2b"))

    reg.register("*", "post", 10, p1, name="p1")
    reg.register("*", "post", 20, p2, name="p2")

    post = await run_post("tool", _Args(), _context(), ToolResult(output="ok"), registry=reg)

    assert post.warnings == ["p1", "p2a", "p2b"]


async def test_post_ignores_non_advisory_outcomes():
    reg = ToolGuardRegistry()

    async def tries_deny(tool_name, args, context, result):
        return Deny(message="ignored in post")

    async def tries_mutate(tool_name, args, context, result):
        return MutateArgs(new_args=_Args(value="x"))

    async def final_adv(tool_name, args, context, result):
        return Advisory(warnings=("kept",))

    reg.register("*", "post", 10, tries_deny, name="deny")
    reg.register("*", "post", 20, tries_mutate, name="mutate")
    reg.register("*", "post", 30, final_adv, name="adv")

    post = await run_post("tool", _Args(), _context(), ToolResult(output="ok"), registry=reg)

    assert post.warnings == ["kept"]


async def test_pre_sees_only_matching_globs():
    reg = ToolGuardRegistry()
    called: list[str] = []

    async def daytona_only(tool_name, args, context):
        called.append(tool_name)
        return Allow()

    reg.register("daytona_*", "pre", 10, daytona_only, name="scoped")

    await run_pre("submit_plan", _Args(), _context(), registry=reg)
    assert called == []

    await run_pre("daytona_write_file", _Args(), _context(), registry=reg)
    assert called == ["daytona_write_file"]


async def test_pre_unknown_outcome_raises():
    reg = ToolGuardRegistry()

    async def broken(tool_name, args, context):
        return "not an outcome"  # type: ignore[return-value]

    reg.register("*", "pre", 10, broken, name="broken")

    with pytest.raises(TypeError):
        await run_pre("tool", _Args(), _context(), registry=reg)
