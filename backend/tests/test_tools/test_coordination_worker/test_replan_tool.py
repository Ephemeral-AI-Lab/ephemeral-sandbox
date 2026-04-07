"""Unit tests for coordination_worker replan_tool."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock, call

import pytest

from tools.coordination_worker.replan_tool import (
    ArtifactStore,
    ReplanHandler,
    make_request_replan_tool,
)
from tools.core.base import ToolExecutionContext


def _ctx() -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata={})


def _args(tool, **kwargs):
    defaults = dict(reason="blocker", context_detail="details here", suggestion="")
    defaults.update(kwargs)
    return tool.input_model(**defaults)


# ---------------------------------------------------------------------------
# Basic execution — no store, no handler
# ---------------------------------------------------------------------------


class TestRequestReplanBasic:
    async def test_returns_success_with_no_store_or_handler(self) -> None:
        tool = make_request_replan_tool()
        result = await tool.execute(_args(tool), _ctx())
        assert not result.is_error

    async def test_output_mentions_recorded(self) -> None:
        tool = make_request_replan_tool()
        result = await tool.execute(_args(tool), _ctx())
        assert "Replan request recorded" in result.output

    async def test_output_mentions_continue_work(self) -> None:
        tool = make_request_replan_tool()
        result = await tool.execute(_args(tool), _ctx())
        assert "Continue" in result.output

    async def test_no_spawned_mention_when_no_handler(self) -> None:
        tool = make_request_replan_tool()
        result = await tool.execute(_args(tool), _ctx())
        assert "Replanner task spawned" not in result.output


# ---------------------------------------------------------------------------
# ArtifactStore integration
# ---------------------------------------------------------------------------


class TestRequestReplanWithStore:
    async def test_calls_save_artifact_with_correct_args(self) -> None:
        store = MagicMock(spec=ArtifactStore)
        tool = make_request_replan_tool(
            task_id="task_42", run_id="run_7", store=store
        )
        result = await tool.execute(
            _args(tool, reason="broken", context_detail="test failed", suggestion="retry"),
            _ctx(),
        )
        assert not result.is_error
        store.save_artifact.assert_called_once()
        call_args = store.save_artifact.call_args
        assert call_args.args[0] == "run_7"   # run_id
        assert call_args.args[1] == "task_42"  # task_id
        artifact = call_args.kwargs["artifact"]
        assert "replan_request" in artifact

    async def test_artifact_contains_expected_fields(self) -> None:
        store = MagicMock(spec=ArtifactStore)
        tool = make_request_replan_tool(task_id="t1", run_id="r1", store=store)
        await tool.execute(
            _args(tool, reason="reason_x", context_detail="ctx_x", suggestion="hint_x"),
            _ctx(),
        )
        artifact = store.save_artifact.call_args.kwargs["artifact"]
        payload = artifact["replan_request"]
        assert payload["reason"] == "reason_x"
        assert payload["context"] == "ctx_x"
        assert payload["suggestion"] == "hint_x"
        assert payload["task_id"] == "t1"
        assert payload["run_id"] == "r1"
        assert payload["type"] == "replan_request"

    async def test_store_exception_does_not_propagate(self) -> None:
        store = MagicMock(spec=ArtifactStore)
        store.save_artifact.side_effect = RuntimeError("disk full")
        tool = make_request_replan_tool(store=store)
        result = await tool.execute(_args(tool), _ctx())
        # Should still succeed — exception is swallowed with a warning
        assert not result.is_error


# ---------------------------------------------------------------------------
# ReplanHandler integration
# ---------------------------------------------------------------------------


class TestRequestReplanWithHandler:
    async def test_calls_handle_replan_with_correct_args(self) -> None:
        handler = MagicMock(spec=ReplanHandler)
        handler.handle_replan.return_value = False
        tool = make_request_replan_tool(
            task_id="task_1", run_id="run_1", replan_handler=handler
        )
        await tool.execute(
            _args(tool, reason="r", context_detail="c", suggestion="s"),
            _ctx(),
        )
        handler.handle_replan.assert_called_once_with("task_1", "run_1", "r", "c", "s")

    async def test_spawned_message_when_handler_returns_true(self) -> None:
        handler = MagicMock(spec=ReplanHandler)
        handler.handle_replan.return_value = True
        tool = make_request_replan_tool(replan_handler=handler)
        result = await tool.execute(_args(tool), _ctx())
        assert "Replanner task spawned" in result.output

    async def test_no_spawned_message_when_handler_returns_false(self) -> None:
        handler = MagicMock(spec=ReplanHandler)
        handler.handle_replan.return_value = False
        tool = make_request_replan_tool(replan_handler=handler)
        result = await tool.execute(_args(tool), _ctx())
        assert "Replanner task spawned" not in result.output

    async def test_handler_exception_does_not_propagate(self) -> None:
        handler = MagicMock(spec=ReplanHandler)
        handler.handle_replan.side_effect = ValueError("handler crash")
        tool = make_request_replan_tool(replan_handler=handler)
        result = await tool.execute(_args(tool), _ctx())
        assert not result.is_error


# ---------------------------------------------------------------------------
# trigger_dispatch_fn integration
# ---------------------------------------------------------------------------


class TestRequestReplanDispatch:
    async def test_dispatch_called_when_handler_spawned(self) -> None:
        handler = MagicMock(spec=ReplanHandler)
        handler.handle_replan.return_value = True
        dispatch = MagicMock()
        tool = make_request_replan_tool(
            replan_handler=handler, trigger_dispatch_fn=dispatch
        )
        await tool.execute(_args(tool), _ctx())
        dispatch.assert_called_once()

    async def test_dispatch_not_called_when_handler_returned_false(self) -> None:
        handler = MagicMock(spec=ReplanHandler)
        handler.handle_replan.return_value = False
        dispatch = MagicMock()
        tool = make_request_replan_tool(
            replan_handler=handler, trigger_dispatch_fn=dispatch
        )
        await tool.execute(_args(tool), _ctx())
        dispatch.assert_not_called()

    async def test_dispatch_not_called_when_no_handler(self) -> None:
        dispatch = MagicMock()
        tool = make_request_replan_tool(trigger_dispatch_fn=dispatch)
        await tool.execute(_args(tool), _ctx())
        dispatch.assert_not_called()

    async def test_dispatch_exception_does_not_propagate(self) -> None:
        handler = MagicMock(spec=ReplanHandler)
        handler.handle_replan.return_value = True
        dispatch = MagicMock(side_effect=OSError("network"))
        tool = make_request_replan_tool(
            replan_handler=handler, trigger_dispatch_fn=dispatch
        )
        result = await tool.execute(_args(tool), _ctx())
        assert not result.is_error


# ---------------------------------------------------------------------------
# Closure captures task_id / run_id correctly
# ---------------------------------------------------------------------------


class TestRequestReplanClosureCapture:
    async def test_task_id_and_run_id_in_artifact(self) -> None:
        store = MagicMock(spec=ArtifactStore)
        tool = make_request_replan_tool(task_id="MY_TASK", run_id="MY_RUN", store=store)
        await tool.execute(_args(tool), _ctx())
        artifact = store.save_artifact.call_args.kwargs["artifact"]
        assert artifact["replan_request"]["task_id"] == "MY_TASK"
        assert artifact["replan_request"]["run_id"] == "MY_RUN"

    async def test_default_task_and_run_id_are_empty_strings(self) -> None:
        store = MagicMock(spec=ArtifactStore)
        tool = make_request_replan_tool(store=store)
        await tool.execute(_args(tool), _ctx())
        artifact = store.save_artifact.call_args.kwargs["artifact"]
        assert artifact["replan_request"]["task_id"] == ""
        assert artifact["replan_request"]["run_id"] == ""


# ---------------------------------------------------------------------------
# Tool metadata
# ---------------------------------------------------------------------------


class TestRequestReplanToolMetadata:
    def test_tool_name(self) -> None:
        tool = make_request_replan_tool()
        assert tool.name == "request_replan"

    def test_description_is_set(self) -> None:
        tool = make_request_replan_tool()
        assert tool.description

    def test_input_model_has_reason_field(self) -> None:
        tool = make_request_replan_tool()
        schema = tool.input_model.model_json_schema()
        assert "reason" in schema.get("properties", {})

    def test_input_model_has_context_detail_field(self) -> None:
        tool = make_request_replan_tool()
        schema = tool.input_model.model_json_schema()
        assert "context_detail" in schema.get("properties", {})

    def test_input_model_has_suggestion_field(self) -> None:
        tool = make_request_replan_tool()
        schema = tool.input_model.model_json_schema()
        assert "suggestion" in schema.get("properties", {})

    def test_reason_is_required(self) -> None:
        tool = make_request_replan_tool()
        required = tool.input_model.model_json_schema().get("required", [])
        assert "reason" in required

    def test_suggestion_has_default(self) -> None:
        # suggestion has a default of "" so it should not be required
        tool = make_request_replan_tool()
        args = tool.input_model(reason="r", context_detail="c")
        assert args.suggestion == ""
