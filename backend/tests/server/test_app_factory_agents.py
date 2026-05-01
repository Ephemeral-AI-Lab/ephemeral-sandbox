"""RuntimeState agent bootstrap tests."""

from __future__ import annotations

import pytest

from agents.registry import get_definition, register_definition, unregister_definition
from server.app_factory import RuntimeState
from server.protocol import BackendHostConfig


@pytest.mark.asyncio
async def test_runtime_state_registers_repository_harness_agents() -> None:
    previous = {name: get_definition(name) for name in ("planner", "executor", "verifier", "evaluator")}
    for name in previous:
        unregister_definition(name)

    try:
        runtime = RuntimeState()
        await runtime.initialize(BackendHostConfig())

        assert get_definition("planner") is not None
        executor = get_definition("executor")
        assert executor is not None
        assert executor.role == "executor"
        assert "request_complex_task_solution" in executor.terminals
    finally:
        for name, definition in previous.items():
            unregister_definition(name)
            if definition is not None:
                register_definition(definition)
