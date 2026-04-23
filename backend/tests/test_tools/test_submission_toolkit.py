"""Tests for tools.submission.toolkit."""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from tools.submission.toolkit import (
    RequestReplanTool,
    SubmitPlanTool,
    SubmitTaskSuccessTool,
)


def test_submit_task_success_rejects_whitespace_only_summary():
    with pytest.raises(ValidationError, match="summary must contain non-whitespace text"):
        SubmitTaskSuccessTool.input_model(summary=" \n\t")


def test_request_replan_rejects_whitespace_only_reason():
    with pytest.raises(ValidationError, match="reason must contain non-whitespace text"):
        RequestReplanTool.input_model(reason=" \n\t")


def test_submit_task_success_schema_requests_evidence_rich_summary():
    schema = SubmitTaskSuccessTool().to_api_schema()
    summary_desc = schema["input_schema"]["properties"]["summary"]["description"]

    assert "Evidence-rich success summary" in summary_desc
    assert "verification commands and outcomes" in summary_desc


def test_request_replan_schema_requests_trigger_and_evidence():
    schema = RequestReplanTool().to_api_schema()
    reason_desc = schema["input_schema"]["properties"]["reason"]["description"]

    assert "replan trigger" in reason_desc
    assert "scope_expansion" in reason_desc
    assert "unresolved_blocker" in reason_desc


def test_submit_plan_schema_requests_concrete_acceptance_evidence():
    schema = SubmitPlanTool().to_api_schema()
    description = schema["description"]
    spec_desc = schema["input_schema"]["$defs"]["NewTaskSpec"]["properties"]["spec"][
        "description"
    ]

    assert "Use validator tasks when a distinct verification lane is useful" in description
    assert "should name concrete commands" in spec_desc
    assert "expected evidence" in spec_desc
