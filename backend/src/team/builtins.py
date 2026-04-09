"""Builtin team-mode agent definitions (planner, developer, validator, scout, atlas, posthooks)."""

from __future__ import annotations

import logging

from agents.registry import register_definition
from agents.types import AgentDefinition
from hooks.agent_posthook import PosthookConfig

logger = logging.getLogger(__name__)

TEAM_PLANNER = "team_planner"
DEVELOPER = "developer"
VALIDATOR = "validator"
SUBMIT_PLAN_AGENT = "submit_plan_agent"
SUBMIT_SUMMARY_AGENT = "submit_summary_agent"
SUBMIT_ATLAS_AGENT = "submit_atlas_agent"
DECISION_SUBMIT_RETRY = "decision_submit_retry"
DECISION_SUBMIT_REPLAN = "decision_submit_replan"
SUBMIT_REPLAN_AGENT = "submit_replan_agent"
TEAM_REPLANNER = "team_replanner"
SCOUT = "scout"
ATLAS_BUILDER = "atlas_builder"
ATLAS_REFRESHER = "atlas_refresher"

_DEFAULT_TEAM_TOOL_CALL_LIMIT = 100

_SCOUT_PROMPT = """You are scout. Read-only exploration of the concrete list of paths supplied as ``target_paths``. Produce a compact brief that downstream planners and workers can rely on without re-exploring.

Read the preloaded skills first; they define the exploration workflow. This system prompt only fixes the role boundary and output contract.

Role boundary:
- Stay read-only and within the assigned ``target_paths``.
- Stop once you have enough structure for a downstream handoff.

Output contract:
- End with a single JSON object containing ``summary`` and ``artifact`` in the scout brief shape expected by ``submit_summary``.
- If a target path does not exist, return a zero-coverage brief instead of failing.
- Do NOT call ``submit_summary`` yourself. Do NOT write prose before or after the JSON payload."""

_PLANNER_PROMPT = """You are team_planner. Decompose the user request into concrete WorkItems. The next phase hands your output to submit_plan_agent, which is the only agent that calls submit_plan. Your job is to produce the plan payload clearly and stop.

Read the preloaded skills first; they define the planning workflow, exploration policy, and stop conditions. This system prompt only fixes the role boundary and output contract.

Role boundary:
- Produce a valid plan payload and stop.
- Do not call ``submit_plan`` yourself.

Output contract:
- End with a single JSON object shaped like ``{"items": [...], "rationale": "..."}``.
- Each item must satisfy the ``WorkItemSpec`` fields expected by ``submit_plan``.
- Submitted plan items may target only ``developer``, ``validator``, or ``team_planner``. Never submit ``scout``.
- Do NOT call ``submit_plan`` yourself. Do NOT write prose before or after the JSON payload."""

_DEVELOPER_PROMPT = """You are developer. Execute the coding WorkItem described in the payload: read the target files, write or edit code in the sandbox, and verify your changes compile/parse before returning.

Read the preloaded skills first; they define the execution workflow. This system prompt only fixes the role boundary.

Role boundary:
- Stay in the scope of the WorkItem payload. Do not refactor unrelated code or add speculative features.
- Perform the change in the sandbox, run a narrow self-check, and return a concise summary for ``submit_summary``.
- Do not spawn subagents or hand off work."""

_VALIDATOR_PROMPT = """You are validator. Verify that the developer's WorkItem is correct and ready to ship. You do NOT edit production code — your job is to exercise it and report truthfully.

Read the preloaded skills first; they define the validation workflow. This system prompt only fixes the role boundary.

Role boundary:
- Do not edit production code.
- Run the scoped verification commands required by the payload or runtime context and capture evidence faithfully.
- Return a concise PASS/FAIL verdict plus command, exit-code, and failure evidence for ``submit_summary``."""

