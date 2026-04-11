"""``submit_plan`` tool — stashes a validated Plan in ``ctx.tool_metadata``."""

from __future__ import annotations

import os
import re
from typing import Any

from pydantic import BaseModel, Field, field_validator

from team.models import Plan, WorkItemKind
from team.planning.validation import validate_plan_phase_a
from tools.core.base import ToolExecutionContext
from tools.posthook.base import SubmitPosthookTool, _decode_json_array_string

_BENCHMARK_COMMAND_KEYS = ("reproduction", "verification", "verify", "retries")
_CD_COMMAND_RE = re.compile(
    r"^\s*cd\s+(?P<quote>['\"]?)(?P<path>[^&|;'\"]+?)(?P=quote)\s*&&\s*(?P<rest>.+?)\s*$"
)
_PARAM_CASE_SUFFIX_RE = re.compile(r"#\d+(?:-\d+)*$")


def _looks_like_validator_payload(payload: dict[str, Any]) -> bool:
    return any(key in payload for key in ("verify", "verification", "retries", "reproduction"))


def _normalize_submit_plan_item_shape(item: Any) -> Any:
    if not isinstance(item, dict):
        return item

    normalized = dict(item)
    payload = normalized.get("payload")
    payload_dict = dict(payload) if isinstance(payload, dict) else {}

    if "local_id" not in normalized and isinstance(normalized.get("id"), str):
        normalized["local_id"] = normalized["id"]

    if "briefings" not in normalized and isinstance(payload_dict.get("briefings"), list):
        normalized["briefings"] = payload_dict.pop("briefings")

    if "agent_name" not in normalized and isinstance(normalized.get("agent"), str):
        normalized["agent_name"] = normalized["agent"]

    raw_agent_name = normalized.get("agent_name")
    local_id = normalized.get("local_id")
    if isinstance(raw_agent_name, str):
        agent_name = raw_agent_name.strip()
        if agent_name not in {"developer", "validator", "team_planner"}:
            inferred_agent = None
            explicit_agent = normalized.get("agent")
            if isinstance(explicit_agent, str) and explicit_agent.strip() in {
                "developer",
                "validator",
                "team_planner",
            }:
                inferred_agent = explicit_agent.strip()
            elif normalized.get("kind") == WorkItemKind.EXPANDABLE.value:
                inferred_agent = "team_planner"
            elif agent_name.startswith("validate") or agent_name.startswith("validator"):
                inferred_agent = "validator"
            elif _looks_like_validator_payload(payload_dict) and normalized.get("deps"):
                inferred_agent = "validator"
            else:
                inferred_agent = "developer"

            normalized["agent_name"] = inferred_agent
            if not isinstance(local_id, str) or not local_id.strip():
                normalized["local_id"] = agent_name

    if payload_dict:
        normalized["payload"] = payload_dict

    return normalized


def _optional_int(value: Any) -> int | None:
    if value in (None, ""):
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


class _SubmitBriefing(BaseModel):
    name: str
    source: str  # "artifact" | "inline"
    ref: str | None = None
    inline: str | None = None
    description: str | None = None


class _SubmitPlanItem(BaseModel):
    agent_name: str
    payload: dict[str, Any] = Field(default_factory=dict)
    local_id: str | None = None
    deps: list[str] = Field(default_factory=list)
    notes: str | None = None
    timeout_seconds: float | None = None
    kind: WorkItemKind = WorkItemKind.ATOMIC
    briefings: list[_SubmitBriefing] = Field(default_factory=list)


class SubmitPlanInput(BaseModel):
    items: list[_SubmitPlanItem]
    rationale: str | None = None

    @field_validator("items", mode="before")
    @classmethod
    def _deserialize_items(cls, value: Any) -> Any:
        raw_items = _decode_json_array_string(value)
        if not isinstance(raw_items, list):
            return raw_items
        return [_normalize_submit_plan_item_shape(item) for item in raw_items]


