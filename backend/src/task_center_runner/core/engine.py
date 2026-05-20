"""``run_pipeline`` — the unified entrypoint for mock / real / benchmark runs.

Phase 4c of the task_center_runner restructure
(.omc/plans/task_center_runner-restructure.md §4). All three run modes
funnel through this one coroutine; the only mode-specific seams are the
five fields documented in the plan's §4 mode-delta table:

- ``config.runner_factory`` — mock returns a ``MockSquadRunner``; real-LLM
  and benchmark return ``None`` so ``start_task_center_entry_run`` falls
  back to its real-agent runner.
- ``config.bootstrap`` — only real-agent paths set this (it seeds the
  agent registry / runtime stores).
- ``config.lifecycle`` — ``ScenarioLifecycle`` / ``SweevoLifecycle`` /
  ``NoopLifecycle``; subscribed to the bus exactly once at startup.
- ``config.sandbox`` — ``AttachExisting`` for tests that pre-provision a
  sandbox via a fixture; the benchmark adapter's
  ``provisioner_for(instance)`` otherwise.
- ``config.run_label`` — path segment under ``audit_dir`` (e.g.
  ``scenario_logs/<name>``, ``user_run``,
  ``benchmark/sweevo/<instance_id>``).

This module knows nothing about ``MockSquadRunner``, ``MutableMockState``,
Daytona, or any ``benchmarks.sweevo.*`` symbol — that runner-agnostic
property is enforced by ``test_no_core_imports.py`` (added with Phase 4d).

The implementation is import-safe and unit-testable, but no production
caller goes through it yet — Phases 4e and 4f slim ``run_scenario`` /
``run_sweevo_real_agent`` to shims over ``run_pipeline``.
"""

from __future__ import annotations

import asyncio
import time
import uuid
from datetime import UTC, datetime
from pathlib import Path
from types import SimpleNamespace

from task_center import TaskCenterSandboxBridge, start_task_center_entry_run

from config.model_config import try_get_active_model_kwargs
from task_center_runner.audit.bus import AuditEventBus
from task_center_runner.audit.events import Event, EventType
from task_center_runner.audit.node_id import NodeId
from task_center_runner.audit.performance_report import _write_perf_report_safe
from task_center_runner.audit.recorder import AuditRecorder
from task_center_runner.audit.stream_bridge import stream_bridge
from task_center_runner.core.config import RunConfig, RunContext
from task_center_runner.core.report import PipelineReport
from task_center_runner.core.stores import create_per_test_task_center_stores


def _default_run_dir(audit_dir: Path, ctx: RunContext) -> Path:
    """Canonical run-dir scheme: ``audit_dir/<run_label>/<utc>_<self_id>``.

    Per the plan's locked decision #8 the same scheme applies to all modes
    so ``run_tiered.py`` (and any other resume tooling) globs a single
    layout.
    """
    utc_stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    self_run_id = uuid.uuid4().hex[:12]
    return Path(audit_dir) / ctx.config.run_label / f"{utc_stamp}_{self_run_id}"


def _default_bridge() -> TaskCenterSandboxBridge:
    """Permissive bridge — accepts whatever sandbox id the caller supplied."""
    return TaskCenterSandboxBridge(start_fn=lambda existing_id: {"id": existing_id})


def _count_task_outcomes(task_rows: list[dict]) -> tuple[int, int, int]:
    total = len(task_rows)
    completed = sum(1 for row in task_rows if row.get("status") == "done")
    failed = sum(1 for row in task_rows if row.get("status") == "failed")
    return total, completed, failed


