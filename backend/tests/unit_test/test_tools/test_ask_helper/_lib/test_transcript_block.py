"""Tests for ``tools.ask_helper._lib._transcript.build_parent_transcript``.

Locks the two-stage filter (drop ``role=='system'`` defensively, drop the
first two user messages), the state-mutating-tool input strip rule, the
message-count cap, the tool-result truncation, and the total-byte cap
with elision marker.
"""

from __future__ import annotations

import logging

from message.message import (
    Message,
    TextBlock,
    ThinkingBlock,
    ToolResultBlock,
    ToolUseBlock,
)
from tools.ask_helper._lib._transcript import (
    MAX_BASH_COMMAND_CHARS,
    MAX_TOOL_RESULT_CHARS,
    MAX_TRANSCRIPT_BYTES,
    MAX_TRANSCRIPT_MESSAGES,
    _ADVISOR_STRIP_INPUT_TOOLS,
    build_parent_transcript,
)


def _user(text: str) -> Message:
    return Message(role="user", content=[TextBlock(text=text)])


def _assistant(text: str) -> Message:
    return Message(role="assistant", content=[TextBlock(text=text)])


def _assistant_tool(name: str, **kwargs) -> Message:
    return Message(
        role="assistant", content=[ToolUseBlock(name=name, input=kwargs)]
    )


def _assistant_thinking() -> Message:
    return Message(
        role="assistant", content=[ThinkingBlock(text="planning…")]
    )


def _user_tool_result(content: str, *, is_error: bool = False) -> Message:
    return Message(
        role="user",
        content=[
            ToolResultBlock(tool_use_id="t1", content=content, is_error=is_error)
        ],
    )


# ---- Shared filter behaviour --------------------------------------------


def test_empty_messages_returns_none():
    assert build_parent_transcript([]) is None


def test_first_non_user_message_returns_none_and_logs_warning(caplog):
    caplog.set_level(logging.WARNING)
    out = build_parent_transcript([_assistant("hi")])
    assert out is None
    assert any(
        "first non-system message has role" in rec.message
        for rec in caplog.records
    )


def test_system_messages_filtered_defensively():
    class _Sys:
        role = "system"
        content = [TextBlock(text="ignored")]

    msgs = [
        _Sys(),
        _user("user_msg_1"),
        _user("user_msg_2"),
        _assistant("kept after filter"),
    ]
    out = build_parent_transcript(msgs)
    assert out is not None
    assert "ignored" not in out
    assert "user_msg_1" not in out
    assert "user_msg_2" not in out
    assert "kept after filter" in out


# ---- Drop-first-two rule -----------------------------------------------


def test_drops_first_two_user_messages():
    msgs = [
        _user("user_msg_1 (context)"),
        _user("user_msg_2 (task)"),
        _assistant_tool("Bash", command="pytest"),
        _user_tool_result("2 failed", is_error=True),
    ]
    out = build_parent_transcript(msgs)
    assert out is not None
    assert "user_msg_1" not in out
    assert "user_msg_2" not in out
    assert "tool_use: Bash" in out
    assert "2 failed" in out


def test_returns_none_when_only_first_two_messages_present():
    msgs = [_user("user_msg_1"), _user("user_msg_2")]
    assert build_parent_transcript(msgs) is None


# ---- Tool-use input filter rules ---------------------------------------


def test_strips_inputs_for_write_edit_notebookedit():
    msgs = [
        _user("user_msg_1"),
        _user("user_msg_2"),
        _assistant_tool("Write", file_path="x.py", content="secret"),
        _assistant_tool("Edit", file_path="y.py", old_string="a", new_string="b"),
        _assistant_tool("NotebookEdit", path="z.ipynb"),
    ]
    out = build_parent_transcript(msgs)
    assert out is not None
    assert "tool_use: Write" in out
    assert "tool_use: Edit" in out
    assert "tool_use: NotebookEdit" in out
    assert "(input elided)" in out
    assert "secret" not in out
    assert "old_string" not in out


def test_renders_bash_command_only_with_cap():
    long_command = "echo " + ("X" * (MAX_BASH_COMMAND_CHARS + 200))
    msgs = [
        _user("user_msg_1"),
        _user("user_msg_2"),
        _assistant_tool("Bash", command=long_command),
    ]
    out = build_parent_transcript(msgs)
    assert out is not None
    assert "tool_use: Bash" in out
    # Full command is NOT present; truncation marker IS.
    assert long_command not in out
    assert "truncated" in out


