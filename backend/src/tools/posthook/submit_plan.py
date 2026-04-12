"""``submit_plan`` tool — stashes a validated Plan in ``ctx.tool_metadata``."""

from __future__ import annotations

import json
from typing import Any

from pydantic import BaseModel, Field, field_validator

from team.models import Plan, WorkItemKind
from team.planning.validation import validate_plan_phase_a
from tools.core.base import ToolExecutionContext
from tools.posthook.base import SubmitPosthookTool


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
            explicit_agent_name = "agent_name" in item
            local_id_like_name = any(sep in agent_name for sep in ("_", "-"))
            inferred_agent = None
            explicit_agent = normalized.get("agent")
            if isinstance(explicit_agent, str) and explicit_agent.strip() in {
                "developer",
                "validator",
                "team_planner",
            }:
                inferred_agent = explicit_agent.strip()
            elif (not explicit_agent_name or local_id_like_name) and normalized.get("kind") == WorkItemKind.EXPANDABLE.value:
                inferred_agent = "team_planner"
            elif (not explicit_agent_name or local_id_like_name) and (
                agent_name.startswith("validate") or agent_name.startswith("validator")
            ):
                inferred_agent = "validator"
            elif (
                (not explicit_agent_name or local_id_like_name)
                and _looks_like_validator_payload(payload_dict)
                and normalized.get("deps")
            ):
                inferred_agent = "validator"
            elif not explicit_agent_name or local_id_like_name:
                inferred_agent = "developer"
            else:
                inferred_agent = agent_name

            normalized["agent_name"] = inferred_agent
            if not isinstance(local_id, str) or not local_id.strip():
                normalized["local_id"] = agent_name

    if payload_dict:
        normalized["payload"] = payload_dict

    return normalized


def _is_submit_plan_item_list(value: Any) -> bool:
    return isinstance(value, list) and all(isinstance(item, dict) for item in value)


def _decode_submit_plan_items(value: Any) -> Any:
    """Decode only top-level arrays of plan-item objects.

    The generic array extractor is intentionally permissive so serializer
    agents can recover arrays embedded in prose. For submit_plan, that
    permissiveness can misfire on nested benchmark/id arrays inside an
    otherwise malformed item payload and turn ``items`` into ``list[str]``.
    Restrict recovery here to arrays whose elements are object-shaped plan
    items.
    """
    if not isinstance(value, str):
        return value

    text = value.strip()
    if not text:
        return value

    if text.startswith("["):
        try:
            payload = json.loads(text)
        except json.JSONDecodeError:
            payload = None
        if _is_submit_plan_item_list(payload):
            return payload

    decoder = json.JSONDecoder()
    best_payload: list[dict[str, Any]] | None = None
    best_start: int | None = None
    best_end = -1
    for start, char in enumerate(text):
        if char != "[":
            continue
        try:
            payload, end = decoder.raw_decode(text, idx=start)
        except ValueError:
            continue
        if not _is_submit_plan_item_list(payload):
            continue
        if end > best_end or (end == best_end and (best_start is None or start < best_start)):
            best_payload = payload
            best_start = start
            best_end = end
    return best_payload if best_payload is not None else value


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
        raw_items = _decode_submit_plan_items(value)
        if isinstance(value, str) and raw_items is value:
            raise ValueError(
                "`items` must be a real list of plan item objects or a JSON array string "
                'that decodes to objects like {"agent_name":"developer","local_id":"w1","payload":{}}.'
            )
        if not isinstance(raw_items, list):
            return raw_items
        bad_entries: list[str] = []
        normalized_items: list[Any] = []
        for index, item in enumerate(raw_items):
            if isinstance(item, _SubmitPlanItem):
                normalized_items.append(item)
                continue
            if not isinstance(item, dict):
                preview = repr(item)
                if len(preview) > 80:
                    preview = preview[:77] + "..."
                bad_entries.append(f"{index}={preview}")
                continue
            normalized_items.append(_normalize_submit_plan_item_shape(item))
        if bad_entries:
            joined = ", ".join(bad_entries[:5])
            raise ValueError(
                "`items` must contain plan item objects, not bare strings or other scalars. "
                'Each item should look like {"agent_name":"developer","local_id":"w1","payload":{}}. '
                f"Invalid entries: {joined}"
            )
        return normalized_items


class SubmitPlanTool(SubmitPosthookTool):
    name: str = "submit_plan"
    description: str = (
        "Submit a Plan to extend the team's DAG. Each item names an existing "
        "agent and an optional list of dependency local_ids or external "
        "work_item_ids. `items` must be a list of object-shaped plan items "
        "with fields such as `agent_name`, optional `local_id`, `payload`, "
        "`deps`, and `kind` (`atomic` or `expandable`) — never a list of "
        "test ids or other bare strings. Validation runs synchronously: if "
        "any structural issue is found the tool returns a structured error "
        "and you MUST fix it and call submit_plan again."
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
        extra_validators = self._build_extra_validators(
            benchmark_test_ids=benchmark_test_ids,
            benchmark_test_files=benchmark_test_files,
        )
        issues = validate_plan_phase_a(
            plan,
            max_plan_size=max_plan_size,
            allow_empty=self._allow_empty_plan(context),
            known_external_deps=self._known_external_dep_ids(context),
            max_validators_per_plan=max_validators_per_plan,
            require_validator_for_plan_size=require_validator_for_plan_size,
            extra_validators=extra_validators,
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

    def _build_extra_validators(
        self,
        *,
        benchmark_test_ids: set[str] | None,
        benchmark_test_files: set[str] | None,
    ) -> list[Any] | None:
        if not benchmark_test_ids and not benchmark_test_files:
            return None
        try:
            from benchmarks.sweevo.plan_validation import (
                build_benchmark_payload_ref_validator,
            )

            return [
                build_benchmark_payload_ref_validator(
                    benchmark_test_ids=benchmark_test_ids or set(),
                    benchmark_test_files=benchmark_test_files or set(),
                )
            ]
        except ImportError:
            return None

    def _known_benchmark_targets(
        self, context: ToolExecutionContext
    ) -> tuple[set[str] | None, set[str] | None]:
        team_run_id = str(context.metadata.get("team_run_id") or "").strip()
        try:
            from benchmarks.sweevo.plan_normalization import (
                extract_benchmark_targets_from_team_run,
            )

            return extract_benchmark_targets_from_team_run(team_run_id)
        except ImportError:
            return None, None

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
        try:
            from benchmarks.sweevo.plan_normalization import (
                normalize_benchmark_command_payloads,
            )

            return normalize_benchmark_command_payloads(
                plan_data,
                repo_dir=repo_dir,
                benchmark_test_ids=benchmark_test_ids,
                benchmark_test_files=benchmark_test_files,
            )
        except ImportError:
            return plan_data
