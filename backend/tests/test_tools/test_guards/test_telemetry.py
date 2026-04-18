"""Tests for guard-pipeline structured logging."""

from __future__ import annotations

import logging
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


async def test_pre_allow_logs_debug(caplog):
    reg = ToolGuardRegistry()

    async def allow(tool_name, args, context):
        return Allow()

    reg.register("*", "pre", 10, allow, name="allow_guard")

    with caplog.at_level(logging.DEBUG, logger="tools.core.guards.pipeline"):
        await run_pre("tool", _Args(), _context(), registry=reg)

    debug_records = [r for r in caplog.records if r.levelno == logging.DEBUG]
    assert any(
        getattr(r, "guard", "") == "allow_guard"
        and getattr(r, "outcome", "") == "Allow"
        and getattr(r, "phase", "") == "pre"
        for r in debug_records
    )


async def test_pre_deny_logs_info(caplog):
    reg = ToolGuardRegistry()

    async def blocker(tool_name, args, context):
        return Deny(message="no")

    reg.register("*", "pre", 10, blocker, name="blocker")

    with caplog.at_level(logging.INFO, logger="tools.core.guards.pipeline"):
        await run_pre("tool", _Args(), _context(), registry=reg)

    info_records = [r for r in caplog.records if r.levelno == logging.INFO]
    assert any(
        getattr(r, "outcome", "") == "Deny"
        and getattr(r, "deny_message", "") == "no"
        and getattr(r, "guard", "") == "blocker"
        for r in info_records
    )


async def test_pre_mutate_logs_info(caplog):
    reg = ToolGuardRegistry()

    async def mutator(tool_name, args, context):
        return MutateArgs(new_args=_Args(value="x"), warnings=("w",))

    reg.register("*", "pre", 10, mutator, name="mutator")

    with caplog.at_level(logging.INFO, logger="tools.core.guards.pipeline"):
        await run_pre("tool", _Args(), _context(), registry=reg)

    info_records = [r for r in caplog.records if r.levelno == logging.INFO]
    assert any(
        getattr(r, "outcome", "") == "MutateArgs"
        and getattr(r, "guard", "") == "mutator"
        and getattr(r, "warning_count", 0) == 1
        for r in info_records
    )


async def test_pre_advisory_logs_only_debug(caplog):
    reg = ToolGuardRegistry()

    async def adv(tool_name, args, context):
        return Advisory(warnings=("w",))

    reg.register("*", "pre", 10, adv, name="adv_guard")

    with caplog.at_level(logging.DEBUG, logger="tools.core.guards.pipeline"):
        await run_pre("tool", _Args(), _context(), registry=reg)

    # Advisory does not emit an INFO record — only the invocation DEBUG.
    info_records = [
        r for r in caplog.records
        if r.levelno == logging.INFO and getattr(r, "guard", "") == "adv_guard"
    ]
    assert info_records == []

    debug_records = [
        r for r in caplog.records
        if r.levelno == logging.DEBUG and getattr(r, "guard", "") == "adv_guard"
    ]
    assert debug_records, "expected a DEBUG invocation record for Advisory"


async def test_post_advisory_logs_debug(caplog):
    reg = ToolGuardRegistry()

    async def adv(tool_name, args, context, result):
        return Advisory(warnings=("pw",))

    reg.register("*", "post", 10, adv, name="post_adv")

    with caplog.at_level(logging.DEBUG, logger="tools.core.guards.pipeline"):
        await run_post("tool", _Args(), _context(), ToolResult(output="ok"), registry=reg)

    debug_records = [
        r for r in caplog.records
        if r.levelno == logging.DEBUG and getattr(r, "guard", "") == "post_adv"
    ]
    assert debug_records
    assert any(getattr(r, "phase", "") == "post" for r in debug_records)
