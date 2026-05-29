"""Unit tests for ``AdvisorApprovalPreHook``.

Each case mirrors the decision table in
``docs/plans/advisor-gated-terminal-tools-implementation-plan.md`` §3.2.
Case 10 is an introspection guard: it walks the registered submission tools and
asserts the hook is present on each main-role terminal and absent from each
helper terminal.
"""

from __future__ import annotations

from pathlib import Path

import pytest
from pydantic import BaseModel

from message.message import (
    Message,
    ToolResultBlock,
    ToolUseBlock,
)
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.hooks import HookResult
from tools._framework.core.runtime import ExecutionMetadata
from tools._framework.factory import ToolFactoryContext, create_tool
from tools._hooks.advisor_approval import AdvisorApprovalPreHook

from ._advisor_approval_fixtures import build_advisor_approval_messages


_TARGET_TOOL = "submit_execution_success"


class _DummyInput(BaseModel):
    pass


def _context(messages: list[Message] | None) -> ToolExecutionContextService:
    metadata = ExecutionMetadata()
    if messages is not None:
        metadata = metadata.with_overrides(conversation_messages=messages)
    return ToolExecutionContextService(cwd=Path("/tmp"), services=metadata)


def _hook() -> AdvisorApprovalPreHook:
    return AdvisorApprovalPreHook(_TARGET_TOOL)


# All fail branches return the same user-facing prescriptive message
# (_MSG_BLOCKED). Per-branch distinction lives in metadata["reason"]. Tests
# assert on the message uniformity AND the metadata tag so the decision-table
# coverage is preserved without coupling to branch-specific prose.
_BLOCKED_PREAMBLE = "BLOCKED: You must get approval from advisor"


def _reason(result: HookResult) -> str:
    return str(result.metadata.get("reason") or "")


# ----- Case 1 ---------------------------------------------------------------
@pytest.mark.asyncio
async def test_no_advisor_history_returns_missing() -> None:
    result = await _hook().run(_DummyInput(), _context(messages=[]))
    assert result.status == "fail"
    assert _BLOCKED_PREAMBLE in result.reason
    assert _TARGET_TOOL in result.reason
    assert _reason(result) == "missing"


# ----- Case 2 ---------------------------------------------------------------
@pytest.mark.asyncio
async def test_approve_for_target_passes() -> None:
    messages = build_advisor_approval_messages(tool_name=_TARGET_TOOL)
    result = await _hook().run(_DummyInput(), _context(messages))
    assert result.status == "pass"


# ----- Case 3 ---------------------------------------------------------------
@pytest.mark.asyncio
async def test_approve_for_different_tool_fails() -> None:
    messages = build_advisor_approval_messages(tool_name="submit_execution_blocker")
    result = await _hook().run(_DummyInput(), _context(messages))
    assert result.status == "fail"
    assert _BLOCKED_PREAMBLE in result.reason
    assert _TARGET_TOOL in result.reason
    assert _reason(result) == "wrong_tool"


# ----- Case 4 ---------------------------------------------------------------
@pytest.mark.asyncio
async def test_reject_fails_with_uniform_message() -> None:
    summary = "Payload references TODOs in the transcript."
    messages = build_advisor_approval_messages(
        tool_name=_TARGET_TOOL, verdict="reject", summary=summary
    )
    result = await _hook().run(_DummyInput(), _context(messages))
    assert result.status == "fail"
    assert _BLOCKED_PREAMBLE in result.reason
    # By design the hook does NOT echo the advisor's reject summary back: the
    # agent already has the prior ask_advisor result in its conversation
    # history and can read the summary from there.
    assert summary not in result.reason
    assert _reason(result) == "rejected"


# ----- Case 5 ---------------------------------------------------------------
@pytest.mark.asyncio
async def test_latest_of_two_calls_wins_when_latest_is_approve() -> None:
    older = build_advisor_approval_messages(
        tool_name="submit_execution_blocker",
        verdict="reject",
        summary="wrong tool",
        tool_use_id="toolu_older",
    )
    newer = build_advisor_approval_messages(
        tool_name=_TARGET_TOOL,
        verdict="approve",
        tool_use_id="toolu_newer",
    )
    result = await _hook().run(_DummyInput(), _context(older + newer))
    assert result.status == "pass"


# ----- Case 6 ---------------------------------------------------------------
@pytest.mark.asyncio
async def test_latest_reject_overrides_prior_approve() -> None:
    older = build_advisor_approval_messages(
        tool_name=_TARGET_TOOL,
        verdict="approve",
        tool_use_id="toolu_older",
    )
    newer = build_advisor_approval_messages(
        tool_name=_TARGET_TOOL,
        verdict="reject",
        summary="payload regressed",
        tool_use_id="toolu_newer",
    )
    result = await _hook().run(_DummyInput(), _context(older + newer))
    assert result.status == "fail"
    assert _reason(result) == "rejected"