async def run_pipeline(config: RunConfig) -> PipelineReport:
    """Drive a single TaskCenter run end-to-end.

    Steps:
      1. ``config.bootstrap()`` if present (real-LLM only).
      2. Open the bus, subscribe ``config.lifecycle.on_event``.
      3. ``config.lifecycle.before_run(ctx)``.
      4. Provision the sandbox via ``config.sandbox``.
      5. Resolve ``run_dir``; start the ``AuditRecorder``.
      6. Build the runner via ``config.runner_factory(ctx)``; ``None``
         signals the real-agent path.
      7. ``start_task_center_entry_run(...)`` + ``wait_for_idle``
         (optionally bounded by ``config.max_duration_s``).
      8. Capture the perf-report snapshot pre-dispose; release the
         sandbox + dispose the recorder + close owned stores in
         ``finally``.
      9. Spawn ``_write_perf_report_safe`` as an asyncio task; attach
         the handle to the report.
     10. ``config.lifecycle.after_run(ctx, report)`` — may mutate
         ``report.lifecycle_extras``.
    """
    if config.bootstrap is not None:
        config.bootstrap()

    bundle = config.stores or create_per_test_task_center_stores()
    owns_stores = config.stores is None

    bus = AuditEventBus()
    lifecycle_unsub = bus.subscribe(config.lifecycle.on_event)

    ctx = RunContext(config=config, bundle=bundle, bus=bus)
    await config.lifecycle.before_run(ctx)

    lease = await config.sandbox.provision(ctx)
    run_dir_factory = config.run_dir_factory or _default_run_dir
    run_dir = run_dir_factory(config.audit_dir, ctx)

    scenario_name = config.extras.get("scenario_name")
    if not isinstance(scenario_name, str) or not scenario_name:
        scenario_name = config.run_label

    class_path = (try_get_active_model_kwargs() or {}).get("class_path", "") or ""
    coding_plan_mode_active = class_path.startswith("providers.clients.coding_plan.")

    recorder = AuditRecorder(
        run_dir,
        task_center_run_id="",
        bus=bus,
        scenario_name=scenario_name,
        instance_id=config.instance_id,
        sandbox_id=lease.sandbox_id,
        coding_plan_mode_active=coding_plan_mode_active,
    )
    recorder.start()

    runner = config.runner_factory(ctx)
    bind_audit_recorder = getattr(runner, "bind_audit_recorder", None)
    if callable(bind_audit_recorder):
        bind_audit_recorder(recorder)
    bridge_factory = config.bridge_factory or _default_bridge
    bridge = bridge_factory()
    bridge_run_id = ""

    async def _on_agent_event(event) -> None:  # type: ignore[no-untyped-def]
        bridge_cb = stream_bridge(bus, task_center_run_id=bridge_run_id)
        await bridge_cb(event)
        agent_run_id = str(getattr(event, "run_id", "") or "")
        if not agent_run_id:
            return
        per_task = recorder.message_recorder_for_agent_run(agent_run_id)
        if per_task is None:
            per_task = recorder.message_recorder_for_task(agent_run_id)
        if per_task is not None:
            per_task.emit(event)

    started = time.perf_counter()
    aborted_by_timeout = False
    handle = None
    try:
        # ``start_task_center_entry_run`` reads only ``config.cwd`` off this
        # object; real-LLM callers may pre-build a full
        # ``runtime.app_factory.RuntimeConfig`` and pass it via
        # ``config.extras["runtime_config"]`` if extra attributes are needed.
        runtime_cfg = config.extras.get(
            "runtime_config", SimpleNamespace(cwd=config.repo_dir)
        )
        handle = start_task_center_entry_run(
            config=runtime_cfg,
            prompt=config.entry_prompt,
            sandbox_id=lease.sandbox_id,
            on_agent_event=_on_agent_event,
            task_store=bundle.task_store,
            goal_store=bundle.goal_store,
            iteration_store=bundle.iteration_store,
            attempt_store=bundle.attempt_store,
            context_packet_store=bundle.context_packet_store,
            runner=runner,
            sandbox_bridge=bridge,
        )
        tcrid = str(handle.task_center_run_id)
        bridge_run_id = tcrid
        recorder.bind_task_center_run_id(tcrid)
        bus.publish(Event(type=EventType.RUN_STARTED, node=NodeId(task_center_run_id=tcrid)))

        try:
            if config.max_duration_s is not None:
                await asyncio.wait_for(
                    handle.launcher.wait_for_idle(), timeout=config.max_duration_s
                )
            else:
                await handle.launcher.wait_for_idle()
        except asyncio.TimeoutError:
            aborted_by_timeout = True
            pending = tuple(handle.launcher._pending)  # noqa: SLF001 — see launcher contract
            for task in pending:
                task.cancel()
            await asyncio.gather(*pending, return_exceptions=True)
            await config.lifecycle.on_aborted(ctx, "timeout")

        bus.publish(Event(type=EventType.RUN_COMPLETED, node=NodeId(task_center_run_id=tcrid)))

        run_row = bundle.task_store.get_run(tcrid) or {}
        task_rows = bundle.task_store.list_tasks_for_run(tcrid)
        task_count, tasks_completed, tasks_failed = _count_task_outcomes(task_rows)
        metrics = recorder.metrics.snapshot()
        perf_snapshot = recorder.metrics.performance_snapshot()
        duration_s = time.perf_counter() - started
    finally:
        await config.sandbox.release(lease)
        recorder.dispose()
        if owns_stores:
            bundle.close()
        lifecycle_unsub()

    perf_task = asyncio.create_task(
        _write_perf_report_safe(run_dir, perf_snapshot),
        name=f"perf_report:{tcrid}",
    )

    report = PipelineReport(
        status="aborted" if aborted_by_timeout else "completed",
        task_center_run_id=tcrid,
        request_id=str(handle.request_id) if handle is not None else "",
        sandbox_id=lease.sandbox_id,
        instance_id=config.instance_id,
        run_dir=run_dir,
        task_center_status=run_row.get("status"),
        duration_s=duration_s,
        task_count=task_count,
        tasks_completed=tasks_completed,
        tasks_failed=tasks_failed,
        metrics=metrics,
        aborted_by_timeout=aborted_by_timeout,
        performance_report_task=perf_task,
    )
    await config.lifecycle.after_run(ctx, report)
    return report


__all__ = ["run_pipeline"]
