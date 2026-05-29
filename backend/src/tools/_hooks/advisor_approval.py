"""Pre-hook that gates main-agent terminal submissions on an advisor approval.

The hook scans ``context.conversation_messages`` for the most recent
``ask_advisor`` result, pairs it with its originating ``ToolUseBlock`` to
recover the ``tool_name`` argument, and rejects the terminal call unless
the advisor approved THIS specific terminal.

Wiring is per-terminal: each gated tool's ``@tool`` decorator carries
``pre_hooks=(AdvisorApprovalPreHook("<own_name>"),)``. Helper / subagent
terminals (``submit_advisor_feedback``, ``submit_exploration_result``)
intentionally omit the hook.
"""

from __future__ import annotations

from pydantic import BaseModel

from message.message import Message, ToolResultBlock, ToolUseBlock
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.hooks import HookResult


_ADVISOR_HELPER_ROLE = "advisor"
_VALID_VERDICTS = frozenset({"approve", "reject"})

# Single prescriptive message for every fail branch. Agents already have the
# prior advisor exchange (reject summaries, prior tool selection, errored
# ask_advisor results) in their own conversation history, so echoing branch-
# specific detail back would duplicate context the agent already sees. Keep
# the user-facing message uniform; per-branch distinction is preserved in
# ``HookResult.metadata["reason"]`` for ops/debugging.
_MSG_BLOCKED = (
    "BLOCKED: You must get approval from advisor before submitting this "
    "terminal. Call ask_advisor(tool_name=\"{tool}\", tool_payload=...) and "
    "resubmit only after the advisor returns verdict=\"approve\"."
)


class AdvisorApprovalPreHook:
    """Per-terminal hook: requires advisor approval for THIS tool.

    The hook is instance-per-terminal so ``target_tool`` matches the
    decorator's ``name``. ``validate_hook_targets`` reads ``target_tool``
    via ``getattr``; an instance attribute is sufficient.
    """

    def __init__(self, target_tool: str) -> None:
        self.target_tool = target_tool
        self.name = f"advisor_approval:{target_tool}"

    async def run(
        self,
        tool_input: BaseModel,
        context: ToolExecutionContextService,
    ) -> HookResult[BaseModel]:
        messages = context.get("conversation_messages") or []
        result_block, originating = _find_latest_advisor_pair(messages)
        reason = self._classify(result_block, originating)
        if reason is None:
            return HookResult.pass_(tool_input)
        return HookResult.fail(
            _MSG_BLOCKED.format(tool=self.target_tool),
            metadata={"policy": "advisor_approval", "reason": reason},
        )

    def _classify(
        self,
        result_block: ToolResultBlock | None,
        originating: ToolUseBlock | None,
    ) -> str | None:
        """Return a failure-reason tag, or ``None`` if the gate passes.

        The tag drives ops/observability via ``HookResult.metadata["reason"]``;
        the agent-facing message is uniform across branches.
        """
        if result_block is None:
            return "missing"
        if result_block.is_error:
            return "advisor_failed"
        verdict = result_block.metadata.get("verdict")
        if verdict not in _VALID_VERDICTS:
            return "structural"
        if verdict == "reject":
            return "rejected"
        if originating is None:
            return "unpaired"
        if originating.input.get("tool_name") != self.target_tool:
            return "wrong_tool"
        return None


def _find_latest_advisor_pair(
    messages: list[Message],
) -> tuple[ToolResultBlock | None, ToolUseBlock | None]:
    """Reverse-walk for the most recent advisor result and forward-pair it."""
    for msg in reversed(messages):
        if msg.role != "user":
            continue
        for block in reversed(msg.content):
            if (
                isinstance(block, ToolResultBlock)
                and block.metadata.get("helper_role") == _ADVISOR_HELPER_ROLE
            ):
                originating = _find_originating_ask_advisor(messages, block.tool_use_id)
                return block, originating
    return None, None


def _find_originating_ask_advisor(
    messages: list[Message],
    tool_use_id: str,
) -> ToolUseBlock | None:
    for msg in messages:
        if msg.role != "assistant":
            continue
        for block in msg.tool_uses:
            if block.tool_use_id == tool_use_id and block.name == "ask_advisor":
                return block
    return None


__all__ = ["AdvisorApprovalPreHook"]
