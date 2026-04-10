from __future__ import annotations

from datetime import datetime
from typing import TYPE_CHECKING, Any

from team.models import (
    Briefing,
    BudgetConfig,
    BudgetState,
    DependencyArtifact,
    TeamRunStatus,
    WorkItem,
    WorkItemKind,
    WorkItemStatus,
)
from team.persistence.run_store import TeamRunStore
from team.runtime.dispatcher import Dispatcher
from team.runtime.services import TeamRuntimeServices, build_team_runtime_services

if TYPE_CHECKING:
    from team.persistence.events import TeamRunEvent
    from team.runtime.team_run import TeamRun


def build_resumed_run(
    *,
    team_run_cls: type["TeamRun"],
    store: TeamRunStore,
    team_run_id: str,
    created_event: "TeamRunEvent",
) -> tuple[TeamRuntimeServices, "TeamRun"]:
    meta = created_event.data
    budgets = budget_config_from_event(meta)
    services = build_team_runtime_services(
        team_run_id=team_run_id,
        budgets=budgets,
        budget_state=BudgetState(),
        user_request=meta.get("user_request") or "",
        goal=meta.get("goal"),
        repo_root=meta.get("repo_root") or None,
        event_store=store,
    )
    run = team_run_cls(
        session_id=meta.get("session_id") or "",
        user_request=meta.get("user_request") or "",
        budgets=budgets,
        goal=meta.get("goal"),
        sandbox_id=meta.get("sandbox_id") or None,
        repo_root=meta.get("repo_root") or None,
        team_run_id=team_run_id,
        services=services,
    )
    return services, run


def budget_config_from_event(meta: dict[str, Any]) -> BudgetConfig:
    valid_keys = set(BudgetConfig.__dataclass_fields__.keys())
    return BudgetConfig(**{k: v for k, v in dict(meta.get("budgets") or {}).items() if k in valid_keys})


def restore_ready_queue(
    *,
    dispatcher: Dispatcher,
    graph: dict[str, WorkItem],
) -> list[str]:
    ready_order: list[str] = []
    for wi in graph.values():
        if wi.status == WorkItemStatus.READY:
            dispatcher._ready_queue.put_nowait(wi.id)
            ready_order.append(wi.id)
    return ready_order


def work_item_from_dict(data: dict[str, Any]) -> WorkItem:
    def _parse_dt(iso: str | None) -> datetime | None:
        return datetime.fromisoformat(iso) if iso else None

    return WorkItem(
        id=data["id"],
        team_run_id=data["team_run_id"],
        agent_name=data["agent_name"],
        status=WorkItemStatus(data["status"]),
        kind=WorkItemKind(data.get("kind", "atomic")),
        deps=list(data.get("deps") or []),
        parent_id=data.get("parent_id"),
        root_id=data.get("root_id") or "",
        agent_run_id=data.get("agent_run_id"),
        payload=dict(data.get("payload") or {}),
        artifact_ref=data.get("artifact_ref"),
        timeout_seconds=data.get("timeout_seconds"),
        depth=int(data.get("depth") or 0),
        local_id=data.get("local_id"),
        briefings=[Briefing(**b) for b in (data.get("briefings") or [])],
        dep_artifacts=[DependencyArtifact(**d) for d in (data.get("dep_artifacts") or [])],
        created_at=_parse_dt(data.get("created_at")) or datetime.now(),
        started_at=_parse_dt(data.get("started_at")),
        finished_at=_parse_dt(data.get("finished_at")),
        failure_reason=data.get("failure_reason"),
        retry_count=int(data.get("retry_count") or 0),
        max_retries=int(data.get("max_retries") or 2),
        replan_source_id=data.get("replan_source_id"),
    )


def apply_replayed_event(
    *,
    event: "TeamRunEvent",
    graph: dict[str, WorkItem],
    services: TeamRuntimeServices,
    root_id: str | None,
) -> tuple[str | None, tuple[int, int, int] | None, str | None]:
    last_budget: tuple[int, int, int] | None = None
    final_status: str | None = None
    if event.kind == "work_item_added":
        wi = work_item_from_dict(event.data["work_item"])
        graph[wi.id] = wi
        if wi.depth == 0 and root_id is None:
            root_id = wi.id
    elif event.kind == "work_item_status":
        wi = graph.get(event.data["wi_id"])
        if wi is not None:
            wi.status = WorkItemStatus(event.data["status"])
            for key in ("started_at", "finished_at"):
                iso = event.data.get(key)
                if iso:
                    setattr(wi, key, datetime.fromisoformat(iso))
            if "agent_run_id" in event.data:
                wi.agent_run_id = event.data["agent_run_id"]
            if "failure_reason" in event.data:
                wi.failure_reason = event.data["failure_reason"]
            if "artifact_ref" in event.data:
                wi.artifact_ref = event.data["artifact_ref"]
    elif event.kind == "artifact_written":
        try:
            services.artifact_store.save(event.data["wi_id"], event.data["payload"])
        except Exception:
            pass
    elif event.kind == "budget_update":
        last_budget = (
            int(event.data["work_items_used"]),
            int(event.data["artifact_bytes_used"]),
            int(event.data.get("replans_used") or 0),
        )
    elif event.kind == "team_run_status":
        status = event.data.get("status")
        if status:
            final_status = TeamRunStatus(status).value
    return root_id, last_budget, final_status
