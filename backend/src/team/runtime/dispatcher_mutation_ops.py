from __future__ import annotations

from typing import TYPE_CHECKING

from team.models import TERMINAL_STATUSES, RetryRequest, TaskStatus
from team.persistence.events import make_work_item_status

if TYPE_CHECKING:
    from team.runtime.dispatcher import Dispatcher


def _cascade_cancel_from_roots(dispatcher: "Dispatcher", roots: list[str]) -> None:
    stack = list(roots)
    seen: set[str] = set()
    while stack:
        cur = stack.pop()
        for other in dispatcher.graph.values():
            if cur in other.deps and other.id not in seen:
                seen.add(other.id)
                if other.status not in TERMINAL_STATUSES:
                    dispatcher._mark_cancelled(other, f"cascaded from {cur}")
                    stack.append(other.id)  # only propagate through cancelled nodes


def cascade_cancel(dispatcher: "Dispatcher", wi_id: str) -> None:
    _cascade_cancel_from_roots(dispatcher, [wi_id])


def cascade_cancel_dependency_subtree(dispatcher: "Dispatcher", wi_id: str) -> None:
    _cascade_cancel_from_roots(dispatcher, dispatcher._dependency_root_ids(wi_id))


async def fail(dispatcher: "Dispatcher", *, wi_id: str, reason: str) -> None:
    async with dispatcher.lock:
        wi = dispatcher.graph.get(wi_id)
        if wi is None or wi.status in TERMINAL_STATUSES:
            return

        # Check if any dependent has retry_first policy and retries remain
        if wi.retry_count < wi.max_retries:
            has_retry_first_dep = any(
                dep.cascade_policy == "retry_first"
                for dep in dispatcher.graph.values()
                if wi_id in dep.deps and dep.status not in TERMINAL_STATUSES
            )
            if has_retry_first_dep:
                # Retry the failed task instead of cascading
                wi.retry_count += 1
                wi.agent_run_id = None
                wi.started_at = None
                wi.finished_at = None
                wi.failure_reason = None
                wi.status = TaskStatus.PENDING
                dispatcher._emit(
                    make_work_item_status(
                        dispatcher.team_run_id,
                        wi_id,
                        "pending",
                        retry_count=wi.retry_count,
                        max_retries=wi.max_retries,
                    )
                )
                dispatcher._promote_ready_work_items()
                return

        dispatcher._mark_failed(wi, reason)

        # Cascade per dependent's policy
        for dep in list(dispatcher.graph.values()):
            if wi_id in dep.deps and dep.status not in TERMINAL_STATUSES:
                if dep.cascade_policy == "continue":
                    # Inject failure context note, let dependent proceed
                    from team.models import Note
                    import time
                    import uuid
                    tc = getattr(dispatcher, "task_center", None)
                    if tc is not None:
                        await tc.post(
                            Note(
                                id=str(uuid.uuid4()),
                                task_id=dep.id,
                                agent_name="system",
                                content=f"Warning: dependency {wi_id} failed: {reason}. "
                                "Proceed with caution.",
                                timestamp=time.time(),
                            )
                        )
                else:
                    # "cancel" (default) — cascade cancel
                    _cascade_cancel_from_roots(dispatcher, dispatcher._dependency_root_ids(wi_id))
                    return  # cascade handles all downstream at once


async def retry_work_item(
    dispatcher: "Dispatcher",
    *,
    wi_id: str,
    request: RetryRequest,
) -> None:
    async with dispatcher.lock:
        wi = dispatcher.graph[wi_id]
        if wi.status != TaskStatus.RUNNING:
            raise RuntimeError(f"retry: {wi_id} is {wi.status.value}, not RUNNING")
        if wi.retry_count >= wi.max_retries:
            dispatcher._mark_failed(wi, f"retry_exhausted: {request.reason}")
            cascade_cancel_dependency_subtree(dispatcher, wi_id)
            return
        wi.retry_count += 1
        wi.agent_run_id = None
        wi.started_at = None
        wi.finished_at = None
        wi.failure_reason = None
        wi.status = TaskStatus.PENDING
        dispatcher._emit(
            make_work_item_status(
                dispatcher.team_run_id,
                wi_id,
                "pending",
                agent_run_id=None,
                started_at=None,
                finished_at=None,
                failure_reason=None,
                retry_count=wi.retry_count,
                max_retries=wi.max_retries,
            )
        )
        dispatcher._promote_ready_work_items()

async def cancel_all_pending(dispatcher: "Dispatcher") -> None:
    async with dispatcher.lock:
        for wi in dispatcher.graph.values():
            if wi.status in (TaskStatus.PENDING, TaskStatus.READY):
                dispatcher._mark_cancelled(wi, "team_run cancelled")


async def cancel_running(dispatcher: "Dispatcher", *, reason: str) -> None:
    async with dispatcher.lock:
        for wi in dispatcher.graph.values():
            if wi.status == TaskStatus.RUNNING:
                dispatcher._mark_cancelled(wi, reason)
