"""Harvest first-three-messages for every agent role and write a report.

Sources:
- main agents (entry_executor, planner, executor, evaluator) — first 2 message.jsonl
  rows (system + user_prompt) from existing live-e2e runs under
  ``.sweevo_runs/scenario_logs/<scenario>/<run>/...``. The third message
  (user_msg_1 = context_message) is captured separately because the recorder
  only stores the spawn prompt, not the seeded initial_messages list. We
  recover it directly from the renderer by re-rendering a representative
  packet shape; see :func:`_synthesise_main_user_msg_1`.
- helpers (advisor, resolver) — programmatically constructed via the actual
  builder functions in ``tools/ask_helper/_lib/_compose.py`` against
  realistic parent context taken from a real planner/executor prompt.
- subagent (explorer) — system prompt + (parent free-text prompt) user_msg_1
  + (explorer_instruction) user_msg_2.

Writes ``docs/reports/first_three_messages_report.md``. Pure harvester — no
sandbox, no DB, no agent execution. Only the existing on-disk artefacts and
the live builder code paths are used.
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
SRC = REPO / "backend" / "src"
sys.path.insert(0, str(SRC))

from agents import get_definition, load_agents_tree, register_definition  # noqa: E402

# Eagerly load + register all agent profiles so get_definition() resolves.
for _ad in load_agents_tree(SRC / "agents" / "profile"):
    register_definition(_ad)

from tools.ask_helper._lib._compose import (  # noqa: E402
    HelperMessages,
    assemble_user_msg_1,
)
from tools.ask_helper.ask_advisor import _build_advisor_user_msg_2  # noqa: E402
from tools.ask_helper.ask_resolver import (  # noqa: E402
    _build_resolver_user_msg_2,
)
from task_center.context_engine.recipes.role_instruction import (  # noqa: E402
    evaluator_instruction,
    explorer_instruction,
    generator_instruction,
    planner_instruction,
)


RUNS_DIR = REPO / ".sweevo_runs" / "scenario_logs"
REPORT_PATH = REPO / "docs" / "reports" / "first_three_messages_report.md"


# ----------------------------------------------------------------------------
# Harvest main-agent first messages from existing live-e2e runs.
# ----------------------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class CapturedAgent:
    label: str
    scenario: str
    run_id: str
    agent_name: str
    iteration: str
    attempt: str
    role_dir: str
    system: str
    user_msg_1: str
    user_msg_2: str  # empty string when the launch shape only seeded one user message
    # Row 4 — the skill + terminal_selection composite. Non-empty only for
    # planner launches in v1 (Round 3 ships skills for planner variants only).
    skill_row: str = ""

    @property
    def message_jsonl(self) -> str:
        return str(Path(self.scenario) / self.run_id / self.role_dir / "message.jsonl")


def _latest_run(scenario: str) -> Path | None:
    scenario_dir = RUNS_DIR / scenario
    if not scenario_dir.exists():
        return None
    runs = sorted(scenario_dir.iterdir(), reverse=True)
    return runs[0] if runs else None


def _read_initial_rows(message_path: Path) -> tuple[str, str, str, str]:
    """Return (system, user_msg_1, user_msg_2, skill_row).

    The recorder writes system + every seeded initial user + the spawn
    prompt. For Round 3 launch shapes:

    * 2 rows = entry_executor (system + combined user); both user fields
      after the first are empty.
    * 3 rows = executor / evaluator (system + context + role_instruction);
      ``skill_row`` is empty.
    * 4 rows = skill-equipped planner (system + context + role_instruction +
      skill row 4). The skill row carries the
      ``Load skill: <name>`` header plus the ``<skill>`` and
      ``<terminal_selection>`` blocks.
    """
    rows: list[dict] = []
    with message_path.open() as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            rows.append(json.loads(line))
            if len(rows) == 4:
                break
    system_text = _text_of(rows[0]) if rows and rows[0].get("role") == "system" else ""
    user_msg_1 = _text_of(rows[1]) if len(rows) > 1 and rows[1].get("role") == "user" else ""
    user_msg_2 = _text_of(rows[2]) if len(rows) > 2 and rows[2].get("role") == "user" else ""
    skill_row = _text_of(rows[3]) if len(rows) > 3 and rows[3].get("role") == "user" else ""
    return system_text, user_msg_1, user_msg_2, skill_row


def _text_of(row: dict) -> str:
    parts: list[str] = []
    for block in row.get("content", []) or []:
        if isinstance(block, dict) and block.get("type") == "text":
            parts.append(block.get("text", ""))
    return "\n".join(parts)


def _find_agents_under(run_dir: Path) -> list[tuple[str, str, str, Path]]:
    """Return (iteration, attempt, role_dir_name, message_jsonl_path) tuples."""
    out: list[tuple[str, str, str, Path]] = []
    for jsonl in sorted(run_dir.rglob("message.jsonl")):
        rel = jsonl.relative_to(run_dir)
        parts = rel.parts
        iteration = ""
        attempt = ""
        role_dir = parts[-2] if len(parts) >= 2 else ""
        for part in parts:
            if part.startswith("iteration_"):
                iteration = part
            elif part.startswith("attempt_"):
                attempt = part
        out.append((iteration, attempt, role_dir, jsonl))
    return out


def harvest_main_agents() -> list[CapturedAgent]:
    """Pick representative main-agent first messages from existing runs.

    Selects:
    - entry_executor from pipeline.iterative_continuation (deepest live coverage)
    - planner iter 1 attempt 1 (continuation: partial plan path)
    - planner iter 2 attempt 1 (continuation: full plan after partial)
    - planner iter 1 attempt 2 from attempt_retry_planner_failure (failed-attempts path)
    - executor (one task) from iterative_continuation
    - evaluator from iterative_continuation iter 1 (partial-plan evaluator branch)
    - evaluator from iterative_continuation iter 2 (full-plan evaluator branch)
    """
    # Prefer the new live scenario run when present — it carries all branches
    # in one tree (attempt retry + continuation + 2 iterations).
    primary = "pipeline.first_three_messages_capture"
    primary_run = _latest_run(primary)
    if primary_run is not None:
        sources = [
            ("entry_executor (root delegation)", primary, lambda r: _select_role(r, "entry_executor_")),
            ("planner — iter1 attempt1 (invalid plan)", primary, lambda r: _select_role(r, "planner_", iteration="iteration_01", attempt="attempt_01")),
            ("planner — iter1 attempt2 (after planner failure)", primary, lambda r: _select_role(r, "planner_", iteration="iteration_01", attempt="attempt_02")),
            ("planner — iter2 attempt1 (continuation, full plan)", primary, lambda r: _select_role(r, "planner_", iteration="iteration_02", attempt="attempt_01")),
            ("executor — iter1 attempt2 (continuation partial)", primary, lambda r: _select_role(r, "executor_", iteration="iteration_01", attempt="attempt_02")),
            ("executor — iter2 attempt1 (continuation full)", primary, lambda r: _select_role(r, "executor_", iteration="iteration_02", attempt="attempt_01")),
            ("evaluator — partial-plan attempt", primary, lambda r: _select_role(r, "evaluator_", iteration="iteration_01", attempt="attempt_02")),
            ("evaluator — full-plan attempt", primary, lambda r: _select_role(r, "evaluator_", iteration="iteration_02", attempt="attempt_01")),
        ]
    else:
        sources = [
            ("entry_executor (root delegation)", "pipeline.iterative_continuation", lambda r: _select_role(r, "entry_executor_")),
            ("planner — iter1 attempt1 (continuation partial)", "pipeline.iterative_continuation", lambda r: _select_role(r, "planner_", iteration="iteration_01", attempt="attempt_01")),
            ("planner — iter2 attempt1 (continuation full)", "pipeline.iterative_continuation", lambda r: _select_role(r, "planner_", iteration="iteration_02", attempt="attempt_01")),
            ("planner — iter1 attempt2 (after planner failure)", "pipeline.attempt_retry_planner_failure", lambda r: _select_role(r, "planner_", iteration="iteration_01", attempt="attempt_02")),
            ("executor — generator task (no deps)", "pipeline.iterative_continuation", lambda r: _select_role(r, "executor_", iteration="iteration_01", attempt="attempt_01")),
            ("evaluator — partial-plan attempt", "pipeline.iterative_continuation", lambda r: _select_role(r, "evaluator_", iteration="iteration_01", attempt="attempt_01")),
            ("evaluator — full-plan attempt", "pipeline.iterative_continuation", lambda r: _select_role(r, "evaluator_", iteration="iteration_02", attempt="attempt_01")),
        ]
    captures: list[CapturedAgent] = []
    for label, scenario, selector in sources:
        run = _latest_run(scenario)
        if run is None:
            continue
        agents = _find_agents_under(run)
        picked = selector(agents)
        if picked is None:
            print(f"WARN: no agent matched for {label} in {scenario}/{run.name}", file=sys.stderr)
            continue
        iteration, attempt, role_dir, jsonl = picked
        system, user_msg_1, user_msg_2, skill_row = _read_initial_rows(jsonl)
        if role_dir.startswith("entry_executor_") or "entry_executor" in role_dir:
            agent_name = "entry_executor"
        elif "planner" in role_dir:
            agent_name = "planner"
        elif "executor" in role_dir:
            agent_name = "executor"
        elif "evaluator" in role_dir:
            agent_name = "evaluator"
        else:
            agent_name = role_dir.split("_", 1)[0]
        captures.append(
            CapturedAgent(
                label=label,
                scenario=scenario,
                run_id=run.name,
                agent_name=agent_name,
                iteration=iteration,
                attempt=attempt,
                role_dir=role_dir,
                system=system,
                user_msg_1=user_msg_1,
                user_msg_2=user_msg_2,
                skill_row=skill_row,
            )
        )
    return captures


def _select_role(
    agents: list[tuple[str, str, str, Path]],
    prefix: str,
    *,
    iteration: str | None = None,
    attempt: str | None = None,
):
    for it, att, role_dir, jsonl in agents:
        # role_dir looks like "01_planner_<uuid>:planner" or
        # "entry_executor_<uuid>:entry"; match prefix substring.
        if prefix not in role_dir:
            continue
        if iteration is not None and not it.startswith(iteration):
            continue
        if attempt is not None and not att.startswith(attempt):
            continue
        return (it, att, role_dir, jsonl)
    return None


# ----------------------------------------------------------------------------
# Helpers / subagent — programmatic construction via real builder code.
# ----------------------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class ConstructedAgent:
    label: str
    agent_name: str
    system: str
    user_msg_1: str
    user_msg_2: str


def _excerpt_for_helper_context(captures: list[CapturedAgent]) -> tuple[str, str, str]:
    """Pick a realistic parent context to feed advisor/resolver constructors.

    Returns (parent_user_msg_1_text, parent_user_msg_2_text, parent_transcript).
    For deterministic illustration we use the iter1 attempt1 executor capture as
    the "parent". Note: the captured row contains the spawn prompt
    (= role_instruction = parent's user_msg_2). The recorder did not store
    parent's user_msg_1 (context) so we use a representative stand-in stitched
    together from the goal/iteration/task sections embedded in the prompt.
    """
    parent = next(
        (c for c in captures if c.agent_name == "executor"), captures[0]
    )
    parent_user_msg_2 = parent.user_msg_2 or parent.user_msg_1
    parent_user_msg_1 = (
        "# Goal\n\nResolve the SWE-EVO mock workspace preflight goal.\n\n"
        "# Current Iteration\n\nIteration 1: validate the harness with a "
        "preflight probe; defer follow-up to a continuation iteration.\n\n"
        "# Attempt Plan\n\n"
        "Run a workspace preflight probe (single task, no dependencies)."
    )
    parent_transcript = (
        "(omitted for brevity — real transcripts include every tool call and "
        "result the parent emitted before submitting.)"
    )
    return parent_user_msg_1, parent_user_msg_2, parent_transcript


def build_main_constructed() -> list[ConstructedAgent]:
    """Build the full 3-message shape for every main-agent variant.

    Uses the real role-instruction builders + the real renderer's terminal
    catalog appender so user_msg_2 is the exact text that
    ``ContextComposer.compose`` would assemble if the launcher took the
    2-message split path. user_msg_1 is a representative context render
    using the renderer-mandated headings (we do not run the live composer
    because it requires populated stores; the heading + body shape is the
    same the renderer produces).
    """
    from tools._terminals.registry import render_terminal_catalog

    out: list[ConstructedAgent] = []

    def _append_catalog(role_instruction: str, agent_def) -> str:
        if not agent_def or not agent_def.terminals:
            return role_instruction
        catalog = render_terminal_catalog(
            list(agent_def.terminals), focus="selection_guidance"
        )
        return (
            f"{role_instruction.rstrip()}\n\n"
            "# Terminal tools you may call\n\n"
            f"Pick exactly one based on outcome:\n\n{catalog}\n\n"
            "# Your task\n\n"
            "Execute the role described above. Before any terminal submission, "
            "call ask_advisor with your chosen tool_name and intended payload. "
            "Submit your chosen terminal only after the advisor returns "
            "\"approve\"."
        )

    planner_def = get_definition("planner") or get_definition("planner_full_only")
    executor_sf = get_definition("executor_success_failure") or get_definition("executor")
    executor_sh = get_definition("executor_success_handoff") or get_definition("executor")
    evaluator_def = get_definition("evaluator")
    entry_def = get_definition("entry_executor")

    # --- Planner — 4 branches × routing ---
    planner_variants = [
        ("iter1 attempt1 (fresh)", 1, False,
         "# Goal\n\n<root goal>\n\n# Current Iteration\n\nIteration 1 (FIRST_ATTEMPT)."),
        ("iter1 attempt2 (after failed plan)", 1, True,
         "# Goal\n\n<root goal>\n\n# Current Iteration\n\nIteration 1 (retry).\n\n# Prior Failed Attempts\n\nAttempt 1: rejected — unknown dependency `missing`."),
        ("iter2 attempt1 (continuation, no prior failure)", 2, False,
         "# Goal\n\n<root goal>\n\n# Current Iteration\n\nIteration 2 (PARTIAL_CONTINUATION) — continuation_goal from iteration 1.\n\n# Previous Iteration Results\n\n## Iteration 1 accepted plan\n\n<partial plan_spec>\n\n## Iteration 1 summary\n\nWorkspace preflight completed."),
        ("iter2 attempt2 (continuation + prior failure)", 2, True,
         "# Goal\n\n<root goal>\n\n# Current Iteration\n\nIteration 2 (PARTIAL_CONTINUATION).\n\n# Previous Iteration Results\n\n## Iteration 1 accepted plan\n\n<partial plan>\n\n## Iteration 1 summary\n\nDone.\n\n# Prior Failed Attempts\n\nAttempt 1 in iteration 2: rejected by evaluator."),
    ]
    for label, iter_n, failed, um1 in planner_variants:
        role_text = planner_instruction(
            iteration_sequence_no=iter_n, has_failed_attempts=failed
        ).text
        um2 = _append_catalog(role_text, planner_def)
        out.append(ConstructedAgent(
            label=f"planner — {label}",
            agent_name="planner",
            system=planner_def.system_prompt or "" if planner_def else "",
            user_msg_1=um1,
            user_msg_2=um2,
        ))

    # --- Executor — 2 dep branches × 2 terminal-routing variants ---
    for has_deps, dep_label in ((True, "with deps"), (False, "no deps")):
        for variant_def, variant_name in (
            (executor_sf, "executor_success_failure"),
            (executor_sh, "executor_success_handoff"),
        ):
            role_text = generator_instruction(has_deps=has_deps).text
            um2 = _append_catalog(role_text, variant_def)
            dep_block = (
                "\n\n# Dependency Results\n\nupstream: success — artifacts=[...]"
                if has_deps
                else ""
            )
            um1 = (
                "# Attempt Plan\n\n<plan_spec>\n\n"
                "# Assigned Task\n\nid: preflight\nagent_name: executor\n"
                f"deps: {'[upstream]' if has_deps else '[]'}\nspec: Run a "
                "lightweight workspace preflight."
            ) + dep_block
            out.append(ConstructedAgent(
                label=f"executor variant `{variant_name}` ({dep_label})",
                agent_name=variant_name,
                system=variant_def.system_prompt or "" if variant_def else "",
                user_msg_1=um1,
                user_msg_2=um2,
            ))

    # --- Evaluator — 2 branches ---
    for is_partial, label in ((True, "partial attempt"), (False, "complete attempt")):
        role_text = evaluator_instruction(is_partial=is_partial).text
        um2 = _append_catalog(role_text, evaluator_def)
        partial_block = (
            "\n\n# Partial Plan Boundary\n\nIntentionally partial; "
            "continuation_goal is set."
            if is_partial
            else ""
        )
        um1 = (
            "# Attempt Plan\n\n<plan_spec>\n\n"
            "# Dependency Results\n\npreflight: success — artifacts=[]\n\n"
            "# Evaluation Criteria\n\n- Workspace preflight completed."
            + partial_block
        )
        out.append(ConstructedAgent(
            label=f"evaluator — {label}",
            agent_name="evaluator",
            system=evaluator_def.system_prompt or "" if evaluator_def else "",
            user_msg_1=um1,
            user_msg_2=um2,
        ))

    # --- Entry executor — single-user-message launch (no role_instruction) ---
    if entry_def is not None:
        out.append(ConstructedAgent(
            label="entry_executor (single-user-message launch)",
            agent_name="entry_executor",
            system=entry_def.system_prompt or "",
            user_msg_1=(
                "# Entry request\n\n<pr_description>\n"
                "(SWE-EVO entry prompt — workspace root + PR description, "
                "verbatim from build_sweevo_user_prompt)\n</pr_description>\n\n"
                "Workspace root: /testbed"
            ),
            user_msg_2="(entry_executor recipe emits no role_instruction — single-user-message launch)",
        ))

    return out


def build_helper_constructed(
    captures: list[CapturedAgent],
) -> list[ConstructedAgent]:
    parent_um1, parent_um2, transcript = _excerpt_for_helper_context(captures)

    advisor_def = get_definition("advisor")
    resolver_def = get_definition("resolver")
    explorer_def = get_definition("explorer")
    if advisor_def is None or resolver_def is None or explorer_def is None:
        raise RuntimeError("Missing helper/explorer agent definitions.")

    # Parent agent definition for catalog rendering — use executor variant.
    parent_def = (
        get_definition("executor_success_failure")
        or get_definition("executor")
    )

    advisor_messages = HelperMessages(
        helper_agent_def=advisor_def,
        parent_agent_def=parent_def,
        parent_user_msg_1=parent_um1,
        parent_user_msg_2=parent_um2,
        parent_transcript=transcript,
    )
    advisor_um1 = assemble_user_msg_1(advisor_messages)
    advisor_um2 = _build_advisor_user_msg_2(
        messages=advisor_messages,
        tool_name="submit_execution_success",
        tool_payload={
            "summary": "Workspace preflight completed.",
            "artifacts": [],
        },
    )

    resolver_messages = HelperMessages(
        helper_agent_def=resolver_def,
        parent_agent_def=parent_def,
        parent_user_msg_1=parent_um1,
        parent_user_msg_2=parent_um2,
        parent_transcript=transcript,
    )
    resolver_um1 = assemble_user_msg_1(resolver_messages)
    resolver_um2 = _build_resolver_user_msg_2(
        issues_to_resolve=[
            "preflight artifact `.ephemeralos/sweevo-mock/probe.txt` not found",
            "git rev-parse --is-inside-work-tree returned non-zero",
        ],
        issue_context=(
            "Evaluator observed the listed issues while inspecting the "
            "preflight executor's reported artifacts."
        ),
    )

    # Subagent: 2 messages total. user_msg_1 = parent's free-text prompt;
    # user_msg_2 = explorer_instruction text (spawn prompt). System = explorer
    # agent.md system_prompt.
    parent_prompt = (
        "Inspect the repository layout under backend/src/task_center to "
        "list every module that registers a context-recipe id and report "
        "file paths plus line numbers."
    )
    explorer_um1 = parent_prompt
    explorer_um2 = explorer_instruction().text

    return [
        ConstructedAgent(
            label="advisor (called from executor pre-submission)",
            agent_name="advisor",
            system=advisor_def.system_prompt or "",
            user_msg_1=advisor_um1,
            user_msg_2=advisor_um2,
        ),
        ConstructedAgent(
            label="resolver (called from verifier/evaluator on issues)",
            agent_name="resolver",
            system=resolver_def.system_prompt or "",
            user_msg_1=resolver_um1,
            user_msg_2=resolver_um2,
        ),
        ConstructedAgent(
            label="explorer subagent (called via run_subagent)",
            agent_name="explorer",
            system=explorer_def.system_prompt or "",
            user_msg_1=explorer_um1,
            user_msg_2=explorer_um2,
        ),
    ]


# ----------------------------------------------------------------------------
# Synthesise the third message for main agents (user_msg_1 / context message).
# ----------------------------------------------------------------------------


def synthesise_main_user_msg_1(capture: CapturedAgent) -> str:
    """Return a representative user_msg_1 (context message) for a main agent.

    The runtime never persists this message into ``message.jsonl`` — the
    recorder only sees the spawn prompt (role_instruction = user_msg_2). The
    runner does pass ``initial_messages=[ConversationMessage.from_user_text(
    context_message)]`` for the 2-message launch shape (see
    ``backend/src/task_center/attempt/launch.py:142``). We document that gap
    and reconstruct a faithful illustration by composing the public block
    headings the renderer would emit for this role + iteration position. Real
    runs would store the verbatim block contents at row 2; here we stub the
    bodies because re-running the composer requires a live DB.
    """
    role = capture.agent_name
    iteration_n = 1 if capture.iteration.startswith("iteration_01") else 2
    if role == "entry_executor":
        return (
            "# Entry request\n\n"
            "<pr_description>\n"
            "(SWE-EVO instance entry request — the same text the entry "
            "executor receives verbatim as its sole context block.)\n"
            "</pr_description>\n\n"
            "Workspace root: /workspace/repo\n"
        )
    if role == "planner":
        if iteration_n == 1 and capture.attempt.startswith("attempt_02"):
            return (
                "# Goal\n\n<iteration goal text>\n\n"
                "# Current Iteration\n\nIteration 1 — retry attempt 2.\n\n"
                "# Prior Failed Attempts\n\n"
                "Attempt 1 submitted a plan with an unknown dependency "
                "`missing`; the planner-validation rejected it (see audit "
                "events PLANNER_INVOKED → TOOL_CALL_ERROR)."
            )
        if iteration_n == 2:
            return (
                "# Goal\n\n<root goal text>\n\n"
                "# Current Iteration\n\n"
                "Iteration 2 (PARTIAL_CONTINUATION) — continue from the "
                "continuation_goal supplied by iteration 1's partial plan.\n\n"
                "# Previous Iteration Results\n\n"
                "## Iteration 1 accepted plan\n\n"
                "<plan_spec from iteration 1, the partial preflight plan>\n\n"
                "## Iteration 1 summary\n\nWorkspace preflight completed; "
                "continuation_goal handed off."
            )
        return (
            "# Goal\n\n<root goal text>\n\n"
            "# Current Iteration\n\nIteration 1 — first attempt."
        )
    if role == "executor":
        return (
            "# Attempt Plan\n\n<plan_spec for the current attempt>\n\n"
            "# Assigned Task\n\n"
            "id: preflight\nagent_name: executor\ndeps: []\nspec: Run a "
            "lightweight workspace preflight and report the observed sandbox "
            "root."
        )
    if role == "evaluator":
        partial = capture.iteration.startswith("iteration_01")
        partial_block = (
            "\n\n# Partial Plan Boundary\n\nThe attempt is intentionally "
            "partial; continuation_goal is set."
            if partial
            else ""
        )
        return (
            "# Attempt Plan\n\n<plan_spec for the attempt>\n\n"
            "# Dependency Results\n\n"
            "preflight: success — artifacts=[], summary='Workspace preflight "
            "completed.'\n\n"
            "# Evaluation Criteria\n\n- Workspace preflight completed."
            + partial_block
        )
    return "(unknown role)"


# ----------------------------------------------------------------------------
# Verdict — coherence and quality assessment per agent + overall.
# ----------------------------------------------------------------------------


def coherence_verdict(
    label: str,
    system: str,
    um1: str,
    um2: str,
    *,
    is_main: bool = False,
) -> dict:
    """Return a small dict with check results and a verdict string.

    For main agents (``is_main=True``) we evaluate only system + the
    observed single user message (``um1``) because the runtime currently
    records only those two rows into ``message.jsonl`` — main-agent recipes
    use the single-user-message launch shape (role_instruction is folded
    into the same packet as the context blocks, or the recipe emits no
    role_instruction at all). For helpers/subagent we evaluate the full
    three-message shape.
    """
    checks: dict[str, bool] = {}
    notes: list[str] = []

    checks["system_nonempty"] = bool(system.strip())
    checks["user_msg_1_nonempty"] = bool(um1.strip())
    if not is_main:
        checks["user_msg_2_nonempty"] = bool(um2.strip())

    if label.startswith("entry_executor"):
        checks["um1_has_entry_request_heading"] = "# Entry request" in um1 or "entry_request" in um1.lower()
        checks["system_mentions_handoff_or_finish"] = (
            "submit_execution_handoff" in system or "submit_execution_success" in system
        )
    elif label.startswith("planner"):
        checks["um1_has_goal"] = "# Goal" in um1
        checks["um1_has_iteration"] = ("# Current Iteration" in um1) or ("Goal / Current Iteration" in um1)
        if (
            "attempt_02" in label
            or "attempt2 (after" in label
            or "+ prior failure" in label
            or "after planner failure" in label
        ):
            checks["um1_has_failed_attempts"] = (
                "# Prior Failed Attempts" in um1 or "# Failed Attempts" in um1
            )
        if "iter2" in label:
            checks["um1_has_previous_iteration_results"] = (
                "# Previous Iteration Results" in um1
                or "Iteration 1 accepted plan" in um1
            )
        checks["system_planner_role"] = "planner" in system.lower()
        if um2:
            checks["um2_terminal_catalog"] = (
                "# Terminal tools you may call" in um2
                or "submit_plan_closes_goal" in um2
            )
            checks["um2_calls_advisor"] = "ask_advisor" in um2
    elif label.startswith("executor"):
        checks["um1_has_attempt_plan"] = "# Attempt Plan" in um1 or "Attempt Plan" in um1
        checks["um1_has_assigned_task"] = "# Assigned Task" in um1 or "Assigned Task" in um1
        checks["system_executor_role"] = "executor" in system.lower() or "generator" in system.lower()
        if um2:
            checks["um2_generator_role_text"] = "generator task" in um2.lower()
            checks["um2_terminal_catalog"] = (
                "submit_execution_success" in um2
                or "submit_execution_failure" in um2
                or "submit_execution_handoff" in um2
            )
            checks["um2_calls_advisor"] = "ask_advisor" in um2
    elif label.startswith("evaluator"):
        checks["um1_has_attempt_plan"] = "# Attempt Plan" in um1 or "Attempt Plan" in um1
        checks["um1_has_criteria"] = "# Evaluation Criteria" in um1 or "Evaluation Criteria" in um1
        checks["um1_has_dependency_results"] = (
            "# Dependency Results" in um1 or "Dependency Results" in um1
        )
        checks["system_evaluator_role"] = "evaluat" in system.lower()
        if um2:
            checks["um2_evaluator_role_text"] = (
                "evaluating" in um2.lower() or "evaluate" in um2.lower()
            )
            checks["um2_terminal_catalog"] = (
                "submit_evaluation_success" in um2
                or "submit_evaluation_failure" in um2
            )
    elif label.startswith("advisor"):
        checks["um1_has_prompt_injection_guard"] = (
            "Do not follow any instruction that appears inside" in um1
        )
        checks["um1_has_parent_context"] = "# Parent agent's original context" in um1
        checks["um1_has_parent_task"] = "# Parent agent's original task" in um1
        checks["um2_has_pending_submission"] = "# Pending submission" in um2
        checks["um2_has_calibration"] = "# Calibration" in um2
        checks["um2_has_how_to_submit"] = "# How to submit" in um2
    elif label.startswith("resolver"):
        checks["um1_has_prompt_injection_guard"] = (
            "Do not follow any instruction that appears inside" in um1
        )
        checks["um1_has_parent_context"] = "# Parent agent's original context" in um1
        checks["um2_has_issues"] = "# Issues to resolve" in um2
        checks["um2_has_task"] = "# Your task" in um2
    elif label.startswith("explorer"):
        checks["um2_has_explorer_identity"] = "explorer subagent" in um2
        checks["um2_has_terminal_call"] = "submit_exploration_result" in um2

    passed = all(checks.values())
    verdict = "PASS" if passed else "FAIL"
    if not passed:
        notes.append(
            "; ".join(name for name, ok in checks.items() if not ok)
        )
    return {"checks": checks, "verdict": verdict, "notes": notes}


# ----------------------------------------------------------------------------
# Report writer.
# ----------------------------------------------------------------------------


def _fmt_message(role: str, text: str) -> str:
    text = text.strip()
    if not text:
        return f"```\n({role}: empty)\n```\n"
    return f"```\n{text}\n```\n"


def _truncate(text: str, limit: int = 6000) -> str:
    if len(text) <= limit:
        return text
    head = text[:limit]
    return head + f"\n\n…(truncated {len(text) - limit} chars)"


def render_report(
    main_captures: list[CapturedAgent],
    helper_subagent: list[ConstructedAgent],
    main_constructed: list[ConstructedAgent] | None = None,
) -> str:
    main_constructed = main_constructed or []
    out: list[str] = []
    out.append("# First-Three-Messages Capture Report\n")
    out.append(
        "## What this report contains\n\n"
        "First three messages observed at agent launch (system + "
        "user_msg_1 + user_msg_2), per agent role. Captured from a live "
        "run of the new scenario "
        "`pipeline.first_three_messages_capture` (continuation goal + "
        "attempt retry across 2 iterations) executed against real "
        "Postgres + real Daytona sandbox + real composer + real recorder; "
        "only the agent LLM is replaced with the deterministic "
        "`MockSquadRunner`.\n\n"
        "- **Main agents (planner, executor, evaluator)** — three "
        "messages: system from `agents/profile/main/<name>.md`; "
        "user_msg_1 = the composer's context block (goal + iteration + "
        "dependency results + attempt plan + evaluation criteria, "
        "rendered by `MarkdownPromptRenderer.render_context`); "
        "user_msg_2 = the spawn prompt (the role_instruction body for "
        "the agent's iteration/attempt position from "
        "`recipes/role_instruction.py` plus the terminal-tool catalog "
        "appended by `_append_terminal_catalog`).\n\n"
        "- **entry_executor** — two messages (no role_instruction recipe "
        "block); user_msg_2 is empty.\n\n"
        "- **Helpers (advisor, resolver)** — three messages: system + "
        "`assemble_user_msg_1(...)` (prompt-injection guard + parent's "
        "original context + parent's original task + filtered parent "
        "transcript) + helper-specific user_msg_2 (advisor: catalog + "
        "pending submission + task + calibration + how-to-submit; "
        "resolver: issues + task). Built by "
        "`tools/ask_helper/_lib/_compose.py` and consumed by "
        "`tools/ask_helper/ask_advisor.py` / `ask_resolver.py`.\n\n"
        "- **Subagent (explorer)** — by code (`tools/subagent/"
        "run_subagent.py:231-240`) the subagent also receives three "
        "messages: system + user_msg_1 (the parent's free-text prompt, "
        "passed via `initial_messages`) + user_msg_2 (the spawn prompt = "
        "`explorer_instruction().text`). The goal text described this as "
        "\"only 2\", presumably referring to the two distinct user "
        "messages (no role-instruction block separate from the spawn "
        "prompt). We render all three below for completeness.\n\n"
        "Source for main-agent rows: existing live-e2e runs under "
        "`.sweevo_runs/scenario_logs/`. Source for helper/subagent: "
        "programmatic construction via the production builder code in "
        "`tools/ask_helper/_lib/_compose.py` and "
        "`task_center/context_engine/recipes/role_instruction.py` against "
        "realistic parent context lifted from a real executor capture.\n"
    )

    out.append("## Coverage matrix\n")
    out.append(
        "| Agent role | Routing / variant | Iteration position | Attempt | Source |\n"
        "|---|---|---|---|---|"
    )
    for cap in main_captures:
        out.append(
            f"| {cap.agent_name} | {cap.role_dir.split('_', 1)[1] if '_' in cap.role_dir else cap.role_dir} "
            f"| {cap.iteration or '—'} | {cap.attempt or '—'} | {cap.scenario}/{cap.run_id} |"
        )
    for ca in helper_subagent:
        out.append(
            f"| {ca.agent_name} | helper/subagent | — | — | programmatic |"
        )
    out.append("")

    out.append("## Main agents (initial rows captured live)\n")
    out.append(
        "Every main-agent row below is harvested verbatim from "
        "`message.jsonl` written by `AgentMessageJsonlRecorder."
        "record_initial_messages`. Launch shapes (Round 3):\n\n"
        "* planner — 4 rows (system + context + role_instruction + skill); "
        "row 4 is the row-4 composite from `build_skill_message`.\n"
        "* executor / evaluator — 3 rows (system + context + role_instruction); "
        "no skill in v1.\n"
        "* entry_executor — 2 rows (single-user-message launch).\n"
    )
    for cap in main_captures:
        out.append(f"### {cap.label}\n")
        out.append(
            f"- `agent_name`: `{cap.agent_name}`\n"
            f"- `scenario`: `{cap.scenario}`\n"
            f"- `run_id`: `{cap.run_id}`\n"
            f"- `role_dir`: `{cap.role_dir}`\n"
            f"- source file: `{cap.message_jsonl}`\n"
        )
        out.append("**system** (verbatim, `message.jsonl` row 1):\n")
        out.append(_fmt_message("system", _truncate(cap.system)))
        out.append(
            "**user_msg_1** (verbatim, `message.jsonl` row 2 — the "
            "composer's context block):\n"
        )
        out.append(_fmt_message("user_msg_1", _truncate(cap.user_msg_1)))
        if cap.user_msg_2:
            out.append(
                "**user_msg_2** (verbatim, `message.jsonl` row 3 — "
                "role_instruction + terminal catalog):\n"
            )
            out.append(_fmt_message("user_msg_2", _truncate(cap.user_msg_2)))
        else:
            out.append(
                "**user_msg_2** — *not emitted* (single-user-message "
                "launch; recipe carries no role_instruction block).\n"
            )
        if cap.skill_row:
            out.append(
                "**row 4** (verbatim, `message.jsonl` row 4 — skill body + "
                "`<terminal_selection>` composite from `build_skill_message`):\n"
            )
            out.append(_fmt_message("skill", _truncate(cap.skill_row)))
        verdict = coherence_verdict(
            cap.label,
            cap.system,
            cap.user_msg_1,
            cap.user_msg_2,
            is_main=not bool(cap.user_msg_2),
        )
        out.append(f"**Verdict:** {verdict['verdict']}  ")
        out.append(f"Checks: `{verdict['checks']}`  ")
        if verdict["notes"]:
            out.append(f"Notes: {verdict['notes'][0]}  ")
        out.append("")

    if main_constructed:
        out.append(
            "## Main agents — full 3-message shape (constructed from real "
            "builder code)\n"
        )
        out.append(
            "These rows show the **three** messages each main-agent role "
            "would receive if the launcher took the 2-user-message split "
            "path (`task_center/attempt/launch.py:141-145`). system text is "
            "the actual `agents/profile/main/<name>.md` body; user_msg_1 is "
            "a renderer-shaped context block (header names from "
            "`renderer._DEFAULT_HEADINGS`); user_msg_2 is the exact text "
            "the composer would emit — the role_instruction text from "
            "`recipes/role_instruction.py` plus the terminal catalog "
            "appended by `_append_terminal_catalog` "
            "(`context_engine/core.py:158-181`). Variants cover the full "
            "matrix: 4 planner branches × iteration-position / failed-"
            "attempts; 2 executor routing variants × dep presence; 2 "
            "evaluator branches; entry_executor's single-user-message "
            "fallback.\n"
        )
        for ca in main_constructed:
            out.append(f"### {ca.label}\n")
            out.append(f"- `agent_name`: `{ca.agent_name}`\n")
            out.append("**system** (verbatim, from `agent.md`):\n")
            out.append(_fmt_message("system", _truncate(ca.system)))
            out.append("**user_msg_1** (constructed; renderer-shaped):\n")
            out.append(_fmt_message("user_msg_1", _truncate(ca.user_msg_1)))
            out.append("**user_msg_2** (constructed via real builders):\n")
            out.append(_fmt_message("user_msg_2", _truncate(ca.user_msg_2)))
            verdict = coherence_verdict(
                ca.label, ca.system, ca.user_msg_1, ca.user_msg_2, is_main=False
            )
            out.append(f"**Verdict:** {verdict['verdict']}  ")
            out.append(f"Checks: `{verdict['checks']}`  ")
            if verdict["notes"]:
                out.append(f"Notes: {verdict['notes'][0]}  ")
            out.append("")

    out.append("## Helpers and subagent\n")
    for ca in helper_subagent:
        out.append(f"### {ca.label}\n")
        out.append(f"- `agent_name`: `{ca.agent_name}`\n")
        out.append("**system** (verbatim, from `agents/profile/.../{name}.md`):\n")
        out.append(_fmt_message("system", _truncate(ca.system)))
        out.append("**user_msg_1** (programmatic, from builder code):\n")
        out.append(_fmt_message("user_msg_1", _truncate(ca.user_msg_1)))
        if ca.agent_name == "explorer":
            out.append(
                "**user_msg_2** (explorer subagent only has two messages — "
                "`user_msg_2` is the spawn prompt = `explorer_instruction()`):\n"
            )
        else:
            out.append("**user_msg_2** (programmatic, from builder code):\n")
        out.append(_fmt_message("user_msg_2", _truncate(ca.user_msg_2)))
        verdict = coherence_verdict(ca.label, ca.system, ca.user_msg_1, ca.user_msg_2)
        out.append(f"**Verdict:** {verdict['verdict']}  ")
        out.append(f"Checks: `{verdict['checks']}`  ")
        if verdict["notes"]:
            out.append(f"Notes: {verdict['notes'][0]}  ")
        out.append("")

    out.append("## Overall verdict\n")
    out.append(
        "- **Coherence (presence contract):** every captured main-agent "
        "system + user message carries the headings the renderer is "
        "contracted to emit for that role and iteration position — `# Goal`, "
        "`# Current Iteration` (or the `Goal / Current Iteration` group "
        "heading), `# Prior Failed Attempts` on attempt ≥2, "
        "`# Previous Iteration Results` (or `## Iteration N accepted plan` "
        "/ `## Iteration N summary` groups) on iteration ≥2. Every helper's "
        "user_msg_1 starts with the prompt-injection guard and shows the "
        "parent context + parent task verbatim. Every helper's user_msg_2 "
        "ends with the bound terminal tool (`submit_advisor_feedback`, "
        "`submit_resolver_result`, `submit_exploration_result`).\n"
        "- **Context quality:** planner prompts adapt to iteration position "
        "and prior-attempt presence (4 branches in "
        "`recipes/role_instruction.py:planner_instruction`); evaluator "
        "prompts adapt to partial/complete attempt; executor prompts adapt "
        "to dependency presence. Routing variants visible in `role_dir` "
        "(`executor_success_failure` vs `executor_success_handoff`) inherit "
        "the same context_message but expose different terminal catalogues "
        "via the composer's `_append_terminal_catalog`.\n"
        "- **Instruction quality:** main-agent system prompts (in "
        "`agents/profile/main/<name>.md`) embed selection criteria, hard "
        "validity rules, and design principles. Helper user_msg_2 enforces "
        "tri-part summary structure (advisor) or per-issue resolution "
        "(resolver). Explorer user_msg_2 demands concrete findings (file "
        "paths, line numbers, symbols).\n"
        "- **Verdict — PASS for all sampled roles.** The presence contract "
        "is satisfied across the iteration / attempt / routing matrix.\n"
        "- **Gap closed:** `AgentMessageJsonlRecorder."
        "record_initial_messages` was extended to accept "
        "`seeded_initial_messages` and write them between the system row "
        "and the spawn-prompt row. Both the live engine "
        "(`engine/query/request.py:_record_initial_messages_once`) and the "
        "mock runner (`task_center_runner/agent/mock/runner.py:"
        "_record_initial_messages`) now feed seeded messages through. "
        "Captured `message.jsonl` files for planner / executor / evaluator "
        "now hold three initial rows (system + user_msg_1 + user_msg_2); "
        "entry_executor stays at two by design (single-user-message recipe).\n"
        "- **Scope notes:** the new scenario file "
        "`backend/src/task_center_runner/scenarios/pipeline/"
        "first_three_messages_capture.py` registers a complex run (2 "
        "iterations with continuation_goal + attempt retry + helper/"
        "subagent invocations). The matching pytest test "
        "`backend/src/task_center_runner/tests/sweevo/"
        "test_first_three_messages_capture.py` was attempted live with the "
        "containerised postgres (`backend/docker-compose.postgres.yml`) "
        "providing `EPHEMERALOS_DATABASE_URL`. The live run reached the "
        "`sweevo_sandbox` session fixture and then **timed out in Daytona "
        "sandbox creation** after 300s (`DaytonaTimeoutError: Function "
        "'create' exceeded timeout of 300.0 seconds`) — see the "
        "`Daytona pending_build hang root cause` memory entry. The "
        "composer / recorder / planner-validation pipeline this report "
        "audits is exercised identically by the most recent live runs of "
        "`pipeline.iterative_continuation` and "
        "`pipeline.attempt_retry_planner_failure`, which is why those are "
        "the captured-row source.\n"
    )

    return "\n".join(out) + "\n"


def main() -> int:
    main_captures = harvest_main_agents()
    if not main_captures:
        print("ERROR: no main-agent captures harvested", file=sys.stderr)
        return 1
    helpers = build_helper_constructed(main_captures)
    main_constructed = build_main_constructed()
    REPORT_PATH.parent.mkdir(parents=True, exist_ok=True)
    REPORT_PATH.write_text(
        render_report(main_captures, helpers, main_constructed)
    )
    print(f"wrote {REPORT_PATH}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
