"""Worker pull loop. Pops ready WorkItems and drives them through execute_with_posthook."""

from __future__ import annotations

import asyncio
import logging
import uuid
from collections.abc import Awaitable
from typing import TYPE_CHECKING, Any, Callable

from hooks.agent_posthook import NoPosthookOutput, execute_with_posthook
from team.models import AgentResult, Plan
from team.runtime.context_builder import TeamAgentContext
from tools.posthook import SubmittedSummary

if TYPE_CHECKING:
    from agents.types import AgentDefinition
    from team.models import WorkItem
    from team.runtime.team_run import TeamRun

logger = logging.getLogger(__name__)

QueryRunner = Callable[["AgentDefinition", Any], Awaitable[Any]]
QueryContextBuilder = Callable[["AgentDefinition", "TeamRun", "WorkItem"], TeamAgentContext]
PosthookContextBuilder = Callable[["AgentDefinition", Any], TeamAgentContext]


class Executor:
    """Runtime invariant: every team agent MUST submit through a posthook.

    Either ``Plan`` (planner) or ``SubmittedSummary`` (worker / scout /
    validator). Anything else fails the WorkItem with a grep-able reason.
    Wire ``submit_summary_agent`` as the default posthook for any agent
    that does not have a domain-specific submission.
    """

    def __init__(
        self,
        team_run: "TeamRun",
        runner: QueryRunner,
        build_query_context: QueryContextBuilder,
        build_posthook_context: PosthookContextBuilder,
        agent_lookup: Callable[[str], "AgentDefinition | None"],
        after_dispatch: Callable[["WorkItem", AgentResult, list["WorkItem"]], Any] | None = None,
    ) -> None:
        self.team_run = team_run
        self.runner = runner
        self.build_query_context = build_query_context
        self.build_posthook_context = build_posthook_context
        self.agent_lookup = agent_lookup
        self.after_dispatch = after_dispatch

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
        try:
            # work_result is consumed for posthook side-effects only; the
            # final dispatch result is built from ``submitted`` below.
            execution = execute_with_posthook(
                work_defn=defn,
                work_ctx=query_ctx,
                runner=self.runner,
                agent_lookup=self.agent_lookup,
                posthook_ctx_builder=self.build_posthook_context,
            )
            _, submitted = await execution
        except NoPosthookOutput as exc:
            await dispatcher.fail(wi_id, f"NoPosthookOutput: {exc}")
            return

        if submitted is None:
            await dispatcher.fail(
                wi_id,
                "no_posthook_submission: team agents must submit via a posthook "
                "(use submit_summary_agent if no domain-specific posthook applies)",
            )
            return
        if isinstance(submitted, Plan):
            result = AgentResult(artifact=None, summary="", submitted_plan=submitted)
        elif isinstance(submitted, SubmittedSummary):
            result = AgentResult(
                artifact=submitted.artifact,
                summary=submitted.summary,
                submitted_plan=None,
            )
        else:
            await dispatcher.fail(
                wi_id, f"unexpected_submission_type: {type(submitted).__name__}"
            )
            return

        new_items = await dispatcher.complete(wi_id, result)
        if self.after_dispatch is not None:
            callback_result = self.after_dispatch(wi, result, new_items)
            if isinstance(callback_result, Awaitable):
                await callback_result
