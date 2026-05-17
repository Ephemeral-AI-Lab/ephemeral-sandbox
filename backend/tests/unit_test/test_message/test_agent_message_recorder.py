from __future__ import annotations

import json

from message.agent_message_recorder import (
    AgentMessageJsonlRecorder,
    clear_recorder_for_agent_run,
    recorder_for_agent_run,
    register_recorder_for_agent_run,
)
from message.messages import (
    ConversationMessage,
    TextBlock,
    ThinkingBlock,
    ToolUseBlock,
)
from message.stream_events import (
    AssistantMessageComplete,
    AssistantTextDelta,
    ThinkingDelta,
    ToolExecutionCompleted,
)
from providers.types import UsageSnapshot


def _read_jsonl(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def test_agent_message_recorder_appends_conversation_messages(tmp_path) -> None:
    path = tmp_path / "message.jsonl"
    recorder = AgentMessageJsonlRecorder(
        path,
        base_event={"benchmark": "sweevo", "instance_id": "demo"},
    )

    recorder.record_initial_messages(
        system_prompt="system",
        user_prompt="user",
        agent_name="executor",
        run_id="t1",
    )
    recorder.emit(
        ThinkingDelta(text="inspect ", agent_name="executor", run_id="t1")
    )
    recorder.emit(ThinkingDelta(text="repo", agent_name="executor", run_id="t1"))
    recorder.emit(
        AssistantTextDelta(
            text="I will run ", agent_name="executor", run_id="t1"
        )
    )
    recorder.emit(
        AssistantTextDelta(text="tests.", agent_name="executor", run_id="t1")
    )
    recorder.emit(
        AssistantMessageComplete(
            message=ConversationMessage(
                role="assistant",
                content=[
                    ToolUseBlock(
                        id="toolu_1",
                        name="shell",
                        input={"cmd": "pytest -q"},
                    )
                ],
            ),
            usage=UsageSnapshot(input_tokens=1, output_tokens=2),
            agent_name="executor",
            run_id="t1",
        )
    )
    recorder.emit(
        ToolExecutionCompleted(
            tool_name="shell",
            output="ok",
            tool_id="toolu_1",
            agent_name="executor",
            run_id="t1",
        )
    )
    recorder.flush()

    records = _read_jsonl(path)
    assert [record["role"] for record in records] == [
        "system",
        "user",
        "assistant",
        "assistant",
        "assistant",
        "user",
    ]
    assert all("step_type" not in record for record in records)
    assert all(record.get("event") != "agent_step" for record in records)
    assert records[2]["content"] == [{"type": "thinking", "text": "inspect repo"}]
    assert records[3]["content"] == [{"type": "text", "text": "I will run tests."}]
    assert records[4]["content"] == [
        {
            "type": "tool_use",
            "id": "toolu_1",
            "name": "shell",
            "input": {"cmd": "pytest -q"},
        }
    ]
    assert records[5]["content"][0]["tool_use_id"] == "toolu_1"
    assert records[5]["content"][0]["content"] == "ok"
    assert all(
        record["metadata"]["benchmark"] == "sweevo" for record in records
    )
    assert all(
        record["metadata"]["agent_name"] == "executor" for record in records
    )


def test_assistant_complete_with_full_blocks_does_not_duplicate(tmp_path) -> None:
    """Real-LLM path: AssistantMessageComplete carries the same thinking/text
    blocks that arrived as deltas. The buffer must be discarded, not flushed,
    so the recorder writes exactly one assistant row per provider turn."""
    path = tmp_path / "message.jsonl"
    recorder = AgentMessageJsonlRecorder(path)

    recorder.emit(ThinkingDelta(text="plan ", agent_name="a", run_id="r"))
    recorder.emit(ThinkingDelta(text="step", agent_name="a", run_id="r"))
    recorder.emit(AssistantTextDelta(text="ok.", agent_name="a", run_id="r"))
    recorder.emit(
        AssistantMessageComplete(
            message=ConversationMessage(
                role="assistant",
                content=[
                    ThinkingBlock(text="plan step"),
                    TextBlock(text="ok."),
                    ToolUseBlock(id="t1", name="shell", input={"cmd": "ls"}),
                ],
            ),
            usage=UsageSnapshot(),
            agent_name="a",
            run_id="r",
        )
    )
    recorder.flush()

    records = _read_jsonl(path)
    assert len(records) == 1, records
    assert [b["type"] for b in records[0]["content"]] == [
        "thinking",
        "text",
        "tool_use",
    ]


def test_recorder_registry_round_trip(tmp_path) -> None:
    recorder = AgentMessageJsonlRecorder(tmp_path / "message.jsonl")
    register_recorder_for_agent_run("run-xyz", recorder)
    try:
        assert recorder_for_agent_run("run-xyz") is recorder
        assert recorder_for_agent_run("") is None
        assert recorder_for_agent_run("other") is None
    finally:
        clear_recorder_for_agent_run("run-xyz")
    assert recorder_for_agent_run("run-xyz") is None
