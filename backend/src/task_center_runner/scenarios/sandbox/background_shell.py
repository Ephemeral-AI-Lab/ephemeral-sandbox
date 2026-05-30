"""Background-shell scenarios that drive ``shell(background=True)`` through the
engine-owned background task harness.

Seven scenarios (T1-T3, T5-T8 from the Phase 2 plan; T4 is covered by the
invocation-keyed daemon in-flight TTL tests):

- ``sandbox.background_shell_golden`` (T1)
- ``sandbox.background_shell_stop`` (T2)
- ``sandbox.background_shell_interleave`` (T3)
- ``sandbox.background_shell_exhaustion`` (T5)
- ``sandbox.background_shell_partial_write_cancel`` (T6)
- ``sandbox.background_shell_stop_during_maintenance`` (T7)
- ``sandbox.background_shell_late_cancel_race`` (T8)

Each scenario uses a single executor action that drives the matching
probe in :mod:`task_center_runner.agent.mock.background_shell_probe`.
The probes call the shell tool with ``background_task_id`` set so the
tool framework keeps the request correlated with the engine background task;
the harness records full ``sandbox_events.jsonl`` plus
``performance_report.json`` artifacts.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from tools.submission.evaluator import submit_evaluation_success
from tools.submission.planner import submit_plan_closes_goal

from task_center_runner.audit.events import EventType
from task_center_runner.scenarios.base import (
    ScenarioBase,
    ScenarioContext,
    ToolCallSpec,
)


def _plan(action_id: str, action_spec: str, summary_hint: str) -> dict[str, Any]:
    return {
        "plan_spec": (
            f"Single-task plan that drives the {action_id} background-shell "
            "probe through the mock-agent harness."
        ),
        "evaluation_criteria": [
            f"Background-shell probe '{action_id}' wrote its summary to {summary_hint}.",
            "Daemon request cancellation and engine background status "
            "produced consistent results for every launch.",
        ],
        "tasks": [
            {"id": action_id, "agent_name": "executor", "deps": []},
        ],
        "task_specs": {action_id: action_spec},
    }


class _BackgroundShellScenarioBase(ScenarioBase):
    """Shared planner/executor/evaluator shape across the 7 scenarios."""

    expected_event_sequence: tuple[EventType, ...] = (
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_COMPLETES_GOAL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_INVOKED,
        EventType.EVALUATOR_SUCCESS,
    )

    action_id: str = ""
    action_spec: str = ""
    summary_path_hint: str = ""

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:  # noqa: ARG002
        return ToolCallSpec(
            submit_plan_closes_goal,
            _plan(self.action_id, self.action_spec, self.summary_path_hint),
        )

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[Any]:
        context_message = ctx.context_message or ctx.prompt or ""
        if f"ACTION {self.action_id}" in context_message:
            return (self.action_id,)
        return ()

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_evaluation_success,
            {
                "summary": (f"{self.action_id} background-shell scenario completed."),
                "passed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )


def _scenario(
    class_name: str,
    *,
    action_id: str,
    action_spec: str,
    summary_path_hint: str,
) -> type[_BackgroundShellScenarioBase]:
    """Build a data-only background-shell scenario leaf.

    ``name`` is derived as ``f"sandbox.{action_id}"`` — the invariant every
    former hand-written leaf class satisfied.
    """
    return type(
        class_name,
        (_BackgroundShellScenarioBase,),
        {
            "name": f"sandbox.{action_id}",
            "action_id": action_id,
            "action_spec": action_spec,
            "summary_path_hint": summary_path_hint,
        },
    )


# T1: N concurrent background launches reach ``finished`` cleanly.
BackgroundShellGolden = _scenario(
    "BackgroundShellGolden",
    action_id="background_shell_golden",
    action_spec=(
        "ACTION background_shell_golden. Launch 3 concurrent background shells "
        "(each sleeps 5 s, echoes 'done'); wait for natural exit; write the "
        "per-launch summary."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/golden/summary.json",
)
# T2: launch background shells; cancel mid-flight; no leftover state.
BackgroundShellStop = _scenario(
    "BackgroundShellStop",
    action_id="background_shell_stop",
    action_spec=(
        "ACTION background_shell_stop. Launch 3 long-running background shells, "
        "cancel each via asyncio.wait_for after 1 s, then issue a follow-up "
        "foreground shell to confirm post-cancel mount latency stays under "
        "budget."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/stop/summary.json",
)
# T3: 1 long background + M foreground shells, record fg p95 mount.
BackgroundShellInterleave = _scenario(
    "BackgroundShellInterleave",
    action_id="background_shell_interleave",
    action_spec=(
        "ACTION background_shell_interleave. Launch 1 long-running background "
        "shell (sleep 30 s) and 5 foreground shells interleaved; record "
        "per-foreground mount-latency timings."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/interleave/summary.json",
)
# T5: 80 launches cancelled in unison; AC-14 post-exhaustion read budget.
BackgroundShellExhaustion = _scenario(
    "BackgroundShellExhaustion",
    action_id="background_shell_exhaustion",
    action_spec=(
        "ACTION background_shell_exhaustion. Fire 80 background shell launches in "
        "parallel, each cancelled after 2 s; issue a follow-up foreground "
        "read_file to validate the daemon RPC dispatcher is not blocked by the "
        "shell executor (Pre-mortem #3 / AC-14)."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/exhaustion/summary.json",
)
# T6: cancel a long ``dd`` mid-write; assert no leaked OCC publish.
BackgroundShellPartialWriteCancel = _scenario(
    "BackgroundShellPartialWriteCancel",
    action_id="background_shell_partial_write_cancel",
    action_spec=(
        "ACTION background_shell_partial_write_cancel. Run an 800 MB dd into a "
        "tracked path as a background shell, cancel at 2 s, then read the target "
        "back to confirm the upperdir was discarded."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/partial_write/summary.json",
)
# T7: short shell + maintenance; verify OCC consistency afterwards.
BackgroundShellStopDuringMaintenance = _scenario(
    "BackgroundShellStopDuringMaintenance",
    action_id="background_shell_stop_during_maintenance",
    action_spec=(
        "ACTION background_shell_stop_during_maintenance. Run a short background "
        "shell that writes one file and then sleeps; confirm the publish + "
        "maintenance sequence leaves a consistent OCC state."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/maintenance/summary.json",
)
# T8: await full completion; late cancel must not mutate the result.
BackgroundShellLateCancelRace = _scenario(
    "BackgroundShellLateCancelRace",
    action_id="background_shell_late_cancel_race",
    action_spec=(
        "ACTION background_shell_late_cancel_race. Await a short background shell "
        "to completion (1 s sleep + echo); assert exit_code 0 and stdout "
        "preserved."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/late_cancel/summary.json",
)
# 3.3.1: direct foreground write races a background shell publish.
BackgroundMixedFgBgSamePathConflict = _scenario(
    "BackgroundMixedFgBgSamePathConflict",
    action_id="background_mixed_fg_bg_same_path_conflict",
    action_spec=(
        "ACTION background_mixed_fg_bg_same_path_conflict. Launch a background "
        "shell that writes /testbed/bg-shared.txt after a short sleep, run a "
        "foreground write_file to the same path while it sleeps, then record the "
        "OCC winner and conflict metadata."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/mixed_fg_bg_same_path_conflict/summary.json",
)
# 3.3.2: heartbeat one background invocation while another goes stale.
BackgroundHeartbeatLossReapsOnlyStaleBg = _scenario(
    "BackgroundHeartbeatLossReapsOnlyStaleBg",
    action_id="background_heartbeat_loss_reaps_only_stale_bg",
    action_spec=(
        "ACTION background_heartbeat_loss_reaps_only_stale_bg. Launch two "
        "background shell invocations with explicit invocation ids, heartbeat "
        "only the protected invocation, let the stale invocation hit the daemon "
        "TTL reaper, and run a foreground shell during recovery."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/heartbeat_loss/summary.json",
)
# 3.3.3: iws enter blocks default bg and iws exit drains per-agent bg.
BackgroundExitIwsDrainsAgentTasks = _scenario(
    "BackgroundExitIwsDrainsAgentTasks",
    action_id="background_exit_iws_drains_agent_tasks",
    action_spec=(
        "ACTION background_exit_iws_drains_agent_tasks. Prove "
        "enter_isolated_workspace rejects while this agent has default background "
        "shell work in flight, then open an isolated workspace for another agent "
        "and exit while its background shell is running."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/exit_iws_drain/summary.json",
)
# 3.3.4: abandoned background work is reaped before foreground recovery.
BackgroundEngineRestartNoLeaseLeak = _scenario(
    "BackgroundEngineRestartNoLeaseLeak",
    action_id="background_engine_restart_no_lease_leak",
    action_spec=(
        "ACTION background_engine_restart_no_lease_leak. Launch a chunked "
        "background shell without heartbeats, wait for daemon stale-invocation "
        "cleanup, then run a normal foreground shell plus write/read cycle."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/engine_restart/summary.json",
)
# 3.3.5: many small background writes with interleaved foreground files.
BackgroundManySmallWritesDoNotStarveDispatcher = _scenario(
    "BackgroundManySmallWritesDoNotStarveDispatcher",
    action_id="background_many_small_writes_do_not_starve_dispatcher",
    action_spec=(
        "ACTION background_many_small_writes_do_not_starve_dispatcher. Launch "
        "many small background shell writes and interleave foreground read_file "
        "and write_file calls, recording dispatcher responsiveness and final "
        "daemon in-flight count."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/many_small_writes/summary.json",
)
# 3.3.6: heterogeneous + conflicting + disjoint concurrent background work.
BackgroundMixedOpConcurrent = _scenario(
    "BackgroundMixedOpConcurrent",
    action_id="background_mixed_op_concurrent",
    action_spec=(
        "ACTION background_mixed_op_concurrent. Launch a pytest run, a pip "
        "install, and a python edit-loop as concurrent background tasks and "
        "confirm each reaches a terminal status; race N background shells "
        "overwriting one seeded path (exactly one OCC winner, the rest abort); "
        "and write N disjoint paths concurrently (all land)."
    ),
    summary_path_hint="/testbed/.ephemeralos/sweevo-mock/background_shell/mixed_op_concurrent/summary.json",
)

__all__ = [
    "BackgroundEngineRestartNoLeaseLeak",
    "BackgroundExitIwsDrainsAgentTasks",
    "BackgroundHeartbeatLossReapsOnlyStaleBg",
    "BackgroundManySmallWritesDoNotStarveDispatcher",
    "BackgroundMixedFgBgSamePathConflict",
    "BackgroundMixedOpConcurrent",
    "BackgroundShellStop",
    "BackgroundShellStopDuringMaintenance",
    "BackgroundShellExhaustion",
    "BackgroundShellGolden",
    "BackgroundShellInterleave",
    "BackgroundShellLateCancelRace",
    "BackgroundShellPartialWriteCancel",
]
