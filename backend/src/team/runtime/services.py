from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from code_intelligence.editing.arbiter import Arbiter
from team.context.project import ProjectContext
from team.models import BudgetConfig, BudgetState
from team.persistence.run_store import TeamRunStore, build_default_store

if TYPE_CHECKING:
    from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker
    from team.task_center import TaskCenter


@dataclass(frozen=True)
class TeamRuntimeServices:
    project_context: ProjectContext
    task_center: "TaskCenter"
    event_store: TeamRunStore
    arbiter: Arbiter | None = None


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
    from team.task_center import TaskCenter

    project_context = ProjectContext(
        goal=goal or user_request,
        user_request=user_request,
        repo_root=repo_root or "",
        project_key=repo_root or "",
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

    arbiter: Any = Arbiter(workspace_root=repo_root or "")

    task_center = TaskCenter(
        session_factory=task_session_factory,
        team_run_id=team_run_id,
        budgets=budgets,
        budget_state=budget_state,
        arbiter=arbiter,
        event_store=store,
    )

    return TeamRuntimeServices(
        project_context=project_context,
        task_center=task_center,
        event_store=store,
        arbiter=arbiter,
    )
