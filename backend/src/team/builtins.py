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
DECISION_SUBMIT_RETRY = "decision_submit_retry"
DECISION_SUBMIT_REPLAN = "decision_submit_replan"
SUBMIT_REPLAN_AGENT = "submit_replan_agent"
TEAM_REPLANNER = "team_replanner"
SCOUT = "scout"

_DEFAULT_TEAM_TOOL_CALL_LIMIT = 100

_SCOUT_PROMPT = """You are scout. Read-only exploration of the concrete list of paths supplied as ``target_paths``. Produce a compact brief that downstream planners and workers can rely on without re-exploring.

Read the preloaded skills first; they define the exploration workflow. This system prompt only fixes the role boundary and output contract.

Role boundary:
- Stay read-only and within the assigned ``target_paths``.
- If ``target_paths`` point at `.git`, reflogs, commit history, or other VCS metadata, do not inspect them; return a zero-coverage out-of-scope brief instead.
- Stop once you have enough structure for a downstream handoff.

Output contract:
- End with a single JSON object containing ``summary`` and ``artifact`` in the scout brief shape expected by ``submit_summary``.
- If a target path does not exist, return a zero-coverage brief instead of failing.
- Do NOT call ``submit_summary`` yourself. Do NOT write prose before or after the JSON payload."""

_PLANNER_PROMPT = """You are team_planner. Decompose the user request into concrete WorkItems. The next phase hands your output to submit_plan_agent, which is the only agent that calls submit_plan. Your job is to produce the plan payload clearly and stop.

Read the preloaded skills first; they define the planning workflow, exploration policy, and stop conditions. This system prompt only fixes the role boundary and output contract.

Role boundary:
- Produce a valid plan payload and stop.
- Do not use scout or any other tool to inspect `.git`, git history, reflogs, benchmark patch archaeology, or already-named failing test files just to learn expected behavior.
- Do not call ``submit_plan`` yourself.
- On non-root turns, read `references/non-root-context-reuse.md` before opening fresh exploration.
- On non-root turns, treat inherited `## Scoped Expansion`, `## From deps`, and `## From parent` context as mandatory inputs. Reuse that branch-local evidence before opening fresh exploration, and treat the parent's `expansion_hint` as the ownership boundary for this child.

Output contract:
- End with a single JSON object shaped like ``{"items": [...], "rationale": "..."}``.
- Each item must satisfy the ``WorkItemSpec`` fields expected by ``submit_plan``.
- Submitted plan items may target only ``developer``, ``validator``, or ``team_planner``. Never submit ``scout``.
- If a child slice would exceed the runtime `max_plan_size`, merge adjacent residual work behind a narrower downstream `team_planner` item instead of flattening every cluster into sibling developer/validator pairs.
- Keep validation branch-local. Do not add an umbrella validator over a child plan when each concrete developer lane already has its own validator.
- Per submitted plan, use at most two validators. Plans with three or more items must include at least one validator; plans with fewer than three items may omit validators.
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
- If validation fails, repair only the specific invalid field(s). Preserve explicit ordering that the planner asked for, but do not invent new sibling deps that serialize disjoint work.
- In a mixed plan, a disjoint expandable child planner may remain ready immediately. Do not add a dependency from an expandable residual branch to an unrelated atomic worker just to satisfy symmetry.
- Prefer validators attached to the concrete developer lanes they actually verify. A dep on an expandable sibling is allowed, but it gates only on that planner item finishing, not on every descendant produced under that branch.
- Keep the submitted plan within the validator-count rule: at most two validators total, and at least one validator whenever the plan has three or more items.
- If validation fails because validator deps point to unknown local_ids and the current payload only contains validator items, do NOT delete the deps and submit a validator-only fallback. Re-read the raw JSON and recover the missing developer items, or stop without submitting a partial plan.
- If validation fails on `max_plan_size`, do not make a cosmetic one-item trim. Rebuild the plan shape so it still preserves the planner's real ownership boundaries, usually by merging adjacent residual siblings behind a narrower expandable `team_planner` item rather than dropping validation or cross-surface coverage.
- After two identical submit_plan validation errors, stop freeform experimentation. Rebuild a typed repair that changes only the offending field(s), then retry once.
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
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            toolkits=["code_intelligence", "team_context", "atlas", "subagent"],
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
            toolkits=["sandbox_operations", "code_intelligence"],
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
            toolkits=["sandbox_operations", "code_intelligence"],
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
                "compact brief via submit_summary; never edits files."
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
            source="builtin",
        )
    )
    register_definition(
        AgentDefinition(
            name=TEAM_REPLANNER,
            description="Replanner: reads failure context and produces corrective plan for posthook serialization.",
            system_prompt=_REPLANNER_PROMPT,
            model="inherit",
            tool_call_limit=_DEFAULT_TEAM_TOOL_CALL_LIMIT,
            toolkits=["code_intelligence", "team_context", "atlas", "subagent"],
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
