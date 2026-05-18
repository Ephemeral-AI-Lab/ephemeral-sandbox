"""Live regression for the first-three-messages capture scenario.

Runs ``pipeline.first_three_messages_capture`` with the standard SWE-EVO
sandbox + stores fixtures, then asserts the captured ``message.jsonl``
trees carry the right shape for every iteration position and attempt:

* planner launches — 4 rows (system + context + role_instruction + skill);
  row 4 is the row-4 composite from
  ``task_center/context_engine/core.py:build_skill_message``. The
  ``<terminal_selection>`` block in row 4 must match the row-3 catalog
  content character-for-character (both render from
  ``render_terminal_catalog(focus="selection_guidance", ...)``).
* executor / evaluator launches — 3 rows (system + context +
  role_instruction); no skill is declared in v1.
* entry_executor — 2 rows (single-user-message launch).

For helper (advisor / resolver) and subagent (explorer) first-message
construction, see ``scripts/build_first_three_messages_report.py`` — the
mock-runner does not invoke helpers today, so the report builder calls the
real builder functions in ``tools/ask_helper/_lib/_compose.py`` and
``task_center/context_engine/recipes/role_instruction.py`` against a
realistic parent context. Once ``MockSquadRunner`` grows a helper dispatch,
this test should be extended to also collect ``advisor`` / ``resolver`` /
``explorer`` ``message.jsonl`` trees from the live run.
"""

from __future__ import annotations

import json
import os
from collections import Counter
from pathlib import Path

import pytest

from agents import load_agents_dir
from benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.audit.events import EventType
from task_center_runner.benchmarks.sweevo.fixtures import run_sweevo_scenario
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.scenarios import SCENARIO_REGISTRY
from tools._terminals.registry import render_terminal_catalog


pytestmark = pytest.mark.asyncio


_SCENARIO_NAME = "pipeline.first_three_messages_capture"


