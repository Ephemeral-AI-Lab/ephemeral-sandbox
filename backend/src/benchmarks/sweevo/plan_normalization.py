"""Benchmark-specific plan payload normalization for SWE-EVO runs.

Extracted from ``SubmitPlanTool`` so the generic submit_plan tool stays
benchmark-agnostic. These functions are called from
``SubmitPlanTool._build_payload`` only when benchmark context is detected.
"""

from __future__ import annotations

import os
import re
from typing import Any

_BENCHMARK_COMMAND_KEYS = ("reproduction", "verification", "verify", "retries")
_CD_COMMAND_RE = re.compile(
    r"^\s*cd\s+(?P<quote>['\"]?)(?P<path>[^&|;'\"]+?)(?P=quote)\s*&&\s*(?P<rest>.+?)\s*$"
)
_PARAM_CASE_SUFFIX_RE = re.compile(r"#\d+(?:-\d+)*$")


def extract_benchmark_targets_from_team_run(
    team_run_id: str,
) -> tuple[set[str] | None, set[str] | None]:
    """Extract benchmark test IDs and file paths from the root work item."""
    if not team_run_id:
        return None, None
    try:
        from team.runtime.registry import get as get_team_run

        team_run = get_team_run(team_run_id)
    except Exception:
        return None, None
    if team_run is None:
        return None, None
    graph = getattr(getattr(team_run, "dispatcher", None), "graph", None)
    root_id = getattr(team_run, "root_work_item_id", None)
    if not isinstance(graph, dict) or not isinstance(root_id, str):
        return None, None
    root = graph.get(root_id)
    payload = getattr(root, "payload", None) if root is not None else None
    if not isinstance(payload, dict):
        return None, None
    fail_to_pass = payload.get("fail_to_pass")
    pass_to_pass = payload.get("pass_to_pass")
    test_ids = {
        str(item).strip()
        for item in (fail_to_pass or [])
        if isinstance(item, str) and str(item).strip()
    }
    test_ids.update(
        str(item).strip()
        for item in (pass_to_pass or [])
        if isinstance(item, str) and str(item).strip()
    )
    if not test_ids:
        return None, None
    test_files = {
        item.split("::", 1)[0]
        for item in test_ids
        if "::" in item and item.split("::", 1)[0]
    }
    return test_ids, test_files


def normalize_benchmark_command_payloads(
    plan_data: dict[str, Any],
    *,
    repo_dir: str,
    benchmark_test_ids: set[str] | None,
    benchmark_test_files: set[str] | None,
) -> dict[str, Any]:
    """Normalize benchmark-specific payload fields in plan items."""
    if not benchmark_test_ids and not benchmark_test_files:
        return plan_data

    items = plan_data.get("items")
    if not isinstance(items, list):
        return plan_data

    normalized_items: list[dict[str, Any]] = []
    for item in items:
        if not isinstance(item, dict):
            normalized_items.append(item)
            continue
        payload = item.get("payload")
        if not isinstance(payload, dict):
            normalized_items.append(item)
            continue
        normalized_payload = dict(payload)
        owned_failures = normalized_payload.get("owned_failures")
        if isinstance(owned_failures, list):
            normalized_payload["owned_failures"] = [
                _normalize_owned_failure(
                    value,
                    benchmark_test_ids=benchmark_test_ids,
                    benchmark_test_files=benchmark_test_files,
                )
                if isinstance(value, str)
                else value
                for value in owned_failures
            ]
        for key in _BENCHMARK_COMMAND_KEYS:
            raw_value = normalized_payload.get(key)
            if not repo_dir:
                continue
            if isinstance(raw_value, str):
                normalized_payload[key] = _normalize_command(
                    raw_value, repo_dir=repo_dir
                )
            elif isinstance(raw_value, list):
                normalized_payload[key] = [
                    _normalize_command(value, repo_dir=repo_dir)
                    if isinstance(value, str)
                    else value
                    for value in raw_value
                ]
        normalized_items.append({**item, "payload": normalized_payload})

    return {**plan_data, "items": normalized_items}


def _normalize_owned_failure(
    value: str,
    *,
    benchmark_test_ids: set[str] | None,
    benchmark_test_files: set[str] | None,
) -> str:
    raw = value.strip()
    if not raw:
        return value
    test_ids = benchmark_test_ids or set()
    test_files = benchmark_test_files or set()
    if raw in test_ids or raw in test_files:
        return raw

    trimmed = _PARAM_CASE_SUFFIX_RE.sub("", raw)
    if trimmed in test_ids or trimmed in test_files:
        return trimmed

    file_candidate = trimmed.split("::", 1)[0].strip() if "::" in trimmed else trimmed
    if file_candidate in test_files:
        return file_candidate

    basename = os.path.basename(file_candidate)
    if basename:
        matches = [path for path in test_files if os.path.basename(path) == basename]
        if len(matches) == 1:
            return matches[0]

    return raw


def _normalize_command(value: str, *, repo_dir: str) -> str:
    match = _CD_COMMAND_RE.match(value)
    if match is None:
        return value

    raw_target = match.group("path").strip()
    if not os.path.isabs(raw_target):
        return value

    repo_root = os.path.normpath(repo_dir)
    target_root = os.path.normpath(raw_target)
    try:
        stays_inside_repo = os.path.commonpath([repo_root, target_root]) == repo_root
    except ValueError:
        stays_inside_repo = False
    if target_root == repo_root or stays_inside_repo:
        return value
    return match.group("rest").strip()
