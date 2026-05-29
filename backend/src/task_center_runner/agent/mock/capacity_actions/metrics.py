"""Capacity metrics prepared scripts."""

from __future__ import annotations

import json
from collections import Counter
from typing import Any

from task_center_runner.scenarios.base import ScenarioContext
from task_center_runner.agent.mock.tool_scripts import PreparedToolScript, ToolScriptStep
from tools.sandbox.read_file import read_file as read_file_tool
from tools.sandbox.write_file import write_file as write_file_tool

_CAPACITY_ROOT = ".ephemeralos/sweevo-mock/capacity"
_SUMMARY_PATH = f"{_CAPACITY_ROOT}/full-system-capacity-summary.json"
_PLANNED_GRAPH_PATH = ".metrics/planned_graph.json"


def full_system_capacity_metrics_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Write capacity-suite metrics and planned-graph artifacts through tools."""
    matrix = _dict_list(ctx.matrix_plan)
    packages = _dict_list(ctx.package_plan)
    requirements = _dict_list(ctx.requirement_ledger)
    tool_counts = _tool_counts(matrix)
    lsp_calls = sum(
        count for tool, count in tool_counts.items() if tool.startswith("lsp.")
    )
    run_id = str(
        ctx.metadata.get("task_center_run_id")
        or ctx.metadata.get("run_id")
        or "unknown-run"
    )
    planned_graph = {
        "schema": "live_e2e.capacity.planned_graph.v1",
        "scenario": "capacity.full_system_capacity_matrix",
        "task_center_run_id": run_id,
        "package_count": len(packages),
        "requirement_count": len(requirements),
        "matrix_cell_count": len(matrix),
        "subsystems": sorted(
            {
                str(cell.get("subsystem") or "")
                for cell in matrix
                if str(cell.get("subsystem") or "")
            }
        ),
        "final_edges": [
            ["final_reconciliation_check", "capacity_metrics_summary"],
            ["capacity_metrics_summary", "final_release_guard"],
        ],
    }
    summary = {
        "schema": "live_e2e.capacity.v1",
        "scenario": "capacity.full_system_capacity_matrix",
        "task_center_run_id": run_id,
        "profile": "project",
        "graph": {
            "workflows": 0,
            "iterations": 0,
            "attempts": 0,
            "tasks": 0,
            "max_depth": 0,
            "max_width": 0,
            "planned_packages": len(packages),
            "planned_requirements": len(requirements),
            "planned_matrix_cells": len(matrix),
        },
        "tool_use": {
            "total": sum(tool_counts.values()),
            "write_file": tool_counts.get("write_file", 0),
            "edit_file": tool_counts.get("edit_file", 0),
            "read_file": tool_counts.get("read_file", 0),
            "shell": tool_counts.get("shell", 0),
            "lsp": lsp_calls,
            "expected_errors": 0,
            "unexpected_errors": 0,
        },
        "sandbox": {
            "occ_commits": 0,
            "overlay_captures": 0,
            "squashes": 0,
            "conflicts": 0,
        },
        "context": {
            "packets": 0,
            "failed_packet_checks": 0,
        },
        "audit": {
            "message_logs": 0,
            "task_logs": 0,
            "sandbox_event_rows": 0,
        },
    }
    return PreparedToolScript(
        name="capacity_metrics_full_system",
        summary="Capacity metrics and planned graph artifacts written.",
        artifact=_SUMMARY_PATH,
        steps=(
            ToolScriptStep(
                "write-capacity-planned-graph",
                write_file_tool,
                {
                    "file_path": _PLANNED_GRAPH_PATH,
                    "content": _json(planned_graph),
                },
            ),
            ToolScriptStep(
                "write-capacity-summary",
                write_file_tool,
                {"file_path": _SUMMARY_PATH, "content": _json(summary)},
            ),
            ToolScriptStep(
                "read-capacity-planned-graph",
                read_file_tool,
                {"file_path": _PLANNED_GRAPH_PATH, "start_line": 1, "end_line": 80},
            ),
            ToolScriptStep(
                "read-capacity-summary",
                read_file_tool,
                {"file_path": _SUMMARY_PATH, "start_line": 1, "end_line": 120},
            ),
        ),
    )


def _tool_counts(matrix: list[dict[str, Any]]) -> Counter[str]:
    counts: Counter[str] = Counter()
    for cell in matrix:
        tools = cell.get("tool_names")
        if not isinstance(tools, (list, tuple)):
            continue
        for tool in tools:
            if str(tool or ""):
                counts[str(tool)] += 1
    return counts


def _dict_list(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, dict)]


def _json(value: dict[str, Any]) -> str:
    return json.dumps(value, indent=2, sort_keys=True) + "\n"


__all__ = ["full_system_capacity_metrics_script"]
