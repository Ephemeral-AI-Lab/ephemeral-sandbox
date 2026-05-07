"""Provider request construction and prompt-report recording for agent runs."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from engine.core.provider_history import prepare_provider_messages
from message.messages import ConversationMessage, ToolResultBlock
from prompt.prompt_report_recorder import PromptReportRecorder
from providers.types import ApiMessageRequest, UsageSnapshot
from tools import decorate_schemas_for_background

if TYPE_CHECKING:
    from engine.core.query import QueryContext


@dataclass(frozen=True)
class QueryRunRequest:
    request: ApiMessageRequest
    prompt_report: PromptReportRecorder
    prompt_report_seq: int


def prompt_report_recorder(context: QueryContext) -> PromptReportRecorder:
    if context.prompt_report_recorder is not None:
        return context.prompt_report_recorder
    metadata = context.tool_metadata
    context.prompt_report_recorder = PromptReportRecorder(
        metadata.get("prompt_report_messages_path") if metadata is not None else None,
        base_event=(
            {
                "agent_run_id": metadata.get("agent_run_id"),
                "agent": context.agent_name or metadata.get("agent_name"),
                "model": context.model,
            }
            if metadata is not None
            else {"agent": context.agent_name, "model": context.model}
        ),
    )
    return context.prompt_report_recorder


def build_query_run_request(
    context: QueryContext,
    messages: list[ConversationMessage],
) -> QueryRunRequest:
    provider_messages = prepare_provider_messages(messages)
    prompt_report = prompt_report_recorder(context)
    prompt_report_seq = prompt_report.next_seq()
    tool_schemas = context.tool_registry.to_api_schema()
    if context.enable_background_tasks:
        tool_schemas = decorate_schemas_for_background(
            context.tool_registry,
            tool_schemas,
            terminal_tools=context.terminal_tools,
        )

    prompt_report.record(
        {
            "event": "llm_request",
            "seq": prompt_report_seq,
            "system_prompt": context.system_prompt,
            "messages": [m.model_dump(mode="json") for m in provider_messages],
            "tools": tool_schemas,
        }
    )

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


def record_assistant_message(
    run_request: QueryRunRequest,
    message: ConversationMessage,
    usage: UsageSnapshot,
) -> None:
    run_request.prompt_report.record(
        {
            "event": "assistant",
            "seq": run_request.prompt_report_seq,
            "message": message.model_dump(mode="json"),
            "usage": usage.model_dump(mode="json"),
        }
    )


def record_tool_results(
    run_request: QueryRunRequest,
    tool_results: list[ToolResultBlock],
) -> None:
    run_request.prompt_report.record(
        {
            "event": "tool_results",
            "seq": run_request.prompt_report_seq,
            "tool_results": [
                result.model_dump(mode="json") for result in tool_results
            ],
        }
    )
