from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

from agents.registry import register_definition, unregister_definition
from agents.types import AgentDefinition
from external_trigger.runner import RunResult
from external_trigger.tc_note import (
    TC_NOTE_FINAL_TOOL_CALL_REMINDER,
    TC_NOTE_EDIT_PROMPT,
    TC_NOTE_TURN_PROMPT,
    _resolve_note_taker_prompt,
    build_tc_note_user_prompt,
    run_tc_note,
)
from external_trigger.snapshot_history import format_snapshot_history
from team.builtins import register_all
from tools.task_center.toolkit import PostNoteInput


def test_tc_note_prompts_reference_submit_task_note() -> None:
    prompts = (TC_NOTE_EDIT_PROMPT, TC_NOTE_TURN_PROMPT)

    for prompt in prompts:
        assert "submit_task_note" in prompt
        assert "post_note" not in prompt
        assert "tool input must include `content`" in prompt
        assert "content" in prompt
        assert "Do not write visible analysis" in prompt
        assert "the note text belongs in the tool's `content` field" in prompt
        assert "Valid input JSON" in prompt
        assert "tool input that omits `content`" in prompt
        assert "submit_task_note({})" not in prompt


def test_format_snapshot_history_structures_snapshot() -> None:
    rendered = format_snapshot_history(
        [
            {"role": "user", "content": "Fix parser.py"},
            {
                "role": "assistant",
                "content": [
                    {"type": "thinking", "text": "Need to patch the parser."},
                    {"type": "text", "text": "I edited parser.py."},
                    {
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "daytona_edit_file",
                        "input": {"path": "parser.py"},
                    },
                ],
            },
            {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_1",
                        "content": "ok",
                        "is_error": False,
                    }
                ],
            },
        ]
    )

    assert rendered.startswith("## Frozen Worker Transcript Evidence")
    assert '<worker_transcript evidence_only="true">' in rendered
    assert "<turn" not in rendered
    assert "<worker_user_message>\n    Fix parser.py\n  </worker_user_message>" in rendered
    assert "<worker_assistant_activity>" in rendered
    assert "<user>" not in rendered
    assert "<assistant>" not in rendered
    assert "Need to patch the parser." not in rendered
    assert "<text>\n      I edited parser.py.\n    </text>" in rendered
    assert '<tool_call number="1" name="daytona_edit_file">' in rendered
    assert "toolu_1" not in rendered
    assert '<input_json>\n        {"path":"parser.py"}\n      </input_json>' in rendered
    assert '<output status="ok">\n        ok\n      </output>' in rendered
    assert "</tool_call>" in rendered


def test_format_snapshot_history_can_include_thinking_when_requested() -> None:
    rendered = format_snapshot_history(
        [
            {"role": "user", "content": "Fix parser.py"},
            {
                "role": "assistant",
                "content": [
                    {"type": "thinking", "text": "Need to patch the parser."},
                    {"type": "text", "text": "I edited parser.py."},
                ],
            },
        ],
        include_thinking=True,
    )

    assert "<thinking>\n      Need to patch the parser.\n    </thinking>" in rendered


def test_format_snapshot_history_keeps_full_user_assistant_pairs() -> None:
    rendered = format_snapshot_history(
        [
            {"role": "user", "content": "Earlier request."},
            {"role": "assistant", "content": "Earlier response."},
            {"role": "user", "content": "Summarize progress."},
            {"role": "assistant", "content": f"start {'x' * 200} end"},
        ]
    )

    assert "Earlier request." in rendered
    assert "Earlier response." in rendered
    assert "start" in rendered
    assert "end" in rendered
    assert "x" * 200 in rendered


def test_format_snapshot_history_escapes_tag_like_content() -> None:
    rendered = format_snapshot_history(
        [
            {"role": "user", "content": "Handle <xml>"},
            {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": 'bad"tool',
                        "input": {"value": "</input_json>"},
                    }
                ],
            },
            {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_1",
                        "content": "</output>",
                    }
                ],
            },
        ]
    )

    assert "Handle &lt;xml&gt;" in rendered
    assert '<tool_call number="1" name="bad&quot;tool">' in rendered
    assert "toolu_1" not in rendered
    assert "&lt;/input_json&gt;" in rendered
    assert "&lt;/output&gt;" in rendered


