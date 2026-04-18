"""Integration tests wiring the guard pipeline into ``run_tool_safely``."""

from __future__ import annotations

from pathlib import Path

import pytest
from pydantic import BaseModel, RootModel

from tools.core.base import (
    BaseTool,
    ToolExecutionContext,
    ToolResult,
    run_tool_safely,
)
from tools.core.guards import Advisory, Allow, Deny, MutateArgs, default_registry

pytestmark = pytest.mark.asyncio


class _Args(BaseModel):
    value: str


class _Out(RootModel[str]):
    """Plain text output."""


class _EchoTool(BaseTool):
    name = "echo_tool"
    description = "Echoes the value argument."
    input_model = _Args
    output_model = _Out

    def __init__(self) -> None:
        self.seen: list[str] = []

    async def execute(self, arguments: _Args, context: ToolExecutionContext) -> ToolResult:
        self.seen.append(arguments.value)
        return ToolResult(output=arguments.value)


class _ExplodingTool(BaseTool):
    name = "exploding_tool"
    description = "Should not run."
    input_model = _Args
    output_model = _Out

    async def execute(self, arguments: _Args, context: ToolExecutionContext) -> ToolResult:
        raise AssertionError("execute() must not be called when a pre-guard denies")


@pytest.fixture(autouse=True)
def _clear_default_registry():
    default_registry().clear()
    yield
    default_registry().clear()


def _ctx() -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"))


async def test_empty_registry_leaves_behavior_unchanged():
    tool = _EchoTool()

    result = await run_tool_safely(tool, {"value": "hi"}, _ctx())

    assert result.is_error is False
    assert result.output == "hi"
    assert "guard_warnings" not in result.metadata
    assert tool.seen == ["hi"]


async def test_pre_deny_short_circuits_execution():
    tool = _ExplodingTool()

    async def block(tool_name, args, context):
        return Deny(message="not allowed")

    default_registry().register("exploding_tool", "pre", 10, block, name="block")

    result = await run_tool_safely(tool, {"value": "hi"}, _ctx())

    assert result.is_error is True
    assert result.output == "not allowed"


async def test_pre_mutate_feeds_tool_with_new_args():
    tool = _EchoTool()

    async def upper(tool_name, args, context):
        return MutateArgs(new_args=_Args(value=args.value.upper()), warnings=("upper",))

    default_registry().register("echo_tool", "pre", 10, upper, name="upper")

    result = await run_tool_safely(tool, {"value": "hi"}, _ctx())

    assert result.is_error is False
    assert result.output == "HI"
    assert tool.seen == ["HI"]
    assert result.metadata.get("guard_warnings") == ["upper"]


async def test_post_advisory_warnings_merge_into_result_metadata():
    tool = _EchoTool()

    async def observe(tool_name, args, context, result):
        return Advisory(warnings=("post-obs",))

    default_registry().register("echo_tool", "post", 10, observe, name="observe")

    result = await run_tool_safely(tool, {"value": "hi"}, _ctx())

    assert result.is_error is False
    assert result.metadata.get("guard_warnings") == ["post-obs"]


async def test_pre_and_post_warnings_combine():
    tool = _EchoTool()

    async def pre_adv(tool_name, args, context):
        return Advisory(warnings=("pre-w",))

    async def post_adv(tool_name, args, context, result):
        return Advisory(warnings=("post-w",))

    default_registry().register("echo_tool", "pre", 10, pre_adv, name="pre")
    default_registry().register("echo_tool", "post", 10, post_adv, name="post")

    result = await run_tool_safely(tool, {"value": "hi"}, _ctx())

    assert result.metadata.get("guard_warnings") == ["pre-w", "post-w"]


async def test_allow_outcome_adds_no_warnings():
    tool = _EchoTool()

    async def allow(tool_name, args, context):
        return Allow()

    default_registry().register("echo_tool", "pre", 10, allow, name="allow")

    result = await run_tool_safely(tool, {"value": "hi"}, _ctx())

    assert "guard_warnings" not in result.metadata


async def test_pre_deny_metadata_includes_prior_warnings():
    tool = _ExplodingTool()

    async def adv(tool_name, args, context):
        return Advisory(warnings=("w",))

    async def blocker(tool_name, args, context):
        return Deny(message="no")

    default_registry().register("exploding_tool", "pre", 10, adv, name="adv")
    default_registry().register("exploding_tool", "pre", 20, blocker, name="blocker")

    result = await run_tool_safely(tool, {"value": "hi"}, _ctx())

    assert result.is_error is True
    assert result.output == "no"
    assert result.metadata.get("guard_warnings") == ["w"]
