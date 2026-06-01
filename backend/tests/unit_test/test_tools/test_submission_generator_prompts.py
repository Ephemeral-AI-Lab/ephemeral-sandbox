"""Generator submission prompt contract tests."""

from __future__ import annotations

from tools.submission.generator._prompt_guidance import (
    GENERATOR_SUBMISSION_CHOICE_GUIDANCE,
)
from tools.submission.generator.submit_generator_outcome.prompt import (
    get_submit_generator_outcome_description,
)


def test_generator_submission_prompt_describes_delegate_workflow_boundary() -> None:
    generator = get_submit_generator_outcome_description()

    assert GENERATOR_SUBMISSION_CHOICE_GUIDANCE in generator
    assert "## Success vs Failure Decision" in generator
    assert '`status`: `"success"`' in generator
    assert '`"failed"`' in generator
    assert "`delegate_workflow`" in generator
    assert "outstanding workflow handles" in generator
