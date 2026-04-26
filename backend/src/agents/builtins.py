"""Builtin executor + evaluator agent definitions.

These two agents have secondary modes (plan_for_handoff, prepare_continue_to_work)
whose tool surfaces and briefings are too rich to express comfortably as YAML
frontmatter. They live as Python literals so the tool lists can be derived from
named constants. The mode briefings live in :mod:`agents.briefings` so they
are not duplicated between the AgentDefinition literal and any caller (tests,
docs, error messages) that wants to inspect or assert against the briefing.

The legacy ``backend/config/agents/executor.md`` and ``evaluator.md`` were
removed when this module was introduced; user-defined agents continue to load
from the YAML directory via :mod:`agents.loader`.

See ``docs/architecture/agent-mode-system-v1.md``.
"""

from __future__ import annotations

from agents.briefings import (
    PLAN_FOR_HANDOFF_BRIEFING,
    PREPARE_CONTINUE_TO_WORK_BRIEFING,
)
from agents.types import AgentDefinition, ModeDefinition

# ---------------------------------------------------------------------------
# Tool surfaces
# ---------------------------------------------------------------------------

# Read-only tools available inside the secondary modes. The list intentionally
# excludes ``shell`` (which can run arbitrary writes) and any
# subagent-spawning tool — secondary modes are deliberately read-only.
_READ_ONLY_INVESTIGATION_TOOLS: list[str] = [
    "grep",
    "glob",
    "read_file",
    "ci_query_symbol",
    "ci_diagnostics",
    "ci_workspace_structure",
]

_DIRECT_WORK_TOOLS: list[str] = [
    "grep",
    "glob",
    "read_file",
    "write_file",
    "edit_file",
    "delete_file",
    "move_file",
    "shell",
    "ci_status",
    "ci_query_symbol",
    "ci_diagnostics",
    "ci_workspace_structure",
    "run_subagent",
    "cancel_background_task",
    "check_background_task_result",
    "wait_background_tasks",
]

# ---------------------------------------------------------------------------
# Executor
# ---------------------------------------------------------------------------

_EXECUTOR_SYSTEM_PROMPT = """\
**Role**
You own one task in the executor-evaluator tree. Your job is to either complete \
the work directly or decompose it into a DAG plan that child executors can run.

**Rules to Follow**
Choose between direct completion and a plan handoff based on the task scope, \
acceptance criteria, and available evidence.

**Forbidden Actions**
Never edit test files or test suites to pass acceptance criteria. Never call \
`submit_continue_work_handoff` — that is evaluator-only.

**Task Completion**
End your turn with exactly one terminal tool call. In the default `direct` mode, \
that is `submit_task_completion` (when you can finish the work yourself). When \
the task needs decomposition, call `enter_plan_for_handoff` to switch into \
planning mode; from planning mode the only exit is `submit_plan_handoff`.
"""


EXECUTOR = AgentDefinition(
    name="executor",
    description=(
        "Owner of a task. Runs trivial work directly or hands off complex work "
        "via a DAG plan."
    ),
    role="executor",
    agent_type="agent",
    model="inherit",
    tool_call_limit=100,
    system_prompt=_EXECUTOR_SYSTEM_PROMPT,
    modes=[
        ModeDefinition(
            name="direct",
            is_default=True,
            allowed_tools=[*_DIRECT_WORK_TOOLS, "enter_plan_for_handoff"],
            terminals=["submit_task_completion"],
        ),
        ModeDefinition(
            name="plan_for_handoff",
            allowed_tools=list(_READ_ONLY_INVESTIGATION_TOOLS),
            terminals=["submit_plan_handoff"],
            entry_tool="enter_plan_for_handoff",
            briefing=PLAN_FOR_HANDOFF_BRIEFING,
        ),
    ],
)


# ---------------------------------------------------------------------------
# Evaluator
# ---------------------------------------------------------------------------

