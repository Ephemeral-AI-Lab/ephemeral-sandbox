"""Tests for tools.submission.toolkit."""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from tools.submission.toolkit import SubmitTaskSummaryTool


def test_submit_task_summary_rejects_whitespace_only_content():
    with pytest.raises(ValidationError, match="content must contain non-whitespace text"):
        SubmitTaskSummaryTool.input_model(type="success", content=" \n\t")
