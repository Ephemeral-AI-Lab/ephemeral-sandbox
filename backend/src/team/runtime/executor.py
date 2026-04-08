"""Worker pull loop. Pops ready WorkItems and drives them through execute_with_posthook."""

from __future__ import annotations

import asyncio
import logging
import uuid
from typing import TYPE_CHECKING, Any, Awaitable, Callable

from hooks.agent_posthook import NoPosthookOutput, execute_with_posthook
from team.models import AgentResult, Plan
from tools.posthook import SubmittedSummary

if TYPE_CHECKING:
    from agents.types import AgentDefinition
    from team.models import WorkItem
    from team.runtime.team_run import TeamRun

logger = logging.getLogger(__name__)

QueryRunner = Callable[["AgentDefinition", Any], Awaitable[Any]]
QueryContextBuilder = Callable[["AgentDefinition", "TeamRun", "WorkItem"], Any]
PosthookContextBuilder = Callable[["AgentDefinition", Any], Any]
ResultExtractor = Callable[[Any, "WorkItem"], AgentResult]


class Executor:
    def __init__(
        self,
        team_run: "TeamRun",
        runner: QueryRunner,
        build_query_context: QueryContextBuilder,
        build_posthook_context: PosthookContextBuilder,
        extract_result: ResultExtractor,
        agent_lookup: Callable[[str], "AgentDefinition | None"],
    ) -> None:
        self.team_run = team_run
        self.runner = runner
        self.build_query_context = build_query_context
        self.build_posthook_context = build_posthook_context
        self.extract_result = extract_result
        self.agent_lookup = agent_lookup

    async def run_forever(self) -> None:
        """Pop READY items until cancel_event is set.

        Workers MUST NOT exit just because the graph is momentarily terminal —
        a peer worker may still complete a planner that submits a fresh Plan,
        re-populating the queue. Only ``TeamRun`` decides when workers stop,
        via ``cancel_event``.
        """
        dispatcher = self.team_run.dispatcher
        while not self.team_run.cancel_event.is_set():
            try:
                wi_id = await asyncio.wait_for(dispatcher.pop_ready(), timeout=0.1)
            except asyncio.TimeoutError:
                continue

            try:
                await self._run_one(wi_id)
            except Exception as exc:  # worker never dies
                logger.exception("Worker error on %s: %s", wi_id, exc)
                await dispatcher.fail(wi_id, f"worker_exception: {exc}")

    async def _run_one(self, wi_id: str) -> None:
        dispatcher = self.team_run.dispatcher
        agent_run_id = str(uuid.uuid4())
        wi = await dispatcher.mark_running(wi_id, agent_run_id)

        defn = self.agent_lookup(wi.agent_name)
        if defn is None:
            await dispatcher.fail(wi_id, f"unknown_agent: {wi.agent_name}")
            return

        query_ctx = self.build_query_context(defn, self.team_run, wi)
        timeout = (
            wi.timeout_seconds
            if wi.timeout_seconds is not None
            else self.team_run.budgets.default_work_item_timeout
        )

        try:
            work_result, submitted = await asyncio.wait_for(
                execute_with_posthook(
                    work_defn=defn,
                    work_ctx=query_ctx,
                    runner=self.runner,
                    agent_lookup=self.agent_lookup,
                    posthook_ctx_builder=self.build_posthook_context,
                ),
                timeout=timeout,
            )
        except asyncio.TimeoutError:
            await dispatcher.fail(wi_id, f"timeout after {timeout}s")
            return
        except NoPosthookOutput as exc:
            await dispatcher.fail(wi_id, f"NoPosthookOutput: {exc}")
            return

        result = self.extract_result(work_result, wi)
        if submitted is not None and result.submitted_plan is None and isinstance(submitted, Plan):
            result.submitted_plan = submitted
        if isinstance(submitted, SubmittedSummary):
            result.summary = submitted.summary
            if submitted.artifact is not None:
                result.artifact = submitted.artifact

        await dispatcher.complete(wi_id, result)