_EVALUATOR_SYSTEM_PROMPT = """\
**Role**
You are the closure gate for one handoff. After every sink task in the DAG \
passes, you read the acceptance criteria, the optional handoff note, and the \
child summaries, then decide whether the parent task can be claimed complete.

**Rules to Follow**
Decide between completion, trivial fix-then-complete, and continuation based on \
the acceptance criteria, handoff note, and child task summaries.

**Forbidden Actions**
Never edit test files or test suites to pass acceptance criteria. Never invoke \
the executor's handoff tools — those are executor-only.

**Task Completion**
End your turn with exactly one terminal tool call. In the default `direct` mode, \
that is `submit_task_completion` (criteria satisfied). When a gap remains, call \
`enter_prepare_continue_to_work` to switch into preparation mode; from \
preparation mode the only exit is `submit_continue_work_handoff`.
"""


EVALUATOR = AgentDefinition(
    name="evaluator",
    description=(
        "Closure gate for a handoff. Validates evidence, may fix trivial issues, "
        "decides task completion or continuation."
    ),
    role="evaluator",
    agent_type="agent",
    model="inherit",
    tool_call_limit=100,
    system_prompt=_EVALUATOR_SYSTEM_PROMPT,
    modes=[
        ModeDefinition(
            name="direct",
            is_default=True,
            allowed_tools=[*_DIRECT_WORK_TOOLS, "enter_prepare_continue_to_work"],
            terminals=["submit_task_completion"],
        ),
        ModeDefinition(
            name="prepare_continue_to_work",
            allowed_tools=list(_READ_ONLY_INVESTIGATION_TOOLS),
            terminals=["submit_continue_work_handoff"],
            entry_tool="enter_prepare_continue_to_work",
            briefing=PREPARE_CONTINUE_TO_WORK_BRIEFING,
        ),
    ],
)


# ---------------------------------------------------------------------------
# Explorer (subagent)
# ---------------------------------------------------------------------------

_EXPLORER_SYSTEM_PROMPT = """\
**Role**
You are a focused exploration worker. The parent agent dispatched you with a \
specific question or area to investigate; your only job is to gather the \
requested information and return your findings.

**Rules to Follow**
You operate read-only — do not modify any files, run mutating commands, or \
spawn further agents. Investigate as deeply as the prompt requires, then \
deliver one clear result.

**Task Completion**
End your turn with exactly one terminal tool call: `submit_exploration_result` \
with your `findings` as a free-form text payload. The parent receives that \
text verbatim as the result of its `run_subagent` call. Do not call any other \
terminal tool.
"""


EXPLORER = AgentDefinition(
    name="explorer",
    description=(
        "Read-only exploration subagent. Investigates a focused question and "
        "returns its findings to the dispatching parent agent."
    ),
    role="explorer",
    agent_type="subagent",
    model="inherit",
    tool_call_limit=50,
    system_prompt=_EXPLORER_SYSTEM_PROMPT,
    modes=[
        ModeDefinition(
            name="direct",
            is_default=True,
            allowed_tools=list(_READ_ONLY_INVESTIGATION_TOOLS),
            terminals=["submit_exploration_result"],
        ),
    ],
)


# ---------------------------------------------------------------------------
# Registration
# ---------------------------------------------------------------------------


BUILTIN_AGENTS: tuple[AgentDefinition, ...] = (EXECUTOR, EVALUATOR, EXPLORER)


def register_builtin_agents() -> None:
    """Register the executor, evaluator, and explorer definitions.

    Idempotent — safe to call from multiple bootstrap paths (server lifespan,
    test fixtures, CLI helpers).
    """
    from agents.registry import register_definition

    for defn in BUILTIN_AGENTS:
        register_definition(defn)


__all__ = [
    "BUILTIN_AGENTS",
    "EVALUATOR",
    "EXECUTOR",
    "EXPLORER",
    # Re-exported from ``agents.briefings`` for ergonomic test imports.
    "PLAN_FOR_HANDOFF_BRIEFING",
    "PREPARE_CONTINUE_TO_WORK_BRIEFING",
    "register_builtin_agents",
]
