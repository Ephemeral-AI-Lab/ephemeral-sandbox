"""Conductor — deterministic blocker mechanics (no LLM calls)."""

from __future__ import annotations

import asyncio
import json
import logging
import time
import uuid
from typing import TYPE_CHECKING

from team.models import Blocker, BlockerStatus, TaskSpec, TaskStatus

if TYPE_CHECKING:
    from team.persistence.blocker_store import BlockerStore
    from team.runtime.team_run import TeamRun

logger = logging.getLogger(__name__)


class Conductor:
    """Deterministic blocker mechanics — no LLM calls.

    Assessment scope: all siblings of the initiating task plus their
    entire subtrees.  blast_radius is removed — the scope is defined
    structurally by the task tree, not by file paths.
    """

    def __init__(self, team_run: "TeamRun", blocker_store: "BlockerStore | None" = None) -> None:
        self._team_run = team_run
        self._blocker_store = blocker_store
        self._active_blockers: dict[str, Blocker] = {}
        self._executor_snapshots: dict[str, list[dict]] = {}  # task_id → display_messages snapshot

    async def restore(self) -> None:
        """Reload active blockers from the store after crash/restart."""
        if self._blocker_store is None:
            return
        blockers = await self._blocker_store.load_active()
        for b in blockers:
            self._active_blockers[b.id] = b
        if blockers:
            logger.info("Restored %d active blockers from store", len(blockers))

    async def _persist(self, blocker: Blocker) -> None:
        """Persist blocker state to durable store."""
        if self._blocker_store is not None:
            try:
                await self._blocker_store.save(blocker)
            except Exception:
                logger.warning("Failed to persist blocker %s", blocker.id, exc_info=True)

    # ------------------------------------------------------------------
    # Snapshot registry (called by executor after each tool result)
    # ------------------------------------------------------------------

    def register_snapshot(self, task_id: str, snapshot: list[dict]) -> None:
        self._executor_snapshots[task_id] = snapshot

    # ------------------------------------------------------------------
    # Active blocker queries
    # ------------------------------------------------------------------

    def active_blockers(self) -> list[Blocker]:
        """Return all active (non-resolved, non-failed) blockers."""
        return list(self._active_blockers.values())

    def has_active_blocker(self) -> bool:
        return bool(self._active_blockers)

    def blocker_for_fix_task(self, task_id: str) -> Blocker | None:
        for blocker in self._active_blockers.values():
            if blocker.fix_task_id == task_id:
                return blocker
        return None

    def guard_pop_ready(self, task: object) -> bool:
        """Allow only resolver tasks to dispatch while blockers are active."""
        if not self._active_blockers:
            return True
        task_id = str(getattr(task, "id", "") or "")
        return any(blocker.fix_task_id == task_id for blocker in self._active_blockers.values())

    # ------------------------------------------------------------------
    # Blocker lifecycle
    # ------------------------------------------------------------------

    async def create_blocker(
        self,
        reason: str,
        root_cause_paths: list[str],
        initiating_task_id: str,
        declared_by: str | None = None,
    ) -> Blocker:
        # Check for existing blocker with overlapping root_cause_paths — merge if found
        for existing in self._active_blockers.values():
            if self._paths_overlap(existing.root_cause_paths, root_cause_paths):
                merged_paths = list(dict.fromkeys(existing.root_cause_paths + root_cause_paths))
                existing.root_cause_paths = merged_paths
                logger.info("Merged blocker %s with new paths %s", existing.id, root_cause_paths)
                await self._persist(existing)
                # Re-assess: new paths may bring new siblings into scope
                await self._assess_running(existing)
                return existing

        blocker = Blocker(
            id=str(uuid.uuid4()),
            team_run_id=self._team_run.id,
            status=BlockerStatus.ASSESSING,
            reason=reason,
            root_cause_paths=root_cause_paths,
            initiating_task_id=initiating_task_id,
            declared_by=declared_by,
        )
        self._active_blockers[blocker.id] = blocker
        logger.info("Created blocker %s: %s", blocker.id, reason)
        await self._persist(blocker)

        await self._assess_running(blocker)

        blocker.status = BlockerStatus.FIXING
        await self._persist(blocker)
        await self._spawn_resolver(blocker)

        return blocker

    async def _assess_running(self, blocker: Blocker) -> None:
        """Assess all RUNNING siblings+descendants of the initiating task."""
        tc = self._team_run.task_center
        candidates = await tc.get_siblings_and_descendants(blocker.initiating_task_id)
        running = [r for r in candidates if r.status == TaskStatus.RUNNING.value]

        # Skip tasks already paused by this or another blocker
        running = [r for r in running if not getattr(r, 'blocker_id', None)]

        api_client = getattr(self._team_run, "api_client", None)

        async def _assess_one(rec: object) -> "PauseVerdict | None":
            from ephemeral_task import PauseVerdict, Snapshot, assess_pause

            task_id: str = rec.id  # type: ignore[attr-defined]
            agent_run_id = getattr(rec, "agent_run_id", None) or task_id
            snap = Snapshot(
                task_id=task_id,
                agent_run_id=agent_run_id,
                messages=self._executor_snapshots.get(task_id, []),
                system_prompt="You are a blocker assessment assistant.",
            )
            if api_client is None:
                logger.warning("No api_client on team_run; auto-pausing task %s", task_id)
                return PauseVerdict(
                    task_id=task_id,
                    answer="YES",
                    reason="no api_client available for assessment",
                    conversation=snap.messages,
                )
            try:
                verdict = await assess_pause(
                    snapshot=snap,
                    broken_files=blocker.root_cause_paths,
                    problem=blocker.reason,
                    api_client=api_client,
                )
            except Exception as exc:
                logger.warning("PauseAssessmentTask failed for %s: %s — skipping", task_id, exc)
                return None  # TIMEOUT / error → skip, don't pause
            return verdict

        verdicts = await asyncio.gather(*[_assess_one(r) for r in running])

        for verdict in verdicts:
            if verdict is None:
                continue
            if verdict.answer == "YES":
                logger.info("Pausing task %s (answer=YES)", verdict.task_id)
                await self._pause_task(verdict.task_id, blocker.id, verdict)
            else:
                logger.debug("Task %s not affected by blocker %s", verdict.task_id, blocker.id)

    async def _pause_task(self, task_id: str, blocker_id: str, verdict: "PauseVerdict") -> None:
        from ephemeral_task import PauseVerdict  # noqa: F811 — type narrowing
        tc = self._team_run.task_center
        checkpoint = json.dumps(verdict.conversation)
        await tc.pause_running_task(task_id, blocker_id, checkpoint, verdict.reason)

    async def _spawn_resolver(self, blocker: Blocker) -> None:
        from agents.registry import find_by_role
        developers = find_by_role("developer")
        agent_name = developers[0].name if developers else "developer"

        resolver_id = str(uuid.uuid4())
        spec = TaskSpec(
            id=resolver_id,
            task=f"RESOLVER: Fix broken files {blocker.root_cause_paths}. Problem: {blocker.reason}",
            agent=agent_name,
            deps=[],
            scope_paths=blocker.root_cause_paths,
        )
        tc = self._team_run.task_center
        await tc.insert_plan([spec])
        blocker.fix_task_id = resolver_id
        logger.info("Spawned resolver task %s for blocker %s", resolver_id, blocker.id)

    # ------------------------------------------------------------------
    # Fix outcome handlers
    # ------------------------------------------------------------------

    async def on_fix_complete(self, blocker_id: str, fix_summary: str) -> None:
        blocker = self._active_blockers.get(blocker_id)
        if blocker is None:
            logger.warning("on_fix_complete: unknown blocker %s", blocker_id)
            return

        blocker.status = BlockerStatus.RESOLVED
        blocker.fix_summary = fix_summary
        blocker.resolved_at = time.time()
        await self._persist(blocker)

        resumed = await self._resume_paused(blocker)
        logger.info("Blocker %s resolved; resumed %d paused tasks", blocker_id, resumed)

        # Spawn a replanner for the initiating task
        try:
            from agents.registry import find_by_role
            replanners = find_by_role("replanner")
            replanner_agent = replanners[0].name if replanners else "replanner"
            replanner_id = str(uuid.uuid4())
            spec = TaskSpec(
                id=replanner_id,
                task=f"Replan after blocker resolved: {fix_summary}",
                agent=replanner_agent,
                deps=[],
                scope_paths=[],
            )
            tc = self._team_run.task_center
            await tc.insert_plan([spec])
        except Exception as exc:
            logger.warning("Failed to spawn replanner after fix: %s", exc)

        del self._active_blockers[blocker_id]

    async def on_fix_failed(self, blocker_id: str, reason: str) -> None:
        blocker = self._active_blockers.get(blocker_id)
        if blocker is None:
            logger.warning("on_fix_failed: unknown blocker %s", blocker_id)
            return

        cancelled = await self._cancel_paused(blocker)
        blocker.status = BlockerStatus.FAILED
        await self._persist(blocker)
        logger.critical(
            "Blocker %s FAILED (%s); cancelled %d paused tasks", blocker_id, reason, cancelled
        )

        del self._active_blockers[blocker_id]

    # ------------------------------------------------------------------
    # Bulk task helpers
    # ------------------------------------------------------------------

    async def _resume_paused(self, blocker: Blocker) -> int:
        return await self._team_run.task_center.resume_paused_tasks(blocker.id)

    async def _cancel_paused(self, blocker: Blocker) -> int:
        return await self._team_run.task_center.cancel_paused_tasks(blocker.id)

    # ------------------------------------------------------------------
    # Path helpers
    # ------------------------------------------------------------------

    @staticmethod
    def _paths_overlap(paths_a: list[str], paths_b: list[str]) -> bool:
        """True if any path in paths_a is a prefix of or prefixed by any path in paths_b."""
        for a in paths_a:
            for b in paths_b:
                if a == b or a.startswith(b.rstrip("/") + "/") or b.startswith(a.rstrip("/") + "/"):
                    return True
        return False
