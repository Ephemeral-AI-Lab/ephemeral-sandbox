"""Shared LLM loop for external-trigger tool phases.

Uses an exact single-tool choice when only one tool is available, otherwise
falls back to ``tool_choice={"type": "any"}``. Retries up to ``max_turns``
until a valid tool call passes Pydantic validation. Exit paths: successful
tool call, max turns exhausted, or asyncio cancellation.
"""

from __future__ import annotations

import asyncio
import logging
import sys
import uuid
from dataclasses import dataclass, field
from typing import Any

from pydantic import BaseModel

from prompts.message_recorder import append_prompt_report_event
from providers.types import ApiMessageRequest, ApiToolUseDeltaEvent, ApiMessageCompleteEvent
from tools.core.base import BaseTool, ToolExecutionContext, ToolResult

logger = logging.getLogger(__name__)


def _emit(msg: str) -> None:
    """Print to stdout (visible in benchmark tee) AND log at INFO level."""
    print(f"[runner] {msg}", file=sys.stdout, flush=True)
    logger.info(msg)


@dataclass
class RunResult:
    """Result of a successful external-trigger tool call."""

    tool_name: str
    tool_input: dict[str, Any]
    validated: BaseModel | None = None
    tool_result: ToolResult | None = None
    conversation: list[dict[str, Any]] = field(default_factory=list)
    turns_used: int = 0


async def _stream_to_response(api_client: Any, request: ApiMessageRequest) -> Any:
    """Consume stream_message and collect tool_use events + final message."""
    tool_uses: list[dict[str, Any]] = []
    final_message: Any = None

    async for event in api_client.stream_message(request):
        if isinstance(event, ApiToolUseDeltaEvent):
            tool_uses.append({
                "type": "tool_use",
                "id": event.id,
                "name": event.name,
                "input": event.input,
            })
        elif isinstance(event, ApiMessageCompleteEvent):
            final_message = event.message

    # Build a lightweight response-like object
    class _Block:
        def __init__(self, d: dict[str, Any]) -> None:
            self.type = d.get("type", "")
            self.name = d.get("name", "")
            self.input = d.get("input", {})
            self.id = d.get("id", "")
            self.text = d.get("text", "")

    blocks: list[_Block] = []
    # Extract text from final message if available
    if final_message is not None:
        for cb in final_message.content:
            if hasattr(cb, "text") and getattr(cb, "text", None):
                blocks.append(_Block({"type": "text", "text": cb.text}))
    # Add tool_use blocks from mid-stream events
    for tu in tool_uses:
        blocks.append(_Block(tu))

    class _Response:
        def __init__(self, content: list[_Block]) -> None:
            self.content = content

    return _Response(blocks)