class SubmitPlanTool(SubmitPosthookTool):
    name: str = "submit_plan"
    description: str = (
        "Submit a Plan to extend the team's DAG. Each item names an existing "
        "agent and an optional list of dependency local_ids or external "
        "work_item_ids. Validation runs synchronously: if any structural "
        "issue is found the tool returns a structured error and you MUST "
        "fix it and call submit_plan again."
    )
    input_model = SubmitPlanInput
    default_metadata_key: str = "submitted_plan"

    def _build_payload(
        self, arguments: BaseModel, context: ToolExecutionContext
    ) -> tuple[Any, str | None]:
        assert isinstance(arguments, SubmitPlanInput)
        benchmark_test_ids, benchmark_test_files = self._known_benchmark_targets(context)
        raw_plan = self._normalize_benchmark_command_payloads(
            arguments.model_dump(),
            context=context,
            benchmark_test_ids=benchmark_test_ids,
            benchmark_test_files=benchmark_test_files,
        )
        try:
            plan = Plan.from_dict(raw_plan)
        except Exception as exc:
            return None, f"Invalid Plan shape: {exc}"

        max_plan_size = int(context.metadata.get("max_plan_size", 50) or 50)
        max_validators_per_plan = _optional_int(context.metadata.get("max_validators_per_plan"))
        require_validator_for_plan_size = _optional_int(
            context.metadata.get("require_validator_for_plan_size")
        )
        issues = validate_plan_phase_a(
            plan,
            max_plan_size=max_plan_size,
            allow_empty=self._allow_empty_plan(context),
            known_external_deps=self._known_external_dep_ids(context),
            benchmark_test_ids=benchmark_test_ids,
            benchmark_test_files=benchmark_test_files,
            max_validators_per_plan=max_validators_per_plan,
            require_validator_for_plan_size=require_validator_for_plan_size,
        )
        if issues:
            lines = [f"- {i['field']}: {i['msg']}" for i in issues]
            return None, (
                "invalid_plan:\n"
                + "\n".join(lines)
                + "\n\nFix the issues above and call submit_plan again."
            )
        return plan, None

    def _accepted_message(self, payload: Any) -> str:
        assert isinstance(payload, Plan)
        return f"Plan accepted: {len(payload.items)} item(s) queued for dispatch."

    def _allow_empty_plan(self, context: ToolExecutionContext) -> bool:
        team_run_id = str(context.metadata.get("team_run_id") or "").strip()
        work_item_id = str(context.metadata.get("work_item_id") or "").strip()
        if not team_run_id or not work_item_id:
            return False
        try:
            from team.runtime.registry import get as get_team_run

            team_run = get_team_run(team_run_id)
        except Exception:
            return False
        if team_run is None:
            return False
        root_id = str(getattr(team_run, "root_work_item_id", "") or "")
        if not root_id or work_item_id == root_id:
            return False
        graph = getattr(getattr(team_run, "dispatcher", None), "graph", None)
        if not isinstance(graph, dict):
            return False
        work_item = graph.get(work_item_id)
        if work_item is None:
            return False
        return (
            str(getattr(work_item, "agent_name", "") or "") == "team_planner"
            and getattr(work_item, "kind", None) == WorkItemKind.EXPANDABLE
        )

    def _known_external_dep_ids(self, context: ToolExecutionContext) -> set[str] | None:
        team_run_id = str(context.metadata.get("team_run_id") or "").strip()
        if not team_run_id:
            return None
        try:
            from team.runtime.registry import get as get_team_run

            team_run = get_team_run(team_run_id)
        except Exception:
            return None
        if team_run is None:
            return None
        graph = getattr(getattr(team_run, "dispatcher", None), "graph", None)
        if not isinstance(graph, dict):
            return None
        return {str(wi_id) for wi_id in graph}

    def _known_benchmark_targets(
        self, context: ToolExecutionContext
    ) -> tuple[set[str] | None, set[str] | None]:
        team_run_id = str(context.metadata.get("team_run_id") or "").strip()
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

    def _normalize_benchmark_command_payloads(
        self,
        plan_data: dict[str, Any],
        *,
        context: ToolExecutionContext,
        benchmark_test_ids: set[str] | None,
        benchmark_test_files: set[str] | None,
    ) -> dict[str, Any]:
        if not benchmark_test_ids and not benchmark_test_files:
            return plan_data
        repo_dir = str(
            context.metadata.get("daytona_cwd")
            or context.metadata.get("ci_workspace_root")
            or ""
        ).strip()
        if not repo_dir:
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
                    self._normalize_benchmark_owned_failure(
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
                if isinstance(raw_value, str):
                    normalized_payload[key] = self._normalize_benchmark_command(
                        raw_value, repo_dir=repo_dir
                    )
                elif isinstance(raw_value, list):
                    normalized_payload[key] = [
                        self._normalize_benchmark_command(value, repo_dir=repo_dir)
                        if isinstance(value, str)
                        else value
                        for value in raw_value
                    ]
            normalized_items.append({**item, "payload": normalized_payload})

        return {**plan_data, "items": normalized_items}

    def _normalize_benchmark_owned_failure(
        self,
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

    def _normalize_benchmark_command(self, value: str, *, repo_dir: str) -> str:
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
