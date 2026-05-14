"""ask_resolver blocking helper tool."""

from __future__ import annotations

from pydantic import BaseModel, Field

from tools.ask_helper._lib._compose import (
    HelperComposeError,
    compose_helper_bundle,
)
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult


class AskResolverInput(BaseModel):
    issues_to_resolve: list[str] = Field(..., min_length=1)
    issue_context: str = Field(default="")


def _issue_section(*, issues_to_resolve: list[str], issue_context: str) -> str:
    issues = "\n".join(f"- {issue}" for issue in issues_to_resolve)
    return (
        "# Resolver request\n\n"
        f"Issues:\n{issues}\n\n"
        f"Context:\n{issue_context}\n"
    )


@tool(
    name="ask_resolver",
    description=(
        "Ask the resolver helper to address unresolved verifier or evaluator "
        "issues. The resolver may edit files."
    ),
    input_model=AskResolverInput,
    output_model=TextToolOutput,
)
async def ask_resolver(
    issues_to_resolve: list[str],
    issue_context: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    from engine.api import run_ephemeral_agent

    runtime_config = context.runtime_config
    if runtime_config is None:
        return ToolResult(
            output="ask_resolver: missing runtime_config in execution context.",
            is_error=True,
        )

    try:
        bundle = compose_helper_bundle(
            helper_role="resolver",
            base_agent_name="resolver",
            context=context,
        )
    except HelperComposeError as exc:
        return exc.to_tool_result()

    composed_input = bundle.rendered_prompt.rstrip() + "\n\n" + _issue_section(
        issues_to_resolve=issues_to_resolve,
        issue_context=issue_context,
    )

    result = await run_ephemeral_agent(
        runtime_config,
        composed_input,
        agent_def=bundle.agent_def,
        sandbox_id=context.sandbox_id or None,
        persist_agent_run=False,
        extra_tool_metadata=context.services_with_overrides(
            role="resolver",
            agent_type="agent",
        ),
    )
    if result.status == "failed":
        return ToolResult(output=f"ask_resolver: resolver crashed: {result.error}", is_error=True)
    if result.terminal_result is None:
        return ToolResult(
            output="ask_resolver: resolver exited without submit_resolver_result.",
            is_error=True,
        )
    terminal = result.terminal_result
    return ToolResult(
        output=terminal.output,
        is_error=terminal.is_error,
        metadata=dict(terminal.metadata or {}),
    )
