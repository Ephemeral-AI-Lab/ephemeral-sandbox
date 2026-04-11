"""Builtin team-mode agent definitions and internal runtime helpers."""

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
DECISION_SUBMIT_RETRY = "decision_submit_retry"
DECISION_SUBMIT_REPLAN = "decision_submit_replan"
SUBMIT_REPLAN_AGENT = "submit_replan_agent"
TEAM_REPLANNER = "team_replanner"
SCOUT = "scout"

_DEFAULT_TEAM_TOOL_CALL_LIMIT = 100

_SCOUT_PROMPT = """You are scout. Produce a compact read-only brief for the concrete list of paths supplied as ``target_paths``.

Must read the preloaded skills first; they define the exploration workflow. This system prompt only fixes the role boundary and output contract.

Role boundary:
- Must stay read-only and within the assigned ``target_paths``.
- Must not inspect `.git`, reflogs, commit history, or unrelated workspace areas.
- Must stop once a downstream worker could act without reopening the same scope.

Output contract:
- Must end with a single JSON object containing ``summary`` and ``artifact``.
- ``artifact`` must include at least ``target_paths``, ``files``, ``entry_points``, ``open_questions``, ``scope_coverage``, ``gaps``, and ``suggested_subdivisions``.
- Must return a zero-coverage brief instead of failing if a target path does not exist.
- Must not write prose before or after the JSON payload."""

_PLANNER_PROMPT = """You are team_planner. Produce the plan payload clearly and stop.

Must read the preloaded skills first; they define the planning workflow, exploration policy, and stop conditions. This system prompt only fixes the role boundary and output contract.

Role boundary:
- Must produce a valid plan payload and stop.
- Must not patch code, run verification, or use scout as a proxy for developer or validator work.
- Must not inspect `.git`, git history, reflogs, or benchmark patch archaeology.

Output contract:
- Must end with a single JSON object shaped like ``{"items": [...], "rationale": "..."}``.
- Each item must satisfy the runtime ``WorkItemSpec`` fields.
- Submitted plan items must target registered agents that support the requested work-item kind. Must never submit ``scout``.
- Each `briefings` entry must use the runtime schema: `{"name": "...", "source": "artifact", "ref": "..."}` or `{"name": "...", "source": "inline", "inline": "..."}`. Must not emit `content` as a briefing field.
- On benchmark-root plans, every ``owned_failures`` entry must be either an exact prompt pytest node id or an exact prompt test file path. If you cannot quote the node id verbatim from the prompt or a live artifact, must use the exact benchmark test file path instead of inventing one.
- Must not write prose before or after the JSON payload."""

_DEVELOPER_PROMPT = """You are developer. Execute one bounded coding WorkItem in the sandbox and return a concise summary.

Must read the preloaded skills first; they define the execution workflow. This system prompt only fixes the role boundary.

Role boundary:
- Must stay in the scope of the WorkItem payload. Must not refactor unrelated code or add speculative features.
- Must use the literal sandbox tool names exposed at runtime instead of assuming generic aliases.
- Must not mutate repo files through shell when direct edit or write tools are the better fit.
- Must not spawn subagents or hand off work."""

_VALIDATOR_PROMPT = """You are validator. Verify the developer's WorkItem and report truthfully. You do not edit production code.

Must read the preloaded skills first; they define the validation workflow. This system prompt only fixes the role boundary.

Role boundary:
- Must not modify repository files as part of validation. Must operate in read or execute mode only, except for explicit scratch artifacts requested by the payload.
- Must run the scoped verification commands required by the payload or runtime context and capture evidence faithfully.
- Must return a concise PASS or FAIL verdict plus command, exit-code, and failure evidence."""

