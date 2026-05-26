"""Regression guard for the four MOCK_* EventType values.

MockSquadRunner publishes these side-channel events; ScenarioLifecycle consumes
them when assembling the rich mock ``RunReport`` view.
"""

from __future__ import annotations

from task_center_runner.audit.events import EventType


def test_mock_event_types_exist_with_expected_string_values() -> None:
    assert EventType.MOCK_LAUNCH_RECORDED.value == "mock_launch_recorded"
    assert EventType.MOCK_TOOL_CALL_RECORDED.value == "mock_tool_call_recorded"
    assert EventType.MOCK_PROMPT_INSPECTED.value == "mock_prompt_inspected"
    assert EventType.MOCK_SANDBOX_CHECK_RECORDED.value == "mock_sandbox_check_recorded"


def test_mock_event_types_are_part_of_eventtype_enum() -> None:
    """The 4 MOCK_* values live in the production ``EventType`` enum, not a separate one."""
    values = {member.value for member in EventType}
    assert {
        "mock_launch_recorded",
        "mock_tool_call_recorded",
        "mock_prompt_inspected",
        "mock_sandbox_check_recorded",
    }.issubset(values)