@pytest.mark.skipif(
    not os.environ.get("EPHEMERALOS_DATABASE_URL"),
    reason="EPHEMERALOS_DATABASE_URL not set - task_center_runner requires PostgreSQL",
)
async def test_first_three_messages_capture(
    sweevo_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
) -> None:
    scenario = SCENARIO_REGISTRY[_SCENARIO_NAME]()
    report = await run_sweevo_scenario(
        scenario,
        instance=sweevo_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
    )

    # 1) Goal closes succeeded, 2 iterations, 3 attempts total
    #    (iter1 attempts 1 and 2; iter2 attempt 1).
    assert report.task_center_status == "done", report.metrics
    goal = report.graph_summary["goals"][0]
    assert goal["status"] == "succeeded"
    assert len(goal["iterations"]) == 2, goal
    attempts = [
        attempt
        for iteration in goal["iterations"]
        for attempt in iteration["attempts"]
    ]
    assert len(attempts) == 3, attempts

    counts = Counter(event.type for event in report.events)
    assert counts[EventType.PLANNER_INVOKED] >= 3, counts
    assert counts[EventType.PLANNER_PARTIAL_PLAN] == 1, counts
    assert counts[EventType.PLANNER_FULL_PLAN] == 1, counts
    assert counts[EventType.TOOL_CALL_ERROR] >= 1, counts
    assert counts[EventType.EVALUATOR_SUCCESS] == 2, counts

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
    profile_dir = (
        Path(__file__).resolve().parents[3] / "agents" / "profile" / "main"
    )
    profiles = {d.name: d for d in load_agents_dir(profile_dir)}
    planner_def = profiles["planner"]
    planner_full_def = profiles["planner_full_only"]
    assert planner_def.skill is not None
    assert planner_full_def.skill is not None
    planner_terminals = list(planner_def.terminals)
    full_only_terminals = list(planner_full_def.terminals)

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
        if "planner" in role_dir:
            # Planner — 4 initial rows: system + context + role_instruction
            # + skill.
            assert len(rows) >= 4, (
                f"{rel}: planner needs >=4 initial rows for the skill "
                f"composite, got {len(rows)}"
            )
            assert all(
                rows[i].get("role") == "user" for i in (1, 2, 3)
            ), f"{rel}: rows 2-4 must all be user messages"
            role_instruction = texts[2]
            skill_row = texts[3]
            assert "# Goal" in user_msg_1, f"{rel}: missing goal block"
            assert (
                "# Current Iteration" in user_msg_1
                or "Goal / Current Iteration" in user_msg_1
            ), f"{rel}: missing iteration block"
            assert "# Terminal tools you may call" in role_instruction, (
                f"{rel}: row 3 missing terminal catalog heading"
            )
            assert skill_row.startswith("Load skill: planner"), (
                f"{rel}: row 4 does not start with `Load skill: planner...`"
            )
            assert "<skill>" in skill_row and "</skill>" in skill_row, (
                f"{rel}: row 4 missing <skill> block"
            )
            assert (
                "<terminal_selection>" in skill_row
                and "</terminal_selection>" in skill_row
            ), f"{rel}: row 4 missing <terminal_selection> block"

            # AC #6 — row 4 <terminal_selection> content matches the
            # row 3 catalog content character-for-character.
            terminals = (
                full_only_terminals
                if "planner_full_only" in role_dir
                else planner_terminals
            )
            expected_catalog = render_terminal_catalog(
                terminals, focus="selection_guidance"
            )
            assert expected_catalog in role_instruction, (
                f"{rel}: row 3 catalog does not match registry render"
            )
            row4_block = skill_row.split("<terminal_selection>", 1)[1].split(
                "</terminal_selection>", 1
            )[0]
            assert expected_catalog in row4_block, (
                f"{rel}: row 4 <terminal_selection> does not contain "
                f"the row 3 catalog text verbatim"
            )
        elif role_dir.startswith("entry_executor"):
            # Entry executor: 2 initial rows (system + single user message).
            assert rows[0].get("role") == "system"
            assert rows[1].get("role") == "user"
        elif "executor" in role_dir:
            # Executor: 3 initial rows (system + context + role_instruction);
            # no skill is declared in v1.
            assert len(rows) >= 3, (
                f"{rel}: executor needs >=3 initial rows, got {len(rows)}"
            )
            assert (
                "Attempt Plan" in user_msg_1 or "Assigned Task" in user_msg_1
            ), f"{rel}: missing attempt plan / assigned task"
            role_instruction = texts[2]
            assert "# Terminal tools you may call" in role_instruction, (
                f"{rel}: row 3 missing terminal catalog heading"
            )
            # And the executor must NOT have a skill row — there is no
            # ``Load skill:`` prefix anywhere in the first three rows.
            for i in range(min(3, len(rows))):
                assert not texts[i].startswith("Load skill:"), (
                    f"{rel}: executor must not see a row-4 skill in v1"
                )
        elif "evaluator" in role_dir:
            assert len(rows) >= 3, (
                f"{rel}: evaluator needs >=3 initial rows, got {len(rows)}"
            )
            assert (
                "Evaluation Criteria" in user_msg_1
            ), f"{rel}: missing evaluation criteria"
            for i in range(min(3, len(rows))):
                assert not texts[i].startswith("Load skill:"), (
                    f"{rel}: evaluator must not see a row-4 skill in v1"
                )

    # 4) Emit the markdown report next to the run.
    report_path = report.run_dir / "first_three_messages_report.md"
    _write_report(report.run_dir, captured, report_path)
    assert report_path.exists()


def _text_of(row: dict) -> str:
    parts: list[str] = []
    for block in row.get("content", []) or []:
        if isinstance(block, dict) and block.get("type") == "text":
            parts.append(block.get("text", ""))
    return "\n".join(parts)


def _write_report(
    run_dir: Path, captured: dict[str, dict[str, object]], dest: Path
) -> None:
    lines: list[str] = []
    lines.append("# First-Three-Messages Capture — Live Run\n")
    lines.append(f"Source run directory: `{run_dir}`\n")
    lines.append(
        "Up to four rows per agent: system + composer's context block + "
        "role_instruction (with terminal catalog) + skill (row 4 — planner "
        "only in v1). Helpers (advisor / resolver) and subagent (explorer) "
        "are constructed by `scripts/build_first_three_messages_report.py` — "
        "see `docs/reports/first_three_messages_report.md`.\n"
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
        lines.append("**user_msg_1** (context block)\n")
        lines.append(f"```\n{user_msg_1.strip()[:6000]}\n```\n")
        if user_msg_2:
            lines.append("**user_msg_2** (role_instruction + terminal catalog)\n")
            lines.append(f"```\n{user_msg_2.strip()[:6000]}\n```\n")
        if skill_row:
            lines.append("**user_msg_3 — row 4** (skill + terminal_selection)\n")
            lines.append(f"```\n{skill_row.strip()[:6000]}\n```\n")
    dest.write_text("\n".join(lines) + "\n")
