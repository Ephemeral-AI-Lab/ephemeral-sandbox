"""``build_scenario_config`` — assembles the ``RunConfig`` for a mock scenario.

Single point of truth where the ``MockSquadRunner`` factory, the
``ScenarioLifecycle`` (and thus ``HookSet``), and the shared
``MutableMockState`` are wired together so they share state. Outside this
builder no other module imports ``MutableMockState`` — the engine remains
runner-agnostic.
"""

from __future__ import annotations

import os
from collections.abc import Sequence
from pathlib import Path
from typing import TYPE_CHECKING

from task_center_runner.core.config import RunConfig
from task_center_runner.core.sandbox import AttachExisting
from task_center_runner.hooks.registry import Hook, HookSet, MutableMockState
from task_center_runner.scenarios.base import Scenario
from task_center_runner.scenarios.lifecycle import ScenarioLifecycle

if TYPE_CHECKING:
    from task_center_runner.core.config import RunContext


# Migration seam: mock scenarios run through the real query loop via
# ``ScenarioLoopRunner`` + an injected ``ScenarioEventSource`` unless explicitly
# disabled. ``MockSquadRunner`` remains available as a fallback while the last
# scenario migrations land.
_EVENT_SOURCE_RUNNER_ENV = "EOS_MOCK_EVENT_SOURCE_RUNNER"
_LEGACY_RUNNER_REQUIRED_SCENARIOS: frozenset[str] = frozenset(
    {
        # Item 3 in docs/plans/mock_event_source_HANDOFF_2026-05-30.md: these
        # still need fan-out promotions before the event-source runner can own
        # their concurrency shape.
        "sandbox.auto_squash_commit_resume",
        "sandbox.complex_project_build",
        "sandbox.complex_project_build_grep_glob",
        "sandbox.complex_project_build_grep_glob_smoke",
        "sandbox.complex_project_build_shell_edit_lsp",
        "sandbox.complex_project_build_shell_edit_lsp_smoke",
        "sandbox.complex_project_build_smoke",
        "sandbox.ephemeral_workspace_same_path_conflict",
        # Item 4: background probes still use the old blocking
        # background_task_id call contract.
        "sandbox.background_engine_restart_no_lease_leak",
        "sandbox.background_exit_iws_drains_agent_tasks",
        "sandbox.background_heartbeat_loss_reaps_only_stale_bg",
        "sandbox.background_many_small_writes_do_not_starve_dispatcher",
        "sandbox.background_mixed_fg_bg_same_path_conflict",
        "sandbox.background_mixed_op_concurrent",
        "sandbox.background_shell_exhaustion",
        "sandbox.background_shell_golden",
        "sandbox.background_shell_interleave",
        "sandbox.background_shell_late_cancel_race",
        "sandbox.background_shell_partial_write_cancel",
        "sandbox.background_shell_stop",
        "sandbox.background_shell_stop_during_maintenance",
        "sandbox.ephemeral_workspace_cancellation",
    }
)


def _event_source_runner_enabled(scenario_name: str) -> bool:
    raw = os.environ.get(_EVENT_SOURCE_RUNNER_ENV)
    if raw is not None:
        return raw.strip().lower() not in {"false", "0", "no", "off"}
    return scenario_name not in _LEGACY_RUNNER_REQUIRED_SCENARIOS


def build_scenario_config(
    scenario: Scenario,
    *,
    sandbox_id: str,
    audit_dir: Path,
    repo_dir: str,
    entry_prompt: str,
    extra_hooks: Sequence[Hook] = (),
    instance_id: str = "",
) -> tuple[RunConfig, MutableMockState, ScenarioLifecycle]:
    """Construct the mock-mode ``RunConfig`` plus the shared mutable state.

    Returns the config alongside the ``MutableMockState`` and
    ``ScenarioLifecycle`` so callers (the ``run_scenario`` shim) can read
    their state after the run for the legacy ``RunReport`` assembly.
    """
    mutable_state = MutableMockState()
    hook_set = HookSet()
    for hook in scenario.hooks():
        hook_set.register(hook)
    for hook in extra_hooks:
        hook_set.register(hook)
    lifecycle = ScenarioLifecycle(
        scenario=scenario, hook_set=hook_set, mutable_state=mutable_state
    )

    def _make_runner(ctx: "RunContext"):
        # Imported lazily to keep scenario import-time setup free of runner state.
        if _event_source_runner_enabled(scenario.name):
            from task_center_runner.agent.mock.scenario_loop_runner import (
                ScenarioLoopRunner,
            )

            return ScenarioLoopRunner(
                repo_dir=repo_dir,
                bus=ctx.bus,
                scenario=scenario,
                mutable_state=mutable_state,
            )

        from task_center_runner.agent.mock.runner import MockSquadRunner

        return MockSquadRunner(
            repo_dir=repo_dir,
            bus=ctx.bus,
            task_center_run_id="",
            scenario=scenario,
            mutable_state=mutable_state,
            audit_recorder=None,
        )

    # A real ``RuntimeConfig`` is threaded as ``runtime_config`` so the launcher
    # passes it (not a bare ``SimpleNamespace``) to the runner: the event-source
    # path needs ``resolve_settings``/``external_api_client``/
    # ``event_source_factory`` to reach ``run_ephemeral_agent`` → ``spawn_agent``.
    # Harmless to ``MockSquadRunner`` (it only reads ``.cwd``).
    from task_center_runner.agent.mock.scenario_loop_runner import (
        make_mock_runtime_config,
    )

    config = RunConfig(
        entry_prompt=entry_prompt,
        repo_dir=repo_dir,
        sandbox=AttachExisting(sandbox_id),
        runner_factory=_make_runner,
        lifecycle=lifecycle,
        bootstrap=None,
        audit_dir=audit_dir,
        run_label=f"scenario_logs/{scenario.name}",
        instance_id=instance_id,
        extras={
            "scenario_name": scenario.name,
            "runtime_config": make_mock_runtime_config(repo_dir),
        },
    )
    return config, mutable_state, lifecycle


__all__ = ["build_scenario_config"]