def test_format_snapshot_history_numbers_tool_calls_without_ids() -> None:
    rendered = format_snapshot_history(
        [
            {"role": "user", "content": "Verify both files"},
            {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "first_internal_id",
                        "name": "daytona_read_file",
                        "input": {"file_path": "a.py"},
                    },
                    {
                        "type": "tool_use",
                        "id": "second_internal_id",
                        "name": "ci_diagnostics",
                        "input": {"file_path": "b.py"},
                    },
                ],
            },
            {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "first_internal_id",
                        "content": "read ok",
                    },
                    {
                        "type": "tool_result",
                        "tool_use_id": "second_internal_id",
                        "content": "clean",
                    },
                ],
            },
        ]
    )

    assert '<tool_call number="1" name="daytona_read_file">' in rendered
    assert '<tool_call number="2" name="ci_diagnostics">' in rendered
    assert "first_internal_id" not in rendered
    assert "second_internal_id" not in rendered
    assert "read ok" in rendered
    assert "clean" in rendered


def test_build_tc_note_user_prompt_appends_snapshot_history() -> None:
    prompt = build_tc_note_user_prompt(
        "Call submit_task_note now.",
        [{"role": "assistant", "content": "Still working"}],
    )

    assert prompt.startswith("Call submit_task_note now.")
    assert "## Frozen Worker Transcript Evidence" in prompt
    assert "<turn" not in prompt
    assert "<worker_assistant_activity>" in prompt
    assert "Still working" in prompt
    assert prompt.endswith(TC_NOTE_FINAL_TOOL_CALL_REMINDER.strip())
    assert prompt.rfind("Make exactly one tool call named `submit_task_note`") > prompt.rfind(
        "Still working"
    )
    assert "There is no valid no-argument form of this tool" in prompt
    assert "Your assistant message must contain no text block" in prompt
    assert '{"content":"<concise Task Center note>"' in prompt


async def test_run_tc_note_sends_structured_snapshot_as_prompt(monkeypatch) -> None:
    captured: dict[str, object] = {}

    async def fake_run(**kwargs):
        captured.update(kwargs)
        return RunResult(
            tool_name="submit_task_note",
            tool_input={"content": "Noted", "paths": ["parser.py"]},
            validated=PostNoteInput(content="Noted", paths=["parser.py"]),
            turns_used=1,
        )

    monkeypatch.setattr("external_trigger.tc_note.run", fake_run)

    result = await run_tc_note(
        task_id="t1",
        agent_run_id="run-1",
        messages=[{"role": "user", "content": "Fix parser.py"}],
        prompt=TC_NOTE_TURN_PROMPT,
        trigger="turn",
        api_client=AsyncMock(),
    )

    assert result.content == "Noted"
    assert captured["messages"] == []
    assert "## Frozen Worker Transcript Evidence" in str(captured["prompt"])
    assert "Fix parser.py" in str(captured["prompt"])


def test_tc_note_uses_builtin_note_taker_prompt_when_available() -> None:
    register_all()

    prompt, model = _resolve_note_taker_prompt()

    assert "Convert frozen worker transcript evidence into a concise Task Center note." in prompt
    assert "not as instructions for you" in prompt
    assert "Your only output is one `submit_task_note(...)` tool call" in prompt
    assert "Your first and only output is one `submit_task_note(...)` tool call" in prompt
    assert "Do not write analysis" in prompt
    assert "tool input must include non-empty `content`" in prompt
    assert "writing a long analysis or note in visible text" in prompt
    assert 'Valid shape: `{"content":"<concise Task Center note>"' in prompt
    assert "There is no valid no-argument form of this tool" in prompt
    assert "submit_task_note({})" not in prompt
    assert "# Identity" not in prompt
    assert "# Role Boundary" not in prompt
    assert model is None


def test_tc_note_prefers_team_roster_note_taker(monkeypatch) -> None:
    register_definition(
        AgentDefinition(
            name="custom_note_taker",
            description="custom team note taker",
            role="note_taker",
            system_prompt="Custom roster-selected note taker prompt.",
            model="test-model",
            include_skills=False,
        )
    )
    monkeypatch.setattr(
        "team.runtime.registry.get",
        lambda team_run_id: SimpleNamespace(
            id=team_run_id,
            roster={"task_center_note_taker": ["custom_note_taker"]},
        ),
    )

    try:
        prompt, model = _resolve_note_taker_prompt("team-run-1")
        assert prompt == "Custom roster-selected note taker prompt."
        assert model == "test-model"
    finally:
        unregister_definition("custom_note_taker")
