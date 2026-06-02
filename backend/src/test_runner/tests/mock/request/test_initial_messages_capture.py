"""Live regression for the initial-messages capture scenario.

Runs ``pipeline.initial_messages_capture`` with the standard SWE-EVO
sandbox + stores fixtures, then asserts the captured ``message.jsonl``
trees carry the right shape for every iteration position and attempt:

* planner launches — 4 rows (system + ``<context>`` envelope +
  ``<Task Guidance>`` envelope + skill row). The row-4 skill row's
  ``<terminal_tool_selection>`` block must match the row-3 block
  byte-for-byte — both render from
  ``render_terminal_catalog(focus="selection_guidance", ...)`` (AC #15).
* executor / reducer launches — 3 rows (system + ``<context>`` +
  ``<Task Guidance>``); no skill is declared in v1.

For helper (advisor) and subagent (explorer) initial-message
construction, see ``scripts/build_initial_messages_report.py`` — the
scenario does not invoke helpers today, so the report builder calls the
real builder functions in ``tools/ask_helper/_lib/_compose.py`` and
``tools/subagent/explorer_guidance.py`` against a realistic
parent context. Once the scenario script grows a helper dispatch, this test
should be extended to also collect ``advisor`` / ``explorer`` ``message.jsonl``
trees from the live run.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from agents import load_agents_dir
from test_runner.benchmarks.sweevo.models import SWEEvoInstance
from test_runner.environments.sweevo_image.fixtures import run_scenario_on_sweevo_image
from test_runner.core.stores import TaskStoreBundle
from test_runner.scenarios import SCENARIO_REGISTRY
from test_runner.tests._live_config import (
    database_configured,
    rust_sandbox_runtime_unavailable_reason,
)
from test_runner.tests.mock._focused_scenario_contracts import (
    count_deferred_attempts,
    count_role_tasks,
)
from tools._terminals.registry import render_terminal_catalog


pytestmark = pytest.mark.asyncio
_RUST_RUNTIME_UNAVAILABLE = rust_sandbox_runtime_unavailable_reason()


_SCENARIO_NAME = "pipeline.initial_messages_capture"


def _main_agent_profile_dir() -> Path:
    for parent in Path(__file__).resolve().parents:
        candidate = parent / "agents" / "profile" / "main"
        if candidate.exists():
            return candidate
    raise AssertionError("could not locate backend/src/agents/profile/main")


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    _RUST_RUNTIME_UNAVAILABLE is not None,
    reason=_RUST_RUNTIME_UNAVAILABLE or "Rust sandbox runtime unavailable",
)
async def test_initial_messages_capture(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskStoreBundle,
) -> None:
    scenario = SCENARIO_REGISTRY[_SCENARIO_NAME]()
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
    )

    # 1) Workflow closes succeeded, 2 iterations, 3 attempts total
    #    (iter1 attempts 1 and 2; iter2 attempt 1).
    assert report.request_status == "done", report.metrics
    workflow = report.graph_summary["workflows"][0]
    assert workflow["status"] == "succeeded"
    assert len(workflow["iterations"]) == 2, workflow
    attempts = [
        attempt
        for iteration in workflow["iterations"]
        for attempt in iteration["attempts"]
    ]
    assert len(attempts) == 3, attempts

    assert count_role_tasks(report, "planner") >= 3, report.graph_summary
    assert count_deferred_attempts(report) == 1, report.graph_summary
    # All 3 attempts' planners submit a valid plan (DONE); the only failure is
    # the intentional attempt-1 reducer failure (asserted below).
    assert count_role_tasks(report, "planner", status="done") == 3, report.graph_summary
    assert count_role_tasks(report, "reducer", status="failed") == 1, (
        report.graph_summary
    )
    assert count_role_tasks(report, "reducer", status="done") == 2, (
        report.graph_summary
    )

    # 2) message.jsonl present for every main-agent role we care about.
    messages = list(report.run_dir.rglob("message.jsonl"))
    assert messages, f"no message.jsonl under {report.run_dir}"

    captured: dict[str, dict[str, object]] = {}
    for path in messages:
        rows = [
            json.loads(line)
            for line in path.read_text().splitlines()
            if line.strip()
        ]
        if len(rows) < 2:
            continue
        role_dir = path.parent.name
        captured[str(path.relative_to(report.run_dir))] = {
            "role_dir": role_dir,
            "rows": rows,
        }
    assert captured, "no agent captures harvested"

    # 3) For every main-agent capture, the presence + shape contract holds.
    # The harness fixture (``registered_mock_agents()``) unregisters all
    # agent definitions on teardown, so re-load the production profiles
    # directly to ground the row-4 catalog character-for-character check.
    profile_dir = _main_agent_profile_dir()
    profiles = {d.name: d for d in load_agents_dir(profile_dir)}
    planner_def = profiles["planner"]
    assert planner_def.skill is not None
    planner_terminals = list(planner_def.terminals)

    for rel, cap in captured.items():
        role_dir = str(cap["role_dir"])
        rows = list(cap["rows"])
        texts = [_text_of(row) for row in rows]
        system = texts[0] if rows and rows[0].get("role") == "system" else ""
        user_msg_1 = (
            texts[1] if len(texts) > 1 and rows[1].get("role") == "user" else ""
        )
        assert system.strip(), f"{rel}: empty system prompt"
        assert user_msg_1.strip(), f"{rel}: empty user_msg_1"

        # The total row count includes assistant tool calls and results
        # appended during execution; assertions below pin only the first
        # N rows (the launch-time initial messages recorded by
        # ``AgentMessageJsonlRecorder.record_initial_messages``).
        # Every main agent's context row 2 must wrap in a context envelope.
        assert user_msg_1.startswith("<context"), (
            f"{rel}: row 2 does not start with '<context'"
        )
        assert user_msg_1.rstrip().endswith("</context>"), (
            f"{rel}: row 2 does not end with '</context>'"
        )

        if "planner" in role_dir:
            # Planner — 4 initial rows: system + <context> + <Task Guidance>
            # + skill.
            assert len(rows) >= 4, (
                f"{rel}: planner needs >=4 initial rows for the skill "
                f"composite, got {len(rows)}"
            )
            assert all(
                rows[i].get("role") == "user" for i in (1, 2, 3)
            ), f"{rel}: rows 2-4 must all be user messages"
            task_guidance = texts[2]
            skill_row = texts[3]
            assert "<goal" in user_msg_1, f"{rel}: missing <goal*> XML tag"
            assert "<current_iteration" in user_msg_1, (
                f"{rel}: missing <current_iteration> XML tag"
            )
            assert task_guidance.startswith("<Task Guidance>\n"), (
                f"{rel}: row 3 does not start with '<Task Guidance>\\n'"
            )
            assert task_guidance.rstrip().endswith("</Task Guidance>"), (
                f"{rel}: row 3 does not end with '</Task Guidance>'"
            )
            assert "<terminal_tool_selection>" in task_guidance, (
                f"{rel}: row 3 missing <terminal_tool_selection> block"
            )
            assert skill_row.startswith("Load skill: planner"), (
                f"{rel}: row 4 does not start with `Load skill: planner...`"
            )
            assert "<skill>" in skill_row and "</skill>" in skill_row, (
                f"{rel}: row 4 missing <skill> block"
            )
            assert (
                "<terminal_tool_selection>" in skill_row
                and "</terminal_tool_selection>" in skill_row
            ), f"{rel}: row 4 missing <terminal_tool_selection> block"

            # AC #15 — row 4 <terminal_tool_selection> content matches the
            # row 3 block byte-for-byte (between the open/close tags).
            terminals = _active_terminals(rows, default=planner_terminals)
            expected_catalog = render_terminal_catalog(
                terminals, focus="selection_guidance"
            )
            row3_block = task_guidance.split(
                "<terminal_tool_selection>\n", 1
            )[1].split("\n</terminal_tool_selection>", 1)[0]
            row4_block = skill_row.split(
                "<terminal_tool_selection>\n", 1
            )[1].split("\n</terminal_tool_selection>", 1)[0]
            assert row3_block == row4_block, (
                f"{rel}: row-3 and row-4 <terminal_tool_selection> bodies "
                "differ"
            )
            assert expected_catalog in row3_block, (
                f"{rel}: row 3 catalog does not match registry render"
            )
        elif "executor" in role_dir:
            # Executor: 4 initial rows (system + <context> + <Task Guidance>
            # + skill). Skills carry operational heuristics (treat
            # `<dependencies>` as fixed inputs, verify deliverable at claimed
            # location).
            assert len(rows) >= 4, (
                f"{rel}: executor needs >=4 initial rows for the skill "
                f"composite, got {len(rows)}"
            )
            assert "<assigned_task" in user_msg_1, (
                f"{rel}: missing <assigned_task> XML tag"
            )
            task_guidance = texts[2]
            assert task_guidance.startswith("<Task Guidance>\n"), (
                f"{rel}: row 3 does not start with '<Task Guidance>\\n'"
            )
            assert "<terminal_tool_selection>" in task_guidance, (
                f"{rel}: row 3 missing <terminal_tool_selection> block"
            )
            skill_row = texts[3]
            assert skill_row.startswith("Load skill: executor"), (
                f"{rel}: row 4 does not start with `Load skill: executor`"
            )
            assert "<skill>" in skill_row and "</skill>" in skill_row, (
                f"{rel}: row 4 missing <skill> block"
            )
        elif "reducer" in role_dir:
            # Reducer: 4 initial rows (system + <context> + <Task Guidance>
            # + skill). Skill carries pass/fail discipline heuristics.
            assert len(rows) >= 4, (
                f"{rel}: reducer needs >=4 initial rows for the skill "
                f"composite, got {len(rows)}"
            )
            assert "<assigned_task" in user_msg_1, (
                f"{rel}: missing <assigned_task> XML tag"
            )
            assert "<dependencies>" in user_msg_1, (
                f"{rel}: missing <dependencies> XML tag"
            )
            task_guidance = texts[2]
            assert task_guidance.startswith("<Task Guidance>\n"), (
                f"{rel}: row 3 does not start with '<Task Guidance>\\n'"
            )
            skill_row = texts[3]
            assert skill_row.startswith("Load skill: reducer"), (
                f"{rel}: row 4 does not start with `Load skill: reducer`"
            )
            assert "<skill>" in skill_row and "</skill>" in skill_row, (
                f"{rel}: row 4 missing <skill> block"
            )

    # 4) Emit the markdown report next to the run.
    report_path = report.run_dir / "initial_messages_report.md"
    _write_report(report.run_dir, captured, report_path)
    assert report_path.exists()


def _text_of(row: dict) -> str:
    parts: list[str] = []
    for block in row.get("content", []) or []:
        if isinstance(block, dict) and block.get("type") == "text":
            parts.append(block.get("text", ""))
    return "\n".join(parts)


def _active_terminals(rows: list[dict[str, object]], *, default: list[str]) -> list[str]:
    for row in rows:
        metadata = row.get("metadata")
        if not isinstance(metadata, dict):
            continue
        active = metadata.get("active_terminals")
        if isinstance(active, list):
            return [str(name) for name in active]
    return default


def _write_report(
    run_dir: Path, captured: dict[str, dict[str, object]], dest: Path
) -> None:
    lines: list[str] = []
    lines.append("# Initial Messages Capture — Live Run\n")
    lines.append(f"Source run directory: `{run_dir}`\n")
    lines.append(
        "Up to four rows per agent: system + <context> envelope + "
        "<Task Guidance> envelope (with embedded <terminal_tool_selection>) "
        "+ skill row (row 4 — planner only in v1). Helper (advisor) and "
        "subagent (explorer) are constructed by "
        "`scripts/build_initial_messages_report.py` — see "
        "`docs/reports/initial_messages_report.md`.\n"
    )
    for rel, cap in sorted(captured.items()):
        rows = list(cap["rows"])
        texts = [_text_of(row) for row in rows]
        system = texts[0] if rows and rows[0].get("role") == "system" else ""
        user_msg_1 = (
            texts[1] if len(texts) > 1 and rows[1].get("role") == "user" else ""
        )
        user_msg_2 = (
            texts[2] if len(texts) > 2 and rows[2].get("role") == "user" else ""
        )
        skill_row = (
            texts[3] if len(texts) > 3 and rows[3].get("role") == "user" else ""
        )

        lines.append(f"## `{rel}`\n")
        lines.append("**system**\n")
        lines.append(f"```\n{system.strip()[:6000]}\n```\n")
        lines.append("**user_msg_1** (<context> envelope)\n")
        lines.append(f"```\n{user_msg_1.strip()[:6000]}\n```\n")
        if user_msg_2:
            lines.append(
                "**user_msg_2** (<Task Guidance> envelope, with "
                "<terminal_tool_selection>)\n"
            )
            lines.append(f"```\n{user_msg_2.strip()[:6000]}\n```\n")
        if skill_row:
            lines.append(
                "**user_msg_3 — row 4** (skill + <terminal_tool_selection>)\n"
            )
            lines.append(f"```\n{skill_row.strip()[:6000]}\n```\n")
    dest.write_text("\n".join(lines) + "\n")