def test_keeps_full_input_for_read_grep_glob():
    msgs = [
        _user("user_msg_1"),
        _user("user_msg_2"),
        _assistant_tool("Read", file_path="src/auth.py"),
    ]
    out = build_parent_transcript(msgs)
    assert out is not None
    assert "tool_use: Read" in out
    assert "src/auth.py" in out


def test_drops_thinking_blocks():
    msgs = [
        _user("user_msg_1"),
        _user("user_msg_2"),
        _assistant_thinking(),
        _assistant("after-thinking text"),
    ]
    out = build_parent_transcript(msgs)
    assert out is not None
    assert "_(thinking)_" not in out
    assert "after-thinking text" in out


# ---- Tool-result rendering ---------------------------------------------


def test_tool_result_content_truncated_at_max_chars():
    huge = "X" * (MAX_TOOL_RESULT_CHARS + 5_000)
    msgs = [
        _user("user_msg_1"),
        _user("user_msg_2"),
        _assistant_tool("Bash", command="something"),
        _user_tool_result(huge),
    ]
    out = build_parent_transcript(msgs)
    assert out is not None
    assert huge not in out
    assert "truncated" in out
    assert "X" * 64 in out


def test_tool_result_error_flag_renders_marker():
    msgs = [
        _user("user_msg_1"),
        _user("user_msg_2"),
        _assistant_tool("Bash", command="echo"),
        _user_tool_result("boom", is_error=True),
    ]
    out = build_parent_transcript(msgs)
    assert out is not None
    assert "tool_result [error]" in out


# ---- Caps --------------------------------------------------------------


def test_message_count_capped_at_max_transcript_messages():
    msgs = (
        [_user("user_msg_1"), _user("user_msg_2")]
        + [_assistant(f"msg {i}") for i in range(200)]
    )
    out = build_parent_transcript(msgs)
    assert out is not None
    boundary = 200 - MAX_TRANSCRIPT_MESSAGES
    assert f"msg {boundary}" in out  # first kept
    assert f"msg {boundary - 1}" not in out  # last dropped
    assert "msg 199" in out


def test_byte_cap_emits_elision_marker_for_oversized_transcript():
    big_text = "B" * 4_000  # 4 KB per assistant msg
    msgs = (
        [_user("user_msg_1"), _user("user_msg_2")]
        + [_assistant(big_text) for _ in range(20)]
    )
    out = build_parent_transcript(msgs)
    assert out is not None
    assert len(out.encode("utf-8")) <= MAX_TRANSCRIPT_BYTES
    assert "earlier message" in out and "elided" in out


# ---- Inline-edit visibility for verifier/evaluator (load-bearing) -------


def test_edit_file_input_is_rendered_verbatim_in_transcript():
    """The advisor MUST see verifier/evaluator edit_file inputs.

    Per remove-ask-resolver plan §7 Risk 1, the load-bearing scope-creep
    gate is the advisor seeing inline-edit inputs verbatim. EphemeralOS's
    lowercase ``edit_file`` / ``write_file`` are intentionally NOT in
    ``_ADVISOR_STRIP_INPUT_TOOLS`` — only Claude Code's literal ``Edit`` /
    ``Write`` / ``NotebookEdit`` are stripped.
    """
    msgs = [
        _user("user_msg_1"),
        _user("user_msg_2"),
        _assistant_tool(
            "edit_file",
            file_path="src/auth.py",
            old_string="typo",
            new_string="fixed",
        ),
    ]
    out = build_parent_transcript(msgs)
    assert out is not None
    assert "tool_use: edit_file" in out
    assert "src/auth.py" in out
    assert "typo" in out
    assert "fixed" in out
    assert "(input elided)" not in out


def test_advisor_strip_set_is_frozen_to_claude_code_literal_names():
    """Codifies the strip-set invariant per remove-ask-resolver §7 Risk 1.

    The advisor sees full ``edit_file`` / ``write_file`` inputs in the
    parent's transcript because EphemeralOS's lowercase tool names are NOT
    in ``_ADVISOR_STRIP_INPUT_TOOLS``. That visibility is how the advisor
    catches verifier/evaluator inline edits that exceed the typo /
    single-line scope. If this constant gains ``edit_file`` or
    ``write_file``, the scope-creep gate is gone — update the plan AND
    this assertion deliberately.
    """
    assert _ADVISOR_STRIP_INPUT_TOOLS == frozenset(
        {"Edit", "Write", "NotebookEdit"}
    ), (
        "Per remove-ask-resolver plan §7 Risk 1, this constant must NOT "
        "gain 'edit_file' or 'write_file' — the advisor seeing edit inputs "
        "verbatim is the load-bearing scope-creep gate. If you change this, "
        "update the plan."
    )