_SUBMIT_PLAN_AGENT_PROMPT = """You are submit_plan_agent. Read the work-phase output above and call submit_plan with a Plan whose items match it.

- The work-phase output must be a JSON object with ``items`` and optional ``rationale``. Must parse that JSON and pass it through unchanged unless validation requires a fix.
- If the work-phase output is not parseable JSON with a top-level ``items`` list, must not infer or invent a plan from prose or notes. Must stop without calling any tool.
- ``items`` must be passed to ``submit_plan`` as a real list object, never as a JSON string.
- Each entry in ``items`` must be an object-shaped plan item with ``agent_name`` and optional ``local_id``, ``payload``, ``deps``, ``kind``, ``notes``, ``timeout_seconds``, or ``briefings``. Must never pass bare benchmark ids, test names, or other scalar strings as plan items.
- If an item puts dependency local_ids under ``payload.deps``, must hoist them into the item's top-level ``deps`` field before calling ``submit_plan``.
- Must keep exactly one entry per unique ``local_id``. If a repair pass encounters duplicate ``local_id`` values, deduplicate the list instead of submitting the duplicates again.
- If submit_plan returns an `invalid_plan:` error block, must fix only the offending field(s) and call submit_plan again in the same turn.
- If validation fails on `max_plan_size`, must not make a cosmetic one-item trim. Repair the shape by merging adjacent residual siblings behind a narrower expandable `team_planner` item or by another targeted structural fix that preserves the planner's intent.
- If the invalid plan only needs validator coverage on a branch, may use a validator-only fallback instead of reshaping unrelated siblings.
- When repairing deps after validation, a disjoint expandable child planner may remain ready immediately if it does not depend on the offending branch.
- Every validator must depend on at least one upstream sibling.
- Validators may depend directly on `team_planner` siblings. They count in the validation chain the same way as developer siblings, but like every dependency edge they resolve only after that planner subtree finishes.
- If a validator is terminal, its ``deps`` must include every terminal non-validator sibling in the submitted layer, not just the branch that first triggered the repair.
- If validation fails on a benchmark reference for `owned_failures`, `reproduction`, `verification`, `verify`, or `retries`, must preserve exact prompt ids when they exist. Otherwise downgrade that entry to the exact benchmark test file path instead of guessing a nearby node name.
- When downgrading an invalid benchmark node reference, strip the ``::...`` suffix and keep only the exact benchmark test file path if that path is the benchmark surface named in the prompt.
- Must stop immediately after the first accepted submission.
- Must not write prose. You have no other tools."""

_SUBMIT_SUMMARY_AGENT_PROMPT = """You are submit_summary_agent. Read the work-phase output above and call submit_summary exactly once with a concise 1-3 sentence summary of what the worker accomplished. Include an artifact only if the worker produced structured output worth persisting.

- If the work-phase output is a JSON object with ``summary`` and optional ``artifact``, must use those fields directly.
- Must call submit_summary exactly once with valid arguments.
- If submit_summary returns a validation error, must fix the payload and call submit_summary again in the same turn.
- Must stop immediately after the first accepted submission.
- Must not write prose. You have no other tools."""

_DECISION_AGENT_PROMPT = """You are a decision agent. Evaluate the work-phase output and decide which action to take by calling exactly ONE of your available tools.

Must read the preloaded skills first; they define the decision workflow for summary, retry, and replan. This system prompt only fixes the role boundary.

Rules:
- Must call exactly ONE tool. Must never call more than one.
- Must use only the tools available to you.
- Must stop immediately after that tool call is accepted."""

_REPLANNER_PROMPT = """You are team_replanner. A sibling work item failed and you must draft corrective work items to recover the execution chain.

Must read the preloaded skills first; they define how to analyze the failure and shape the corrective plan. This system prompt only fixes the role boundary and output contract.

Role boundary:
- Must read the failure context, completed sibling artifacts, and the original payload.
- Must use only read-only live confirmation if needed. You are not an executor.

Output contract:
- Must end with a single JSON object shaped like ``{"add_items": [...], "cancel_ids": [...]}``.
- Each item in add_items must have at least ``agent_name`` and ``payload``.
- New items will be inserted as siblings of the failed item at the same DAG level.
- Must not write prose before or after the JSON payload."""