async def run(
    *,
    agent_name: str,
    messages: list[dict[str, Any]],
    system_prompt: str,
    prompt: str,
    tools: list[BaseTool],
    api_client: Any,
    max_tokens_per_turn: int = 500,
    model: str | None = None,
    max_turns: int = 10,
    execution_context: ToolExecutionContext | None = None,
    execute_tools: bool = False,
    prompt_report_messages_path: str | None = None,
    team_run_id: str | None = None,
    work_item_id: str | None = None,
    agent_run_id: str | None = None,
) -> RunResult:
    """Execute the LLM loop until a valid tool call succeeds.

    Parameters
    ----------
    agent_name:
        Identity for logging/observability (e.g. "checkpoint:task_123").
    messages:
        Frozen conversation snapshot (read-only context for the LLM).
    system_prompt:
        System prompt for the LLM session.
    prompt:
        Injected as the final user message after the snapshot.
    tools:
        Constrained tool set — the LLM must call one of these.
    api_client:
        Client implementing ``stream_message(ApiMessageRequest)``.
    max_tokens_per_turn:
        Max tokens per LLM response.
    model:
        Model override. Defaults to claude-sonnet-4.
    execution_context:
        Tool execution context used when ``execute_tools`` is enabled.
    execute_tools:
        When ``True``, validated tool calls are executed immediately. Tool
        errors are fed back into the frozen conversation as ``tool_result``
        blocks so the LLM can retry with corrected input.
    """
    run_id = uuid.uuid4().hex[:8]
    tool_names = [t.name for t in tools]
    _emit(f"{agent_name} (run={run_id}) starting with tools={tool_names}")

    api_tools = [tool.to_api_schema() for tool in tools]
    tool_map = {tool.name: tool for tool in tools}
    if len(api_tools) == 1:
        tool_choice: dict[str, Any] = {
            "type": "tool",
            "name": str(api_tools[0].get("name") or tools[0].name),
        }
    else:
        tool_choice = {"type": "any"}

    conversation: list[dict[str, Any]] = list(messages) + [
        {"role": "user", "content": prompt},
    ]

    turn = 0
    while turn < max_turns:
        turn += 1

        request = ApiMessageRequest(
            model=model or "claude-sonnet-4-20250514",
            max_tokens=max_tokens_per_turn,
            system_prompt=system_prompt,
            tools=api_tools,
            tool_choice=tool_choice,
            raw_messages=conversation,
        )
        append_prompt_report_event(
            prompt_report_messages_path,
            {
                "event": "llm_request",
                "seq": turn,
                "team_run_id": team_run_id,
                "work_item_id": work_item_id,
                "agent_run_id": agent_run_id,
                "agent": agent_name,
                "model": request.model,
                "system_prompt": system_prompt,
                "messages": conversation,
                "tools": api_tools,
                "external_trigger": True,
            },
        )

        try:
            response = await _stream_to_response(api_client, request)
        except asyncio.CancelledError:
            raise
        except Exception:
            logger.warning(
                "external_trigger runner: API call failed on turn %d/%d, retrying",
                turn,
                max_turns,
                exc_info=True,
            )
            continue

        # Extract tool_use block from response
        tool_use_block: Any = None
        text_parts: list[str] = []
        for block in response.content:
            if getattr(block, "type", None) == "tool_use":
                tool_use_block = block
            elif getattr(block, "text", None):
                text_parts.append(block.text.strip())

        # With tool_choice="any", tool_use_block should always be present.
        # Defensive: if somehow missing, retry.
        if tool_use_block is None:
            logger.warning("external_trigger runner: no tool_use block on turn %d", turn)
            continue

        tool_name = tool_use_block.name
        tool_input = tool_use_block.input
        tool_id = getattr(tool_use_block, "id", f"tu_{turn}")

        # Build assistant message for conversation trail
        assistant_content: list[dict[str, Any]] = []
        if text_parts:
            assistant_content.append({"type": "text", "text": "\n".join(text_parts)})
        assistant_content.append({
            "type": "tool_use",
            "id": tool_id,
            "name": tool_name,
            "input": tool_input,
        })
        assistant_message = {"role": "assistant", "content": assistant_content}
        conversation.append(assistant_message)
        append_prompt_report_event(
            prompt_report_messages_path,
            {
                "event": "assistant",
                "seq": turn,
                "team_run_id": team_run_id,
                "work_item_id": work_item_id,
                "agent_run_id": agent_run_id,
                "agent": agent_name,
                "model": request.model,
                "message": assistant_message,
                "external_trigger": True,
            },
        )

        # Check tool is in our set
        tool = tool_map.get(tool_name)
        if tool is None:
            tool_names = list(tool_map.keys())
            _emit(f"{agent_name} (run={run_id}) turn {turn}: unknown tool '{tool_name}' (available: {tool_names})")
            tool_result_message = {
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": tool_id,
                             "content": f"Error: unknown tool '{tool_name}'. "
                                        f"Use one of: {', '.join(tool_names)}",
                             "is_error": True}],
            }
            conversation.append(tool_result_message)
            append_prompt_report_event(
                prompt_report_messages_path,
                {
                    "event": "tool_result",
                    "seq": turn,
                    "team_run_id": team_run_id,
                    "work_item_id": work_item_id,
                    "agent_run_id": agent_run_id,
                    "agent": agent_name,
                    "model": request.model,
                    "message": tool_result_message,
                    "external_trigger": True,
                },
            )
            continue

        # Pydantic validation
        validated: BaseModel | None = None
        try:
            validated = tool.input_model.model_validate(tool_input)
        except Exception as exc:
            required = tool.input_model.model_json_schema().get("required") or []
            required_hint = (
                f" Required fields for `{tool_name}`: {', '.join(map(str, required))}."
                if required
                else ""
            )
            _emit(f"{agent_name} (run={run_id}) turn {turn}: pydantic validation failed for {tool_name}: {exc}")
            tool_result_message = {
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": tool_id,
                             "content": f"Validation error: {exc}.{required_hint} Fix and retry.",
                             "is_error": True}],
            }
            conversation.append(tool_result_message)
            append_prompt_report_event(
                prompt_report_messages_path,
                {
                    "event": "tool_result",
                    "seq": turn,
                    "team_run_id": team_run_id,
                    "work_item_id": work_item_id,
                    "agent_run_id": agent_run_id,
                    "agent": agent_name,
                    "model": request.model,
                    "message": tool_result_message,
                    "external_trigger": True,
                },
            )
            continue

        tool_result: ToolResult | None = None
        if execute_tools:
            if execution_context is None:
                raise RuntimeError("external_trigger runner: execute_tools=True requires execution_context")
            try:
                tool_result = await tool.execute(validated, execution_context)
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                _emit(f"{agent_name} (run={run_id}) turn {turn}: tool {tool_name} raised: {exc}")
                tool_result = ToolResult(
                    output=f"Tool execution failed: {exc}",
                    is_error=True,
                )
            tool_result_message = {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_id,
                    "content": tool_result.output,
                    "is_error": tool_result.is_error,
                }],
            }
            conversation.append(tool_result_message)
            append_prompt_report_event(
                prompt_report_messages_path,
                {
                    "event": "tool_result",
                    "seq": turn,
                    "team_run_id": team_run_id,
                    "work_item_id": work_item_id,
                    "agent_run_id": agent_run_id,
                    "agent": agent_name,
                    "model": request.model,
                    "message": tool_result_message,
                    "external_trigger": True,
                },
            )
            if tool_result.is_error:
                _emit(f"{agent_name} (run={run_id}) turn {turn}: tool {tool_name} returned error: {tool_result.output}")
                continue

        # Success
        _emit(f"{agent_name} (run={run_id}) completed: tool={tool_name} turns={turn}")
        return RunResult(
            tool_name=tool_name,
            tool_input=tool_input,
            validated=validated,
            tool_result=tool_result,
            conversation=conversation,
            turns_used=turn,
        )

    # Print the conversation trail for debugging before raising
    trail = "\n".join(
        f"  [{m.get('role', '?')}] {str(m.get('content', ''))[:300]}"
        for m in conversation[-10:]  # last 10 messages to avoid log explosion
    )
    _emit(
        f"{agent_name} (run={run_id}): EXHAUSTED {max_turns} turns. "
        f"Conversation trail ({len(conversation)} messages):\n{trail}"
    )
    raise RuntimeError(f"runner.run {agent_name}: exhausted {max_turns} turns without valid tool call")
