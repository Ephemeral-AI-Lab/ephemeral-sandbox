from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from team.context.project import ProjectContext
from team.models import BudgetConfig, BudgetState
from team.persistence.run_store import TeamRunStore, build_default_store

if TYPE_CHECKING:
    from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker
    from team.persistence.file_change_store import FileChangeStore, NullFileChangeStore
    from team.runtime.dispatch_queue import DispatchQueue
    from team.task_center import TaskCenter


@dataclass(frozen=True)
class TeamRuntimeServices:
    project_context: ProjectContext
    task_center: "TaskCenter"
    dispatch_queue: "DispatchQueue"
    event_store: TeamRunStore
    file_change_store: "FileChangeStore | NullFileChangeStore | None" = None


def build_team_runtime_services(
    *,
    team_run_id: str,
    budgets: BudgetConfig,
    budget_state: BudgetState,
    user_request: str,
    goal: str | None = None,
    repo_root: str | None = None,
    event_store: TeamRunStore | None = None,
    session_factory: "async_sessionmaker[AsyncSession] | None" = None,
) -> TeamRuntimeServices:
    from team.persistence.team_engine import create_team_engine
    from team.runtime.dispatch_queue import DispatchQueue
    from team.task_center import TaskCenter

    project_context = ProjectContext(
        goal=goal or user_request, user_request=user_request,
        repo_root=repo_root or "", project_key=repo_root or "",
    )
    store = event_store if event_store is not None else build_default_store()

    task_session_factory = session_factory
    if task_session_factory is None:
        try:
            _, task_session_factory = create_team_engine()
        except RuntimeError as exc:
            raise RuntimeError(
                "Team runtime requires a configured async database. "
                "Set EPHEMERALOS_DATABASE_URL or pass session_factory explicitly."
            ) from exc

    from team.persistence.file_change_store import FileChangeStore
    file_change_store: Any = FileChangeStore()

    task_center = TaskCenter(
        session_factory=task_session_factory,
        team_run_id=team_run_id,
        budgets=budgets,
        budget_state=budget_state,
        goal=goal or "",
        user_request=user_request,
        file_change_store=file_change_store,
        event_store=store,
    )
    dispatch_queue = DispatchQueue(task_session_factory)

    return TeamRuntimeServices(
        project_context=project_context,
        task_center=task_center,
        dispatch_queue=dispatch_queue,
        event_store=store,
        file_change_store=file_change_store,
    )
