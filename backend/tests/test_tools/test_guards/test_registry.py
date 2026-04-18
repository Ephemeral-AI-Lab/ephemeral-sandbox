"""Unit tests for the tool-guard registry."""

from __future__ import annotations

from pydantic import BaseModel

from tools.core.guards import Allow, ToolGuardRegistry


class _Args(BaseModel):
    value: str = ""


async def _noop(tool_name: str, args: BaseModel, context):  # pragma: no cover - stub
    return Allow()


def test_register_and_match_by_exact_name():
    reg = ToolGuardRegistry()
    reg.register("daytona_write_file", "pre", 10, _noop, name="g1")

    assert [e.name for e in reg.matching("daytona_write_file", "pre")] == ["g1"]
    assert reg.matching("daytona_write_file", "post") == []
    assert reg.matching("other_tool", "pre") == []


def test_register_matches_glob():
    reg = ToolGuardRegistry()
    reg.register("daytona_*", "pre", 10, _noop, name="glob")

    assert [e.name for e in reg.matching("daytona_edit_file", "pre")] == ["glob"]
    assert [e.name for e in reg.matching("daytona_write_file", "pre")] == ["glob"]
    assert reg.matching("submit_plan", "pre") == []


def test_register_orders_by_priority_then_name():
    reg = ToolGuardRegistry()
    reg.register("*", "pre", 20, _noop, name="b")
    reg.register("*", "pre", 10, _noop, name="a")
    reg.register("*", "pre", 10, _noop, name="c")

    assert [e.name for e in reg.matching("anything", "pre")] == ["a", "c", "b"]


def test_separate_pre_and_post_buckets():
    reg = ToolGuardRegistry()
    reg.register("foo", "pre", 1, _noop, name="pre1")
    reg.register("foo", "post", 1, _noop, name="post1")

    assert [e.name for e in reg.matching("foo", "pre")] == ["pre1"]
    assert [e.name for e in reg.matching("foo", "post")] == ["post1"]


def test_clear_empties_both_buckets():
    reg = ToolGuardRegistry()
    reg.register("foo", "pre", 1, _noop, name="pre1")
    reg.register("foo", "post", 1, _noop, name="post1")
    reg.clear()

    assert reg.matching("foo", "pre") == []
    assert reg.matching("foo", "post") == []
