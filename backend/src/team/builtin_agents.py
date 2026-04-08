"""Builtin team_planner / team_worker / submit_plan_agent definitions."""

from __future__ import annotations

import logging

from agents.registry import register_definition
from agents.types import AgentDefinition
from hooks.agent_posthook import PosthookConfig
from tools.core.factory import register_standalone_tool
from tools.posthook import SubmitPlanTool, SubmitSummaryTool

logger = logging.getLogger(__name__)

TEAM_PLANNER = "team_planner"
TEAM_WORKER = "team_worker"
SUBMIT_PLAN_AGENT = "submit_plan_agent"
SUBMIT_SUMMARY_AGENT = "submit_summary_agent"

_PLANNER_PROMPT = """You are team_planner. Decompose the user request into concrete WorkItems.
Think clearly, reference the user request, and produce a structured plan.
The next phase will hand your output to submit_plan_agent, which will call
submit_plan, so be explicit about dependencies between items."""

_WORKER_PROMPT = """You are team_worker. Execute the specific WorkItem described in the payload. Return a concise summary and any artifacts. Use the team context tools (team_list_siblings, team_files_changed_since_dispatch) to stay aware of peer work."""

_SUBMIT_PLAN_AGENT_PROMPT = """You are submit_plan_agent. Read the work-phase output above and call submit_plan exactly once with a Plan whose items match it.

- Call submit_plan exactly once with valid arguments.
- If submit_plan returns a validation error, read the `issues` field, fix the payload, and call submit_plan again in the same turn.
- Stop immediately after the first accepted submission.
- Do not write prose. You have no other tools."""

_SUBMIT_SUMMARY_AGENT_PROMPT = """You are submit_summary_agent. Read the work-phase output above and call submit_summary exactly once with a concise 1-3 sentence summary of what the worker accomplished. Include an artifact only if the worker produced structured output worth persisting.

- Call submit_summary exactly once with valid arguments.
- If submit_summary returns a validation error, fix the payload and call submit_summary again in the same turn.
- Stop immediately after the first accepted submission.
- Do not write prose. You have no other tools."""


def register_all() -> None:
    register_standalone_tool("submit_plan", SubmitPlanTool)
    register_standalone_tool("submit_summary", SubmitSummaryTool)
    register_definition(
        AgentDefinition(
            name=SUBMIT_PLAN_AGENT,
            description="Serializes a planner's free-form output into a validated Plan via submit_plan.",
            system_prompt=_SUBMIT_PLAN_AGENT_PROMPT,
            model="inherit",
            max_turns=5,
            toolkits=[],
            skills=[],
            extra_tools=["submit_plan"],
            include_skills=False,
            agent_type="subagent",
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=TEAM_PLANNER,
            description="Team-mode planner agent: decomposes requests and submits Plans.",
            system_prompt=_PLANNER_PROMPT,
            model="inherit",
            max_turns=10,
            toolkits=["code_intelligence"],
            source="builtin",
            posthook=PosthookConfig(
                agent_name=SUBMIT_PLAN_AGENT,
                metadata_key="submitted_plan",
            ),
        )
    )
    register_definition(
        AgentDefinition(
            name=SUBMIT_SUMMARY_AGENT,
            description="Serializes a worker's free-form output into a validated SubmittedSummary via submit_summary.",
            system_prompt=_SUBMIT_SUMMARY_AGENT_PROMPT,
            model="inherit",
            max_turns=5,
            toolkits=[],
            skills=[],
            extra_tools=["submit_summary"],
            include_skills=False,
            agent_type="subagent",
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=TEAM_WORKER,
            description="Team-mode worker agent: executes one WorkItem with full toolkit.",
            system_prompt=_WORKER_PROMPT,
            model="inherit",
            max_turns=15,
            toolkits=["sandbox_operations", "code_intelligence"],
            source="builtin",
            posthook=PosthookConfig(
                agent_name=SUBMIT_SUMMARY_AGENT,
                metadata_key="submitted_summary",
            ),
        )
    )
    logger.info(
        "team builtins registered: %s, %s, %s, %s",
        TEAM_PLANNER,
        TEAM_WORKER,
        SUBMIT_PLAN_AGENT,
        SUBMIT_SUMMARY_AGENT,
    )
