from __future__ import annotations

from external_trigger.tc_note import (
    TC_NOTE_EDIT_PROMPT,
    TC_NOTE_TURN_PROMPT,
    _resolve_note_taker_prompt,
)
from team.builtins import register_all


def test_tc_note_prompts_reference_submit_task_note() -> None:
    prompts = (TC_NOTE_EDIT_PROMPT, TC_NOTE_TURN_PROMPT)

    for prompt in prompts:
        assert "submit_task_note" in prompt
        assert "post_note" not in prompt


def test_tc_note_uses_builtin_note_taker_prompt_when_available() -> None:
    register_all()

    prompt, model = _resolve_note_taker_prompt()

    assert "Convert a frozen task snapshot into a concise Task Center note." in prompt
    assert "Your only output is `submit_task_note(...)`." in prompt
    assert "# Identity" not in prompt
    assert "# Role Boundary" not in prompt
    assert model is None
