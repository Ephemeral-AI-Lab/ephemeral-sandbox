"""Provider request construction and prompt-report recording for agent runs."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from engine.query.provider_history import prepare_provider_messages
from message.agent_message_recorder import recorder_for_agent_run
from message.messages import ConversationMessage, TextBlock
from prompt.prompt_report_recorder import PromptReportRecorder, recorder_for_context
from providers.types import ApiMessageRequest
from tools import decorate_schemas_for_background

if TYPE_CHECKING:
    from engine.query.context import QueryContext


@dataclass(frozen=True)
class QueryRunRequest:
    request: ApiMessageRequest
    prompt_report: PromptReportRecorder
    prompt_report_seq: int


def build_query_run_request(
    context: QueryContext,
    messages: list[ConversationMessage],
) -> QueryRunRequest:
    provider_messages = prepare_provider_messages(messages)
    prompt_report = recorder_for_context(context)
    prompt_report_seq = prompt_report.next_seq()
    tool_schemas = context.tool_registry.to_api_schema()
    if context.enable_background_tasks:
        tool_schemas = decorate_schemas_for_background(
            context.tool_registry,
            tool_schemas,
            terminal_tools=context.terminal_tools,
        )

    prompt_report.record_llm_request(
        seq=prompt_report_seq,
        system_prompt=context.system_prompt,
        messages=provider_messages,
        tools=tool_schemas,
    )

    _record_initial_messages_once(context, messages)

    return QueryRunRequest(
        request=ApiMessageRequest(
            model=context.model,
            messages=provider_messages,
            system_prompt=context.system_prompt,
            max_tokens=context.max_tokens,
            tools=tool_schemas,
        ),
        prompt_report=prompt_report,
        prompt_report_seq=prompt_report_seq,
    )


def _record_initial_messages_once(
    context: QueryContext, messages: list[ConversationMessage]
) -> None:
    """Write the system prompt + first user message to the task's message.jsonl.

    The recorder ignores repeated calls via its ``_initial_messages_recorded``
    flag, so this is safe on every turn. The audit recorder is reached via a
    module-level registry keyed by ``agent_run_id`` (see
    ``message.agent_message_recorder.register_recorder_for_agent_run``).
    """
    recorder = recorder_for_agent_run(context.run_id)
    if recorder is None:
        return
    user_prompt = _first_user_prompt_text(messages)
    if user_prompt is None:
        return
    recorder.record_initial_messages(
        system_prompt=context.system_prompt,
        user_prompt=user_prompt,
        agent_name=context.agent_name,
        run_id=context.run_id,
    )


def _first_user_prompt_text(messages: list[ConversationMessage]) -> str | None:
    for message in messages:
        if message.role != "user":
            continue
        parts: list[str] = []
        for block in message.content:
            if isinstance(block, TextBlock):
                parts.append(block.text)
        return "".join(parts) if parts else ""
    return None
