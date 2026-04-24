from __future__ import annotations

import asyncio
from types import SimpleNamespace

from engine.core.query import QueryExitReason
from team.runtime.agent_context import TeamAgentContext
from team.runtime.runner import TeamAgentRunner
from tools.core.runtime import ExecutionMetadata


def test_team_agent_runner_autofails_missing_terminal_submission(monkeypatch):
    run_prompts: list[str] = []

    class _Tracker:
        run_id = "agent-run-1"

    async def _fake_run(prompt: str):
        run_prompts.append(prompt)
        agent.query_context.exit_reason = QueryExitReason.TEXT_RESPONSE
        if False:
            yield None

    agent = SimpleNamespace(
        query_context=SimpleNamespace(
            tool_metadata=ExecutionMetadata(session_config="cfg", sandbox_id="sbx-1"),
            run_id="",
            session_state=None,
            exit_reason=None,
            terminal_tools=set(),
            on_turn=None,
        ),
        display_messages=[SimpleNamespace(role="assistant", text="Still working")],
        model="test-model",
        run=_fake_run,
    )

    monkeypatch.setattr(
        "team.runtime.runner.AgentRunTracker",
        SimpleNamespace(create=lambda **_: _Tracker()),
    )
    monkeypatch.setattr("team.runtime.runner.spawn_agent", lambda *_args, **_kwargs: agent)

    runner = TeamAgentRunner(
        session_config=SimpleNamespace(session_id="session-1"),
        sandbox_id="sbx-1",
    )
    tool_metadata = ExecutionMetadata(
        team_run_id="team-run-1",
        work_item_id="task-1",
    )
    tool_metadata["terminal_tools"] = {"submit_task_success"}
    ctx = TeamAgentContext(user_message="Do the task", tool_metadata=tool_metadata)

    result = asyncio.run(
        runner(
            SimpleNamespace(name="developer"),
            ctx,
        )
    )

    assert run_prompts == ["Do the task"]
    assert ctx.tool_metadata["task_summary_type"] == "request_replan"
    assert ctx.tool_metadata["task_summary"] == "Agent did not call a terminal submission tool."
    assert ctx.tool_metadata["work_result"] == "Still working"
    assert result["agent_run_id"] == "agent-run-1"


def test_team_agent_runner_injects_terminal_nudges_after_budget_exhaustion(monkeypatch):
    run_prompts: list[str] = []
    spawn_calls: list[dict[str, object]] = []

    class _Tracker:
        run_id = "agent-run-1"

    def _query_context() -> SimpleNamespace:
        return SimpleNamespace(
            tool_metadata=ExecutionMetadata(session_config="cfg", sandbox_id="sbx-1"),
            run_id="",
            session_state=None,
            exit_reason=None,
            terminal_tools=set(),
            on_turn=None,
            tool_call_limit=50,
            tool_calls_used=0,
            last_budget_warning_remaining=None,
            terminal_nudge_retries_used=0,
            terminal_nudge_budget_extended=False,
        )

    class _Agent:
        def __init__(self, label: str) -> None:
            self.label = label
            self.query_context = _query_context()
            self.display_messages: list[SimpleNamespace] = []
            self.model = "test-model"
            self.total_usage = SimpleNamespace(input_tokens=0, output_tokens=0)

        async def run(self, prompt: str):
            run_prompts.append(prompt)
            self.display_messages.append(SimpleNamespace(role="user", text=prompt))
            if self.label == "first":
                self.total_usage.input_tokens = 3
                self.total_usage.output_tokens = 4
                self.query_context.tool_calls_used = self.query_context.tool_call_limit
                self.query_context.exit_reason = QueryExitReason.RESOURCE_LIMIT
                self.display_messages.append(
                    SimpleNamespace(role="assistant", text="Still working first")
                )
            elif self.label == "second":
                self.total_usage.input_tokens = 5
                self.total_usage.output_tokens = 6
                self.query_context.tool_calls_used = self.query_context.tool_call_limit
                self.query_context.exit_reason = QueryExitReason.RESOURCE_LIMIT
                self.display_messages.append(
                    SimpleNamespace(role="assistant", text="Still working second")
                )
            else:
                self.total_usage.input_tokens = 7
                self.total_usage.output_tokens = 8
                self.query_context.tool_metadata["task_summary_type"] = "success"
                self.query_context.tool_metadata["task_summary"] = "Submitted after nudge"
                self.query_context.exit_reason = QueryExitReason.TOOL_STOP
                self.display_messages.append(SimpleNamespace(role="assistant", text="Submitted"))
            if False:
                yield None

    spawned_agents = [_Agent("first"), _Agent("second"), _Agent("third")]
    agents = list(spawned_agents)

    def _fake_spawn_agent(*_args, **kwargs):
        spawn_calls.append(dict(kwargs))
        next_agent = agents.pop(0)
        next_agent.display_messages = list(kwargs.get("messages") or [])
        return next_agent

    monkeypatch.setattr(
        "team.runtime.runner.AgentRunTracker",
        SimpleNamespace(create=lambda **_: _Tracker()),
    )
    monkeypatch.setattr("team.runtime.runner.spawn_agent", _fake_spawn_agent)

    runner = TeamAgentRunner(
        session_config=SimpleNamespace(session_id="session-1"),
        sandbox_id="sbx-1",
    )
    tool_metadata = ExecutionMetadata()
    tool_metadata["terminal_tools"] = {"submit_task_success"}
    ctx = TeamAgentContext(user_message="Do the task", tool_metadata=tool_metadata)

    result = asyncio.run(
        runner(
            SimpleNamespace(name="developer"),
            ctx,
        )
    )

    assert run_prompts[0] == "Do the task"
    assert run_prompts[1].startswith("[terminal-tool reminder]")
    assert "submit_task_success" in run_prompts[1]
    assert run_prompts[2].startswith("[terminal-tool reminder]")
    assert "nudge 2/" in run_prompts[2]
    assert len(spawn_calls) == 3
    assert spawn_calls[1]["messages"][-1].text == "Still working first"
    assert spawn_calls[2]["messages"][-1].text == "Still working second"
    assert ctx.tool_metadata["task_summary_type"] == "success"
    assert ctx.tool_metadata["task_summary"] == "Submitted after nudge"
    assert ctx.tool_metadata["work_result"] == "Submitted"
    assert spawned_agents[2].total_usage.input_tokens == 15
    assert spawned_agents[2].total_usage.output_tokens == 18
    assert result["agent_run_id"] == "agent-run-1"
