from __future__ import annotations

import asyncio
from types import SimpleNamespace

from engine.testing.eval_agent import EvalAgent
from message.stream_events import BackgroundTaskCompleted, SystemNotification


class _DummyClient:
    async def aclose(self) -> None:
        return None


async def _fake_event_iter(events):
    for event in events:
        yield event, None


def test_eval_agent_verbose_logging_keeps_full_background_and_system_messages(
    monkeypatch,
    capsys,
) -> None:
    long_system = "system-note-" * 50
    long_background = '{"summary":"' + ("background-output-" * 40) + '"}'

    async def _fake_run_query(_query_context, messages):
        return messages, _fake_event_iter(
            [
                SystemNotification(
                    text=long_system,
                    category="background_progress",
                    agent_name="analysis_agent",
                    work_id="wid-1",
                ),
                BackgroundTaskCompleted(
                    task_id="bg_1",
                    tool_name="run_subagent",
                    output=long_background,
                    agent_name="analysis_agent",
                    work_id="wid-1",
                ),
            ]
        )

    monkeypatch.setattr("engine.testing.eval_agent.run_query", _fake_run_query)
    monkeypatch.setattr("engine.core.query.run_query", _fake_run_query)

    query_context = SimpleNamespace(
        tool_metadata=None,
        agent_name="eval_agent",
        run_id="",
    )
    ephemeral_agent = SimpleNamespace(query_context=query_context)
    agent = EvalAgent(
        ephemeral_agent=ephemeral_agent,
        settings=SimpleNamespace(),
        model="test-model",
        api_client=_DummyClient(),
        runtime_config=SimpleNamespace(),
    )

    asyncio.run(agent.invoke("benchmark prompt", verbose=True))

    out = capsys.readouterr().out
    assert f"    [system] {long_system}" in out
    assert f"    << bg_done:    run_subagent {long_background}" in out
    assert "..." not in next(
        line for line in out.splitlines() if line.startswith("    [system] ")
    )
