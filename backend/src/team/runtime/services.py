from __future__ import annotations

from dataclasses import dataclass

from code_intelligence.atlas.identity import project_key_for
from team.artifacts.store import InMemoryArtifactStore
from team.context.project import ProjectContext
from team.models import BudgetConfig, BudgetState
from team.persistence.run_store import TeamRunStore, build_default_store
from team.runtime.dispatcher import Dispatcher


@dataclass(frozen=True)
class TeamRuntimeServices:
    project_context: ProjectContext
    artifact_store: InMemoryArtifactStore
    dispatcher: Dispatcher
    event_store: TeamRunStore


def build_team_runtime_services(
    *,
    team_run_id: str,
    budgets: BudgetConfig,
    budget_state: BudgetState,
    user_request: str,
    goal: str | None = None,
    repo_root: str | None = None,
    event_store: TeamRunStore | None = None,
) -> TeamRuntimeServices:
    project_context = ProjectContext(
        goal=goal or user_request,
        user_request=user_request,
        repo_root=repo_root or "",
        project_key=project_key_for(repo_root),
    )
    artifact_store = InMemoryArtifactStore(budgets, budget_state)
    store = event_store if event_store is not None else build_default_store()
    dispatcher = Dispatcher(
        team_run_id=team_run_id,
        budgets=budgets,
        budget_state=budget_state,
        artifact_store=artifact_store,
        event_store=store,
    )
    return TeamRuntimeServices(
        project_context=project_context,
        artifact_store=artifact_store,
        dispatcher=dispatcher,
        event_store=store,
    )