_SUBMIT_PLAN_AGENT_PROMPT = """You are submit_plan_agent. Read the work-phase output above and call submit_plan exactly once with a Plan whose items match it.

- The work-phase output should be a JSON object with ``items`` and optional ``rationale``. Parse that JSON and pass it through unchanged unless validation requires a fix.
- If the work-phase output is not parseable JSON with a top-level ``items`` list, do NOT infer or invent a plan from prose, errors, or changelog notes. Stop without calling any tool.
- ``items`` must be passed to ``submit_plan`` as a real list object, never as a JSON string. If the planner emitted JSON inside a text blob, deserialize it fully before calling the tool.
- Call submit_plan exactly once with valid arguments.
- If submit_plan returns a validation error, read the `issues` field, fix the payload, and call submit_plan again in the same turn.
- Stop immediately after the first accepted submission.
- Do not write prose. You have no other tools."""

_SUBMIT_SUMMARY_AGENT_PROMPT = """You are submit_summary_agent. Read the work-phase output above and call submit_summary exactly once with a concise 1-3 sentence summary of what the worker accomplished. Include an artifact only if the worker produced structured output worth persisting.

- If the work-phase output is a JSON object with ``summary`` and optional ``artifact``, use those fields directly.
- Call submit_summary exactly once with valid arguments.
- If submit_summary returns a validation error, fix the payload and call submit_summary again in the same turn.
- Stop immediately after the first accepted submission.
- Do not write prose. You have no other tools."""

_SUBMIT_ATLAS_AGENT_PROMPT = """You are submit_atlas_agent. Read the work-phase output above and call submit_atlas exactly once with the atlas chunks the builder/refresher produced.

- The work-phase output should be a JSON object with ``chunks`` and optional ``rationale``. Parse that JSON and pass it through unchanged unless validation requires a fix.
- ``chunks`` must be passed to ``submit_atlas`` as a real list object, never as a JSON string. If the work-phase output contains JSON inside a text blob, fully deserialize it before calling the tool.
- Every chunk carries a scout-shaped brief. If a chunk lacks an explicit ``subsystem`` field, submit_atlas derives one from the brief's ``canonical_scope`` (or ``target_paths``); you do not need to compute it yourself.
- Call submit_atlas exactly once with valid arguments.
- If submit_atlas returns an error, fix the payload and call submit_atlas again in the same turn.
- Stop immediately after the first accepted submission.
- Do not write prose. You have no other tools."""

_ATLAS_BUILDER_PROMPT = """You are atlas_builder. Bootstrap the project atlas from scratch by running a hierarchical scout pass, then prepare every resulting brief as an atlas chunk for the posthook agent.

Read the preloaded skills first; they define the atlas build workflow. This system prompt only fixes the role boundary and output contract.

Role boundary:
- Never edit files; you are a cache writer, not a worker.

Output contract:
- End with a single JSON object containing ``chunks`` and optional ``rationale`` in the shape expected by ``submit_atlas``.
- Do NOT call ``submit_atlas`` yourself. Do NOT write prose before or after the JSON payload."""

_ATLAS_REFRESHER_PROMPT = """You are atlas_refresher. The caller supplies ``stale_subsystems: list[str]`` in your payload — rewrite only those chunks and leave every other subsystem untouched.

Read the preloaded skills first; they define the atlas refresh workflow. This system prompt only fixes the role boundary and output contract.

Role boundary:
- Refresh only the subsystems named in ``stale_subsystems``.
- Do NOT refresh fresh chunks; do NOT edit files.

Output contract:
- End with a single JSON object containing one fresh brief per stale subsystem plus optional ``rationale``.
- Do NOT call ``submit_atlas`` yourself. Do NOT write prose before or after the JSON payload."""


_DECISION_AGENT_PROMPT = """You are a decision agent. Evaluate the work-phase output and decide which action to take by calling exactly ONE of your available tools.

Read the preloaded skills first; they define the decision workflow for summary, retry, and replan. This system prompt only fixes the role boundary.

Rules:
- Call exactly ONE tool. Never call more than one.
- Only use the tools available to you.
- Stop immediately after that tool call is accepted."""