# ----- Case 7 ---------------------------------------------------------------
@pytest.mark.asyncio
async def test_advisor_is_error_treated_as_failed_advisor() -> None:
    messages = build_advisor_approval_messages(
        tool_name=_TARGET_TOOL, is_error=True
    )
    result = await _hook().run(_DummyInput(), _context(messages))
    assert result.status == "fail"
    assert _reason(result) == "advisor_failed"


# ----- Case 8 ---------------------------------------------------------------
@pytest.mark.asyncio
async def test_unknown_verdict_is_structural_error() -> None:
    messages = build_advisor_approval_messages(
        tool_name=_TARGET_TOOL, verdict="approved"  # typo
    )
    result = await _hook().run(_DummyInput(), _context(messages))
    assert result.status == "fail"
    assert _reason(result) == "structural"


# ----- Case 9 ---------------------------------------------------------------
@pytest.mark.asyncio
async def test_result_without_originating_call_is_unpaired() -> None:
    # Only the user-side advisor result is in the transcript; the assistant
    # message that originally produced the ask_advisor call has been trimmed
    # (compaction). The hook still surfaces a uniform deny but tags the
    # branch in metadata so ops can see it.
    user_msg = Message(
        role="user",
        content=[
            ToolResultBlock(
                tool_use_id="toolu_orphan",
                content="ok",
                is_error=False,
                metadata={"helper_role": "advisor", "verdict": "approve"},
            )
        ],
    )
    result = await _hook().run(_DummyInput(), _context([user_msg]))
    assert result.status == "fail"
    assert _reason(result) == "unpaired"


# ----- Case 10 --------------------------------------------------------------
def test_hook_wired_to_main_terminals_and_omitted_from_helpers() -> None:
    """Structural guard: every main terminal carries the hook; helpers do not."""
    main_terminals = (
        "submit_plan_closes_goal",
        "submit_plan_defers_goal",
        "submit_execution_success",
        "submit_execution_blocker",
        "submit_execution_handoff",
        "submit_evaluation_success",
        "submit_evaluation_failure",
        "submit_verification_success",
        "submit_verification_failure",
    )
    helper_terminals = (
        "submit_advisor_feedback",
        "submit_exploration_result",
    )
    ctx = ToolFactoryContext()

    for name in main_terminals:
        tool = create_tool(name, ctx)
        hooks = tuple(getattr(tool, "pre_hooks", ()) or ())
        advisor_hooks = [h for h in hooks if isinstance(h, AdvisorApprovalPreHook)]
        assert len(advisor_hooks) == 1, (
            f"{name!r}: expected exactly one AdvisorApprovalPreHook, got {hooks!r}"
        )
        assert advisor_hooks[0].target_tool == name, (
            f"{name!r}: hook.target_tool={advisor_hooks[0].target_tool!r}"
        )

    for name in helper_terminals:
        tool = create_tool(name, ctx)
        hooks = tuple(getattr(tool, "pre_hooks", ()) or ())
        advisor_hooks = [h for h in hooks if isinstance(h, AdvisorApprovalPreHook)]
        assert not advisor_hooks, (
            f"{name!r}: helper terminal must not carry AdvisorApprovalPreHook"
        )


@pytest.mark.asyncio
async def test_hook_returns_hook_result_type() -> None:
    """Type guard: contract requires a ``HookResult`` instance back."""
    result = await _hook().run(_DummyInput(), _context(messages=[]))
    assert isinstance(result, HookResult)


@pytest.mark.asyncio
async def test_originating_use_id_must_be_ask_advisor() -> None:
    """A non-ask_advisor ToolUseBlock with the same id must not be paired."""
    fake_assistant = Message(
        role="assistant",
        content=[
            ToolUseBlock(
                tool_use_id="toolu_collision",
                name="read_file",
                input={"path": "/tmp/x"},
            )
        ],
    )
    user_msg = Message(
        role="user",
        content=[
            ToolResultBlock(
                tool_use_id="toolu_collision",
                content="ok",
                is_error=False,
                metadata={"helper_role": "advisor", "verdict": "approve"},
            )
        ],
    )
    result = await _hook().run(
        _DummyInput(), _context([fake_assistant, user_msg])
    )
    # Even though a ToolUseBlock with this id exists, it is not ask_advisor —
    # so the pair is unresolvable.
    assert result.status == "fail"
    assert _reason(result) == "unpaired"