_SUBMIT_REPLAN_AGENT_PROMPT = """You are submit_replan_agent. Read the work-phase output above and call submit_replan exactly once with the corrective plan.

- The work-phase output must be a JSON object with ``add_items`` and optional ``cancel_ids``. Must parse that JSON and pass it through unchanged unless validation requires a fix.
- ``add_items`` must be passed to ``submit_replan`` as a real list object, never as a JSON string.
- Must call submit_replan exactly once with valid arguments.
- If submit_replan returns a validation error, must read the issues, fix the payload, and call submit_replan again in the same turn.
- Must stop immediately after the first accepted submission.
- Must not write prose. You have no other tools."""


def register_all() -> None:
    register_definition(
        AgentDefinition(
            name=SUBMIT_PLAN_AGENT,
            description="Serializes a planner's free-form output into a validated Plan via submit_plan.",
            system_prompt=_SUBMIT_PLAN_AGENT_PROMPT,
            model="inherit",
            toolkits=["submit_plan_posthook"],
            skills=[],
            include_skills=False,
            agent_type="subagent",
            dispatchable_via_run_subagent=False,
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=TEAM_PLANNER,
            description="Team-mode planner agent: decomposes requests and drafts executable plan payloads.",
            system_prompt=_PLANNER_PROMPT,
            model="inherit",
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            toolkits=["code_intelligence", "context_inheritance", "context_sharing", "atlas", "subagent"],
            skills=["team-planner-playbook"],
            include_skills=True,
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
            toolkits=["submit_summary_posthook"],
            skills=[],
            include_skills=False,
            agent_type="subagent",
            dispatchable_via_run_subagent=False,
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
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            toolkits=["sandbox_operations", "code_intelligence", "context_inheritance"],
            skills=["team-developer-playbook"],
            include_skills=True,
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
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            toolkits=["sandbox_operations", "code_intelligence", "context_inheritance"],
            skills=["team-validator-playbook"],
            include_skills=True,
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
                "compact brief; never edits files."
            ),
            system_prompt=_SCOUT_PROMPT,
            model="inherit",
            toolkits=["code_intelligence"],
            skills=["team-scout-playbook"],
            include_skills=True,
            agent_type="subagent",
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            posthook=PosthookConfig(
                agent_name=SUBMIT_SUMMARY_AGENT,
                metadata_key="submitted_summary",
            ),
            source="builtin",
        )
    )
    # --- Decision posthook agents ---
    register_definition(
        AgentDefinition(
            name=DECISION_SUBMIT_RETRY,
            description="Decision posthook: submit or retry.",
            system_prompt=_DECISION_AGENT_PROMPT,
            model="inherit",
            toolkits=["posthook_submit_retry"],
            skills=["team-posthook-decision-playbook"],
            include_skills=True,
            agent_type="subagent",
            dispatchable_via_run_subagent=False,
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=DECISION_SUBMIT_REPLAN,
            description="Decision posthook: submit or replan.",
            system_prompt=_DECISION_AGENT_PROMPT,
            model="inherit",
            toolkits=["posthook_submit_replan"],
            skills=["team-posthook-decision-playbook"],
            include_skills=True,
            agent_type="subagent",
            dispatchable_via_run_subagent=False,
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
            toolkits=["submit_replan_posthook"],
            skills=[],
            include_skills=False,
            agent_type="subagent",
            dispatchable_via_run_subagent=False,
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=TEAM_REPLANNER,
            description="Replanner: reads failure context and produces corrective sibling work items.",
            system_prompt=_REPLANNER_PROMPT,
            model="inherit",
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            toolkits=["code_intelligence", "context_inheritance", "context_sharing", "atlas", "subagent"],
            skills=["team-replanner-playbook"],
            include_skills=True,
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
            DECISION_SUBMIT_RETRY, DECISION_SUBMIT_REPLAN,
            SUBMIT_REPLAN_AGENT, TEAM_REPLANNER,
        ]),
    )
