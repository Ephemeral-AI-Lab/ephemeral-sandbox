"""ask_advisor blocking helper tool."""

from __future__ import annotations

import json

from pydantic import BaseModel, Field

from tools.ask_helper._lib._compose import (
    HelperComposeError,
    compose_helper_bundle,
)
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult


class AskAdvisorInput(BaseModel):
    tool_name: str = Field(..., min_length=1)
    tool_payloads: list[dict[str, object]] = Field(default_factory=list)
    prompt: str = Field(..., min_length=1)


def _question_section(
    *,
    tool_name: str,
    tool_payloads: list[dict[str, object]],
    prompt: str,
) -> str:
    payloads = json.dumps(tool_payloads, indent=2, sort_keys=True)
    return (
        "# Advisor request\n\n"
        f"Tool name: {tool_name}\n\n"
        f"Tool payloads:\n{payloads}\n\n"
        f"Prompt:\n{prompt}\n"
    )


@tool(
    name="ask_advisor",
    description=(
        "Ask the advisor helper for blocking read-only advice before a "
        "terminal submission or decision."
    ),
    input_model=AskAdvisorInput,
    output_model=TextToolOutput,
)
async def ask_advisor(
    tool_name: str,
    tool_payloads: list[dict[str, object]],
    prompt: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    from engine.api import run_ephemeral_agent

    runtime_config = context.runtime_config
    if runtime_config is None:
        return ToolResult(
            output="ask_advisor: missing runtime_config in execution context.",
            is_error=True,
        )

    try:
        bundle = compose_helper_bundle(
            helper_role="advisor",
            base_agent_name="advisor",
            context=context,
        )
    except HelperComposeError as exc:
        return exc.to_tool_result()

    composed_input = bundle.rendered_prompt.rstrip() + "\n\n" + _question_section(
        tool_name=tool_name,
        tool_payloads=tool_payloads,
        prompt=prompt,
    )

    result = await run_ephemeral_agent(
        runtime_config,
        composed_input,
        agent_def=bundle.agent_def,
        sandbox_id=context.sandbox_id or None,
        persist_agent_run=False,
        extra_tool_metadata=context.services_with_overrides(
            role="advisor",
            agent_type="agent",
        ),
    )
    if result.status == "failed":
        return ToolResult(output=f"ask_advisor: advisor crashed: {result.error}", is_error=True)
    if result.terminal_result is None:
        return ToolResult(
            output="ask_advisor: advisor exited without submit_advisor_feedback.",
            is_error=True,
        )
    terminal = result.terminal_result
    return ToolResult(
        output=terminal.output,
        is_error=terminal.is_error,
        metadata=dict(terminal.metadata or {}),
    )
