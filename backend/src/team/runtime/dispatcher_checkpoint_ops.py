from __future__ import annotations

import copy
import uuid
from typing import TYPE_CHECKING, Any, Callable

from team.models import WorkItemStatus, _utcnow
from team.persistence.events import make_checkpoint_taken, make_work_item_status
from team.runtime.checkpoint import TeamRunCheckpoint

if TYPE_CHECKING:
    from team.runtime.dispatcher import Dispatcher


async def checkpoint(
    dispatcher: "Dispatcher",
    *,
    label: str | None,
    project_context: Any,
) -> TeamRunCheckpoint:
    async with dispatcher.lock:
        dispatcher._checkpoint_seq += 1
        cp = TeamRunCheckpoint(
            id=str(uuid.uuid4()),
            team_run_id=dispatcher.team_run_id,
            sequence=dispatcher._checkpoint_seq,
            taken_at=_utcnow(),
            label=label,
            work_items=copy.deepcopy(dispatcher.graph),
            ready_queue_order=list(dispatcher._ready_order),
            artifacts=dispatcher.artifact_store.snapshot(),
            project_context=copy.deepcopy(project_context),
            budget_state=copy.deepcopy(dispatcher.budget_state),
        )
        dispatcher._checkpoints.append(cp)
        dispatcher._emit(
            make_checkpoint_taken(
                dispatcher.team_run_id,
                checkpoint_id=cp.id,
                sequence=cp.sequence,
                label=label,
            )
        )
        return cp


async def rollback_to(
    dispatcher: "Dispatcher",
    *,
    checkpoint_id: str,
    project_context_setter: Callable[[Any], None],
) -> TeamRunCheckpoint:
    async with dispatcher.lock:
        cp = dispatcher._get_checkpoint(checkpoint_id)
        if cp is None:
            from team.errors import CheckpointNotFound
            raise CheckpointNotFound(checkpoint_id)
        dispatcher.graph = copy.deepcopy(cp.work_items)
        dispatcher.artifact_store.restore(cp.artifacts)
        dispatcher.budget_state.work_items_used = cp.budget_state.work_items_used
        dispatcher.budget_state.artifact_bytes_used = cp.budget_state.artifact_bytes_used
        dispatcher.budget_state.replans_used = cp.budget_state.replans_used
        project_context_setter(copy.deepcopy(cp.project_context))
        while not dispatcher._ready_queue.empty():
            dispatcher._ready_queue.get_nowait()
        dispatcher._ready_order = []
        for wi_id in cp.ready_queue_order:
            wi = dispatcher.graph.get(wi_id)
            if wi is not None and wi.status == WorkItemStatus.READY:
                dispatcher._ready_queue.put_nowait(wi_id)
                dispatcher._ready_order.append(wi_id)
        return cp


async def prepare_for_resume(dispatcher: "Dispatcher") -> None:
    async with dispatcher.lock:
        while not dispatcher._ready_queue.empty():
            dispatcher._ready_queue.get_nowait()
        dispatcher._ready_order = []
        for wi in dispatcher.graph.values():
            if wi.status == WorkItemStatus.RUNNING:
                wi.status = WorkItemStatus.READY
                wi.agent_run_id = None
                wi.started_at = None
                dispatcher._ready_queue.put_nowait(wi.id)
                dispatcher._ready_order.append(wi.id)
                dispatcher._emit(make_work_item_status(dispatcher.team_run_id, wi.id, "ready"))
                continue
            if wi.status == WorkItemStatus.READY:
                dispatcher._ready_queue.put_nowait(wi.id)
                dispatcher._ready_order.append(wi.id)
                continue
            if dispatcher._compute_readiness(wi):
                dispatcher._promote_to_ready(wi)
