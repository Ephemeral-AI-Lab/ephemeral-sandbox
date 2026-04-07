"""Unit tests for CoordinationWorkerToolkit (__init__.py)."""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from tools.coordination_worker import CoordinationWorkerToolkit
from tools.coordination_worker.replan_tool import ArtifactStore, ReplanHandler


class TestCoordinationWorkerToolkitInit:
    def test_instantiates_with_no_args(self) -> None:
        tk = CoordinationWorkerToolkit()
        assert tk is not None

    def test_name_is_coordination_worker(self) -> None:
        tk = CoordinationWorkerToolkit()
        assert tk.name == "coordination_worker"

    def test_description_is_set(self) -> None:
        tk = CoordinationWorkerToolkit()
        assert tk.description

    def test_instructions_is_set(self) -> None:
        tk = CoordinationWorkerToolkit()
        assert tk.instructions

    def test_registers_one_tool(self) -> None:
        tk = CoordinationWorkerToolkit()
        assert len(tk.list_tools()) == 1

    def test_registers_request_replan_tool(self) -> None:
        tk = CoordinationWorkerToolkit()
        assert tk.get("request_replan") is not None

    def test_tool_names_contains_request_replan(self) -> None:
        tk = CoordinationWorkerToolkit()
        assert "request_replan" in tk.tool_names()

    def test_task_id_forwarded(self) -> None:
        tk = CoordinationWorkerToolkit(task_id="task_99")
        # Tool is registered regardless
        assert tk.get("request_replan") is not None

    def test_run_id_forwarded(self) -> None:
        tk = CoordinationWorkerToolkit(run_id="run_42")
        assert tk.get("request_replan") is not None

    def test_store_forwarded(self) -> None:
        store = MagicMock(spec=ArtifactStore)
        tk = CoordinationWorkerToolkit(store=store)
        assert tk.get("request_replan") is not None

    def test_replan_handler_forwarded(self) -> None:
        handler = MagicMock(spec=ReplanHandler)
        tk = CoordinationWorkerToolkit(replan_handler=handler)
        assert tk.get("request_replan") is not None

    def test_trigger_dispatch_fn_forwarded(self) -> None:
        dispatch = MagicMock()
        tk = CoordinationWorkerToolkit(trigger_dispatch_fn=dispatch)
        assert tk.get("request_replan") is not None

    def test_all_exported(self) -> None:
        from tools.coordination_worker import __all__
        assert "CoordinationWorkerToolkit" in __all__

    def test_instructions_mention_request_replan(self) -> None:
        tk = CoordinationWorkerToolkit()
        assert "request_replan" in tk.instructions