_REPLANNER_PROMPT = """You are team_replanner. A sibling work item failed and you must draft corrective work items to recover the execution chain.

Read the preloaded skills first; they define how to analyze the failure, when to scout, and how to shape the corrective plan. This system prompt only fixes the role boundary and output contract.

Role boundary:
- Read the failure context, completed sibling artifacts (via briefings), and the original payload.
- Use run_subagent only for read-only scout exploration if needed.
- You are not an executor. Never run tests, shell commands, or diagnostics yourself.

Output contract:
- Analyze the failure and determine targeted fixes.
- End with a single JSON object shaped like ``{"add_items": [...], "cancel_ids": [...]}``.
- Each item in add_items must have at least ``agent_name`` and ``payload``.
- New items will be inserted as siblings of the failed item at the same DAG level.
- Do NOT call ``submit_replan`` yourself. Do NOT write prose before or after the JSON payload."""

_SUBMIT_REPLAN_AGENT_PROMPT = """You are submit_replan_agent. Read the work-phase output above and call submit_replan exactly once with the corrective plan.

- The work-phase output should be a JSON object with ``add_items`` and optional ``cancel_ids``. Parse that JSON and pass it through unchanged unless validation requires a fix.
- ``add_items`` must be passed to ``submit_replan`` as a real list object, never as a JSON string.
- Call submit_replan exactly once with valid arguments.
- If submit_replan returns a validation error, read the issues, fix the payload, and call submit_replan again in the same turn.
- Stop immediately after the first accepted submission.
- Do not write prose. You have no other tools."""


