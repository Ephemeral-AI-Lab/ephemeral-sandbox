from __future__ import annotations

from typing import TYPE_CHECKING

from team.models import TERMINAL_WI_STATUSES, RetryRequest, WorkItemStatus
from team.persistence.events import make_work_item_status

if TYPE_CHECKING:
    from team.runtime.dispatcher import Dispatcher


def cascade_cancel(dispatcher: "Dispatcher", wi_id: str) -> None:
    stack = [wi_id]
    seen: set[str] = set()
    while stack:
        cur = stack.pop()
        for other in dispatcher.graph.values():
            if cur in other.deps and other.id not in seen:
                seen.add(other.id)
                if other.status not in TERMINAL_WI_STATUSES:
                    dispatcher._mark_cancelled(other, f"cascaded from {wi_id}")
                stack.append(other.id)


async def fail(dispatcher: "Dispatcher", *, wi_id: str, reason: str) -> None:
    async with dispatcher.lock:
        wi = dispatcher.graph.get(wi_id)
        if wi is None or wi.status in TERMINAL_WI_STATUSES:
            return
        dispatcher._mark_failed(wi, reason)
        cascade_cancel(dispatcher, wi_id)


async def retry_work_item(
    dispatcher: "Dispatcher",
    *,
    wi_id: str,
    request: RetryRequest,
) -> None:
    async with dispatcher.lock:
        wi = dispatcher.graph[wi_id]
        if wi.status != WorkItemStatus.RUNNING:
            raise RuntimeError(f"retry: {wi_id} is {wi.status.value}, not RUNNING")
        if wi.retry_count >= wi.max_retries:
            dispatcher._mark_failed(wi, f"retry_exhausted: {request.reason}")
            cascade_cancel(dispatcher, wi_id)
            return
        wi.retry_count += 1
        wi.agent_run_id = None
        wi.started_at = None
        wi.status = WorkItemStatus.PENDING
        retries = wi.payload.setdefault("_retry_history", [])
        retries.append({"attempt": wi.retry_count, "reason": request.reason})
        dispatcher._emit(make_work_item_status(dispatcher.team_run_id, wi_id, "pending"))
        dispatcher._promote_to_ready(wi)

async def cancel_all_pending(dispatcher: "Dispatcher") -> None:
    async with dispatcher.lock:
        for wi in dispatcher.graph.values():
            if wi.status in (WorkItemStatus.PENDING, WorkItemStatus.READY):
                dispatcher._mark_cancelled(wi, "team_run cancelled")


async def cancel_running(dispatcher: "Dispatcher", *, reason: str) -> None:
    async with dispatcher.lock:
        for wi in dispatcher.graph.values():
            if wi.status == WorkItemStatus.RUNNING:
                dispatcher._mark_cancelled(wi, reason)
