"""Single launch builder for every harness agent role.

The four ``_build_*_launch`` helpers that used to live on
:class:`AttemptOrchestrator`, :class:`AttemptDispatcher`, and
:class:`TaskCenterEntryCoordinator` are unified here. Every harness launch
flows through the composer with role-specific :class:`ContextScope` fields
populated, then bundles the :class:`AgentLaunch` for the launcher to consume.

Adding a per-launch knob (priority, retry policy, latency budget) becomes
one edit on :class:`AgentLaunch` plus one edit here — instead of four
identical edits at four sites.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from task_center.attempt.runtime import AgentLaunch, AttemptDeps
from task_center.context_engine.scope import ContextScope
from task_center.exceptions import TaskCenterInvariantViolation
from task_center.task.state import TaskCenterTaskRole

if TYPE_CHECKING:
    from task_center.attempt.state import Attempt


PLANNER_AGENT_NAME = "planner"
EVALUATOR_AGENT_NAME = "evaluator"


@dataclass(frozen=True, slots=True)
class LaunchBuilder:
    """Build :class:`AgentLaunch` records for each harness role."""

    runtime: AttemptDeps

    def for_planner(
        self, *, attempt: Attempt, task_id: str
    ) -> AgentLaunch:
        episode = self._require_episode(attempt)
        bundle = self.runtime.require_composer().compose(
            base_agent_name=PLANNER_AGENT_NAME,
            scope=ContextScope(
                mission_id=episode.mission_id,
                episode_id=episode.id,
                attempt_id=attempt.id,
            ),
        )
        return AgentLaunch(
            task_id=task_id,
            task_center_run_id=self.runtime.run_id_for_attempt(attempt),
            attempt_id=attempt.id,
            role=TaskCenterTaskRole.PLANNER,
            agent_name=bundle.agent_def.name,
            rendered_prompt=bundle.rendered_prompt,
            needs=(),
            context_packet_id=bundle.context_packet_id,
            mission_id=episode.mission_id,
        )

    def for_generator(
        self,
        *,
        attempt: Attempt,
        task: dict[str, Any],
        base_agent_name: str,
    ) -> AgentLaunch:
        episode = self._require_episode(attempt)
        task_id = str(task["id"])
        bundle = self.runtime.require_composer().compose(
            base_agent_name=base_agent_name,
            scope=ContextScope(
                mission_id=episode.mission_id,
                episode_id=episode.id,
                attempt_id=attempt.id,
                task_id=task_id,
            ),
        )
        return AgentLaunch(
            task_id=task_id,
            task_center_run_id=task["task_center_run_id"],
            attempt_id=attempt.id,
            role=TaskCenterTaskRole.GENERATOR,
            agent_name=bundle.agent_def.name,
            rendered_prompt=bundle.rendered_prompt,
            needs=tuple(task["needs"]),
            context_packet_id=bundle.context_packet_id,
            mission_id=episode.mission_id,
        )

    def for_evaluator(
        self, *, attempt: Attempt, task_id: str
    ) -> AgentLaunch:
        episode = self._require_episode(attempt)
        bundle = self.runtime.require_composer().compose(
            base_agent_name=EVALUATOR_AGENT_NAME,
            scope=ContextScope(
                mission_id=episode.mission_id,
                episode_id=episode.id,
                attempt_id=attempt.id,
            ),
        )
        return AgentLaunch(
            task_id=task_id,
            task_center_run_id=self.runtime.run_id_for_attempt(attempt),
            attempt_id=attempt.id,
            role=TaskCenterTaskRole.EVALUATOR,
            agent_name=bundle.agent_def.name,
            rendered_prompt=bundle.rendered_prompt,
            needs=tuple(attempt.generator_task_ids),
            context_packet_id=bundle.context_packet_id,
            mission_id=episode.mission_id,
        )

    def for_entry(
        self,
        *,
        task_id: str,
        task_center_run_id: str,
        base_agent_name: str,
    ) -> AgentLaunch:
        bundle = self.runtime.require_composer().compose(
            base_agent_name=base_agent_name,
            scope=ContextScope(task_id=task_id),
        )
        return AgentLaunch(
            task_id=task_id,
            task_center_run_id=task_center_run_id,
            attempt_id=None,
            role=TaskCenterTaskRole.ENTRY_EXECUTOR,
            agent_name=bundle.agent_def.name,
            rendered_prompt=bundle.rendered_prompt,
            needs=(),
            context_packet_id=bundle.context_packet_id,
            mission_id=None,
        )

    # ---- internal ---------------------------------------------------------

    def _require_episode(self, attempt: Attempt) -> Any:
        episode = self.runtime.episode_store.get(attempt.episode_id)
        if episode is None:
            raise TaskCenterInvariantViolation(
                f"Episode {attempt.episode_id!r} not found"
            )
        return episode


__all__ = ["LaunchBuilder", "PLANNER_AGENT_NAME", "EVALUATOR_AGENT_NAME"]