def register_all() -> None:
    register_definition(
        AgentDefinition(
            name=SUBMIT_PLAN_AGENT,
            description="Serializes a planner's free-form output into a validated Plan via submit_plan.",
            system_prompt=_SUBMIT_PLAN_AGENT_PROMPT,
            model="inherit",
            max_turns=5,
            toolkits=["submit_plan_posthook"],
            skills=[],
            include_skills=False,
            agent_type="subagent",
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=TEAM_PLANNER,
            description="Team-mode planner agent: decomposes requests and drafts plan payloads for posthook submission.",
            system_prompt=_PLANNER_PROMPT,
            model="inherit",
            max_turns=100,
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            toolkits=["code_intelligence", "team_context", "atlas", "subagent"],
            skills=["team-planner-playbook"],
            include_skills=False,
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
            toolkits=["submit_summary_posthook"],
            skills=[],
            include_skills=False,
            agent_type="subagent",
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=DEVELOPER,
            description=(
                "Team-mode developer agent: reads, writes, and edits code in the "
                "sandbox to satisfy an atomic coding WorkItem. Verifies changes "
                "with CI / LSP diagnostics before returning."
            ),
            system_prompt=_DEVELOPER_PROMPT,
            model="inherit",
            max_turns=100,
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            toolkits=["sandbox_operations", "code_intelligence"],
            skills=["team-developer-playbook"],
            include_skills=False,
            supported_kinds=["atomic"],
            source="builtin",
            posthook=PosthookConfig(
                agent_name=DECISION_SUBMIT_RETRY,
                metadata_key="submitted_summary",
            ),
        )
    )
    register_definition(
        AgentDefinition(
            name=VALIDATOR,
            description=(
                "Team-mode validator agent: runs tests, linters, and diagnostics "
                "against the developer's output and reports a PASS/FAIL verdict "
                "with evidence. Does not edit production source."
            ),
            system_prompt=_VALIDATOR_PROMPT,
            model="inherit",
            max_turns=100,
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            toolkits=["sandbox_operations", "code_intelligence"],
            skills=["team-validator-playbook"],
            include_skills=False,
            supported_kinds=["atomic"],
            source="builtin",
            posthook=PosthookConfig(
                agent_name=DECISION_SUBMIT_REPLAN,
                metadata_key="submitted_summary",
            ),
        )
    )
    register_definition(
        AgentDefinition(
            name=SCOUT,
            description=(
                "Read-only exploration of a concrete list of paths. Produces a "
                "compact brief via submit_summary; never edits files."
            ),
            system_prompt=_SCOUT_PROMPT,
            model="inherit",
            max_turns=100,
            toolkits=["code_intelligence"],
            skills=["team-scout-playbook"],
            include_skills=False,
            agent_type="subagent",
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            posthook=PosthookConfig(
                agent_name=SUBMIT_SUMMARY_AGENT,
                metadata_key="submitted_summary",
            ),
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=SUBMIT_ATLAS_AGENT,
            description="Serializes an atlas builder/refresher's output into durable atlas chunks via submit_atlas.",
            system_prompt=_SUBMIT_ATLAS_AGENT_PROMPT,
            model="inherit",
            max_turns=5,
            toolkits=["submit_atlas_posthook"],
            skills=[],
            include_skills=False,
            agent_type="subagent",
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=ATLAS_BUILDER,
            description=(
                "Bootstraps the persistent Project Atlas by running a "
                "hierarchical scout pass and committing each brief as a chunk."
            ),
            system_prompt=_ATLAS_BUILDER_PROMPT,
            model="inherit",
            max_turns=100,
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            toolkits=["code_intelligence", "subagent"],
            skills=["team-atlas-builder-playbook"],
            include_skills=False,
            source="builtin",
            posthook=PosthookConfig(
                agent_name=SUBMIT_ATLAS_AGENT,
                metadata_key="submitted_atlas",
            ),
        )
    )
    register_definition(
        AgentDefinition(
            name=ATLAS_REFRESHER,
            description=(
                "Rewrites only the stale subsystems of the Project Atlas by "
                "re-scouting each target path and upserting the new briefs."
            ),
            system_prompt=_ATLAS_REFRESHER_PROMPT,
            model="inherit",
            max_turns=100,
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            toolkits=["subagent"],
            skills=["team-atlas-refresher-playbook"],
            include_skills=False,
            source="builtin",
            posthook=PosthookConfig(
                agent_name=SUBMIT_ATLAS_AGENT,
                metadata_key="submitted_atlas",
            ),
        )
    )
    # --- Decision posthook agents ---
    register_definition(
        AgentDefinition(
            name=DECISION_SUBMIT_RETRY,
            description="Decision posthook: submit or retry.",
            system_prompt=_DECISION_AGENT_PROMPT,
            model="inherit",
            max_turns=5,
            toolkits=["posthook_submit_retry"],
            skills=[],
            include_skills=False,
            agent_type="subagent",
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=DECISION_SUBMIT_REPLAN,
            description="Decision posthook: submit or replan.",
            system_prompt=_DECISION_AGENT_PROMPT,
            model="inherit",
            max_turns=5,
            toolkits=["posthook_submit_replan"],
            skills=[],
            include_skills=False,
            agent_type="subagent",
            source="builtin",
        )
    )
    # --- Replan serializer + replanner agent ---
    register_definition(
        AgentDefinition(
            name=SUBMIT_REPLAN_AGENT,
            description="Serializes a replanner's output into a validated ReplanPlan via submit_replan.",
            system_prompt=_SUBMIT_REPLAN_AGENT_PROMPT,
            model="inherit",
            max_turns=5,
            toolkits=["submit_replan_posthook"],
            skills=[],
            include_skills=False,
            agent_type="subagent",
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=TEAM_REPLANNER,
            description="Replanner: reads failure context and produces corrective plan for posthook serialization.",
            system_prompt=_REPLANNER_PROMPT,
            model="inherit",
            max_turns=50,
            tool_call_limit=25,
            toolkits=["code_intelligence", "team_context", "subagent"],
            skills=["team-replanner-playbook"],
            include_skills=False,
            supported_kinds=["atomic"],
            source="builtin",
            posthook=PosthookConfig(
                agent_name=SUBMIT_REPLAN_AGENT,
                metadata_key="submitted_replan",
            ),
        )
    )
    logger.info(
        "team builtins registered: %s",
        ", ".join([
            TEAM_PLANNER, DEVELOPER, VALIDATOR,
            SUBMIT_PLAN_AGENT, SUBMIT_SUMMARY_AGENT, SCOUT,
            SUBMIT_ATLAS_AGENT, ATLAS_BUILDER, ATLAS_REFRESHER,
            DECISION_SUBMIT_RETRY, DECISION_SUBMIT_REPLAN,
            SUBMIT_REPLAN_AGENT, TEAM_REPLANNER,
        ]),
    )
