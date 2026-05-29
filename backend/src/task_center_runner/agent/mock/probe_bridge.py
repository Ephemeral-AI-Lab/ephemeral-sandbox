"""Queue-bridge: run an imperative ``call_tool``-based probe through the REAL loop.

The heavy probe modules (``high_concurrency_probe``, ``heavy_io_zoned_probe``,
``complex_project_build_probe``, …) were written for the old ``MockSquadRunner``:
they accept an injected ``call_tool(tool_obj, raw_input, metadata, emit, *,
allow_error=...)`` and call it many times deep in their bodies. To run those
bodies through the real ``query.py`` loop WITHOUT rewriting them as async
generators, this module injects a *bridging* ``call_tool`` that hands each call
to the driving role ``TurnScript`` — one :class:`Turn` per call — so the
``ScenarioEventSource`` emits it as a scripted ``tool_use`` and the **real loop
dispatches it**. The bridge changes nothing about how tools execute; it only
adapts an imperative body into the scripted event stream. Mock vs. real still
differ *only* in the event source.

This is the "two-level coroutine bridge": the probe runs as a concurrent task;
:func:`bridge_turns` pulls each tool request off a queue and ``yield``s a
``Turn`` at the top level of the role ``TurnScript`` (Python forbids hiding an
async-generator yield inside a helper), resolving the probe's awaited future
with the loop-normalized :class:`~tools.ToolResult`.

Budget: a single agent is capped at its ``tool_call_limit`` (executor=75, hard
ceiling 1.5×). Heavy probes exceed that, so the scenario planner fans the work
out into a generator DAG (see [[mock_event_source_heavy_probe_fanout_decision]]);
each generator's tool stream is budget-sized and routes through the loop here.
Background dispatch (``background_task_id``) is fire-and-forget through the loop
and cannot satisfy the old probes' blocking-await contract, so the bridge
rejects it — those probes are rewritten to the real-agent background model.
"""

from __future__ import annotations

import asyncio
import contextlib
from collections.abc import AsyncGenerator, Awaitable, Callable
from typing import Any

from message.message import ToolResultBlock
from tools._framework.core.results import ToolResult

from task_center_runner.agent.mock.event_source import ToolCall, Turn

# A probe coroutine factory: given the bridging call_tool, returns the probe
# coroutine (which returns the artifact path string).
ProbeFactory = Callable[[Callable[..., Awaitable[ToolResult]]], Awaitable[str]]


async def _noop_emit(_event: Any) -> None:
    return None


class _CallToolBridge:
    """Provides the bridging ``call_tool`` + a request queue the driver drains."""

    __slots__ = ("_queue",)

    def __init__(self) -> None:
        # items: ("call", tool_name, raw_input, future) | ("done", None, None, None)
        self._queue: asyncio.Queue[tuple[str, str | None, dict | None, Any]] = (
            asyncio.Queue()
        )

    async def call_tool(
        self,
        tool_obj: Any,
        raw_input: dict[str, Any],
        metadata: Any = None,  # noqa: ARG002 — loop owns tool_metadata
        emit: Any = None,  # noqa: ARG002 — loop owns the event stream
        *,
        allow_error: bool = False,
        background_task_id: str | None = None,
        sandbox_invocation_id: str | None = None,  # noqa: ARG002
        **_kwargs: Any,
    ) -> ToolResult:
        if background_task_id is not None:
            raise NotImplementedError(
                "Background tool dispatch is not expressible through the query "
                "loop bridge (the loop's background path is fire-and-forget). "
                "Background probes must use the real-agent background model "
                "(shell(background=True) + wait_background_tasks / "
                "cancel_background_task). See the heavy-probe fan-out decision."
            )
        fut: asyncio.Future[ToolResult] = asyncio.get_running_loop().create_future()
        await self._queue.put(("call", tool_obj.name, dict(raw_input), fut))
        result = await fut
        # Mirror MockSquadRunner._call_tool: raise unless the caller opted in to
        # tolerate errors (probe bodies rely on this to fail fast).
        if result.is_error and not allow_error:
            raise RuntimeError(f"{tool_obj.name} failed: {result.output}")
        return result


async def bridge_turns(
    factory: ProbeFactory,
    *,
    artifact_out: list[str],
    normalize: Callable[[list[ToolResultBlock]], ToolResult],
) -> AsyncGenerator[Turn, list[ToolResultBlock]]:
    """Drive an imperative probe, yielding one :class:`Turn` per tool call.

    ``factory(call_tool)`` builds the probe coroutine. Each ``await call_tool``
    inside it surfaces here as ``yield Turn(calls=(ToolCall,))``; the value sent
    back (the loop's trailing ``ToolResultBlock``s) is normalized and used to
    resolve the probe's awaited future. The probe's return value (artifact path)
    is appended to *artifact_out*. Probe exceptions propagate to the caller.
    """
    bridge = _CallToolBridge()

    async def _run() -> None:
        try:
            artifact_out.append(await factory(bridge.call_tool))
        finally:
            await bridge._queue.put(("done", None, None, None))  # noqa: SLF001

    probe_task = asyncio.create_task(_run())
    try:
        while True:
            kind, name, raw_input, fut = await bridge._queue.get()  # noqa: SLF001
            if kind == "done":
                break
            blocks = yield Turn(calls=(ToolCall(str(name), dict(raw_input or {})),))
            if fut is not None and not fut.done():
                fut.set_result(normalize(blocks))
        # Re-raise any exception the probe body raised (e.g. a failed sandbox
        # check or a fail-fast tool error).
        await probe_task
    finally:
        if not probe_task.done():
            probe_task.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await probe_task


def bridge_script_for(
    action: str,
    *,
    ctx: Any,
) -> tuple[ProbeFactory, str] | None:
    """Map a ``PreparedToolScript`` executor action to ``(factory, summary)``.

    The full_stack / capacity / full_case script-engine actions build a
    deterministic :class:`PreparedToolScript` from the live
    :class:`ScenarioContext` (``ctx``) and run it through
    :class:`PreparedToolScriptEngine`. The engine calls its injected
    ``call_tool`` exactly as the queue-bridge expects (4 positional +
    ``allow_error`` only, never ``background_task_id``), so the bridge's
    ``call_tool`` is passed straight in with no shim. The factory returns the
    script's artifact path (the engine drives all its steps as scripted
    ``Turn``s through the loop, identical to a probe body). Returns ``None`` if
    *action* is not a script action (the caller then tries
    :func:`bridge_probe_for` or raises ``NotImplementedError``). Script modules
    are imported lazily to keep the import graph DAG-shaped.

    Budget note: ``inspect_user_input`` (111 steps) and
    ``layerstack_squash_lease`` (118 steps) exceed the executor design limit of
    100, but stay under the runtime hard ceiling of ``ceil(1.5 * 100) = 150``
    (the 100 limit only fires a reminder, which a scripted event source
    ignores). They are bridged here so their scenarios run end-to-end; Item 3
    fan-out splits them below 100.
    """

    def _engine_factory(
        build: Callable[[], Any], summary: str
    ) -> tuple[ProbeFactory, str]:
        async def _run_script(call_tool: Any) -> str:
            from task_center_runner.agent.mock.tool_scripts import (
                PreparedToolScriptEngine,
            )

            engine = PreparedToolScriptEngine(call_tool)
            result = await engine.run(
                build(), metadata=ctx.metadata, emit=_noop_emit
            )
            return result.artifact

        return _run_script, summary

    if action == "inspect_user_input":
        def _build() -> Any:
            from task_center_runner.agent.mock.tool_scripts import (
                inspect_user_input_script,
            )

            return inspect_user_input_script(ctx)

        return _engine_factory(
            _build, "Requirement ledger and package DAG were written and read back."
        )

    if action.startswith("execute_package:"):
        package_id = action.split(":", 1)[1]

        def _build_execute() -> Any:
            from task_center_runner.agent.mock.tool_scripts import (
                execute_package_script,
            )

            return execute_package_script(ctx, package_id=package_id)

        return _engine_factory(
            _build_execute, f"Executed package {package_id} with sandbox evidence."
        )

    if action == "final_reconciliation":
        def _build_final() -> Any:
            from task_center_runner.agent.mock.tool_scripts import (
                final_reconciliation_script,
            )

            return final_reconciliation_script(ctx)

        return _engine_factory(
            _build_final, "Final coverage ledger and readback probe passed."
        )

    if action == "recursive_step":
        def _build_recursive() -> Any:
            from task_center_runner.agent.mock.tool_scripts import (
                recursive_step_script,
            )

            return recursive_step_script(ctx)

        return _engine_factory(
            _build_recursive, "Recursive goal step completed with sandbox evidence."
        )

    # --- full_stack scripts -------------------------------------------------
    if action == "inspect_full_user_input":
        def _build_inspect_full() -> Any:
            from task_center_runner.agent.mock.full_stack_tool_scripts import (
                inspect_full_user_input_script,
            )

            return inspect_full_user_input_script(ctx)

        return _engine_factory(
            _build_inspect_full, "Full user-input inventory script passed."
        )

    if action == "occ_conflict_matrix":
        def _build_occ() -> Any:
            from task_center_runner.agent.mock.full_stack_tool_scripts import (
                occ_conflict_matrix_script,
            )

            return occ_conflict_matrix_script(ctx)

        return _engine_factory(_build_occ, "OCC conflict matrix script passed.")

    if action == "overlay_edge_matrix":
        def _build_overlay() -> Any:
            from task_center_runner.agent.mock.full_stack_tool_scripts import (
                overlay_edge_matrix_script,
            )

            return overlay_edge_matrix_script(ctx)

        return _engine_factory(_build_overlay, "Overlay edge matrix script passed.")

    if action == "layerstack_squash_lease":
        def _build_layerstack() -> Any:
            from task_center_runner.agent.mock.full_stack_tool_scripts import (
                layerstack_squash_lease_script,
            )

            return layerstack_squash_lease_script(ctx)

        return _engine_factory(
            _build_layerstack, "LayerStack squash-lease script passed."
        )

    if action == "lsp_refresh_semantics":
        def _build_lsp() -> Any:
            from task_center_runner.agent.mock.full_stack_tool_scripts import (
                lsp_refresh_semantics_script,
            )

            return lsp_refresh_semantics_script(ctx)

        return _engine_factory(_build_lsp, "LSP refresh-semantics script passed.")

    if action == "recursive_oversized_matrix":
        def _build_recursive_matrix() -> Any:
            from task_center_runner.agent.mock.full_stack_tool_scripts import (
                recursive_oversized_matrix_script,
            )

            return recursive_oversized_matrix_script(ctx)

        return _engine_factory(
            _build_recursive_matrix, "Recursive oversized matrix script passed."
        )

    if action == "full_stack_final_reconciliation":
        def _build_full_stack_final() -> Any:
            from task_center_runner.agent.mock.full_stack_tool_scripts import (
                final_reconciliation_script as full_stack_final_reconciliation_script,
            )

            return full_stack_final_reconciliation_script(ctx)

        return _engine_factory(
            _build_full_stack_final, "Full-stack final reconciliation script passed."
        )

    if action == "capacity_metrics_full_system":
        def _build_capacity() -> Any:
            from task_center_runner.agent.mock.capacity_actions import (
                full_system_capacity_metrics_script,
            )

            return full_system_capacity_metrics_script(ctx)

        return _engine_factory(
            _build_capacity, "Full-system capacity metrics script passed."
        )

    return None


def bridge_probe_for(
    action: str,
    *,
    probe_ctx: Any,
) -> tuple[ProbeFactory, str] | None:
    """Map a call_tool-based executor action to ``(probe_factory, summary)``.

    Returns ``None`` if *action* is not a bridge probe (the caller then tries
    the generator-style ``PROBE_BUILDERS`` or raises ``NotImplementedError``).
    Probe modules are imported lazily to keep the package import graph DAG-shaped.
    """
    metadata = probe_ctx.metadata

    if action == "high_concurrency_seed":
        def _seed(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.high_concurrency_probe import (
                run_high_concurrency_seed_probe,
            )

            return run_high_concurrency_seed_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _seed, "High-concurrency sandbox seed passed."

    if action.startswith("high_concurrency_worker:"):
        index = int(action.split(":", 1)[1])

        def _worker(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.high_concurrency_probe import (
                run_high_concurrency_worker_probe,
            )

            return run_high_concurrency_worker_probe(
                index=index,
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                publish=probe_ctx.publish,
                publish_mock_record=probe_ctx.publish_mock_record,
                record_tool_check=probe_ctx.record_check,
            )

        return _worker, f"High-concurrency worker {index:02d} passed."

    if action == "high_concurrency_reconcile":
        def _reconcile(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.high_concurrency_probe import (
                run_high_concurrency_reconcile_probe,
            )

            return run_high_concurrency_reconcile_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _reconcile, "High-concurrency sandbox reconciliation passed."

    if action == "heavy_io_zoned_seed":
        def _hiz_seed(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.heavy_io_zoned_probe import (
                run_heavy_io_zoned_seed_probe,
            )

            return run_heavy_io_zoned_seed_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _hiz_seed, "Heavy-IO zoned sandbox seed passed."

    if action.startswith("heavy_io_zoned_worker:"):
        index = int(action.split(":", 1)[1])

        def _hiz_worker(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.heavy_io_zoned_probe import (
                run_heavy_io_zoned_worker_probe,
            )

            return run_heavy_io_zoned_worker_probe(
                index=index,
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                publish=probe_ctx.publish,
                publish_mock_record=probe_ctx.publish_mock_record,
                record_tool_check=probe_ctx.record_check,
            )

        return _hiz_worker, f"Heavy-IO zoned worker {index:02d} passed."

    if action == "heavy_io_zoned_reconcile":
        def _hiz_reconcile(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.heavy_io_zoned_probe import (
                run_heavy_io_zoned_reconcile_probe,
            )

            return run_heavy_io_zoned_reconcile_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _hiz_reconcile, "Heavy-IO zoned sandbox reconciliation passed."

    # --- plugin_workspace (single-action scenarios; all queue-bridge) -------
    if action == "plugin_read_only_lsp_refresh":
        def _plugin_read_only(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.plugin_workspace_probe import (
                run_plugin_read_only_lsp_refresh_probe,
            )

            return run_plugin_read_only_lsp_refresh_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
                sandbox_id=metadata.sandbox_id,
            )

        return _plugin_read_only, "Plugin read-only LSP refresh probe passed."

    if action == "plugin_write_allowed_publish":
        def _plugin_write_allowed(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.plugin_workspace_probe import (
                run_plugin_write_allowed_publish_probe,
            )

            return run_plugin_write_allowed_publish_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
                sandbox_id=metadata.sandbox_id,
            )

        return _plugin_write_allowed, "Plugin write-allowed publish probe passed."

    if action == "plugin_intent_contract":
        def _plugin_intent(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.plugin_workspace_probe import (
                run_plugin_intent_contract_probe,
            )

            return run_plugin_intent_contract_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _plugin_intent, "Plugin intent-contract probe passed."

    if action == "plugin_iws_policy":
        def _plugin_iws(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.plugin_workspace_probe import (
                run_plugin_iws_policy_probe,
            )

            return run_plugin_iws_policy_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
                sandbox_id=metadata.sandbox_id,
            )

        return _plugin_iws, "Plugin isolated-workspace policy probe passed."

    if action == "plugin_setup_failure":
        def _plugin_setup_failure(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.plugin_workspace_probe import (
                run_plugin_setup_failure_probe,
            )

            return run_plugin_setup_failure_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
                sandbox_id=metadata.sandbox_id,
            )

        return _plugin_setup_failure, "Plugin setup-failure probe passed."

    if action == "plugin_service_evict":
        def _plugin_service_evict(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.plugin_workspace_probe import (
                run_plugin_service_evict_probe,
            )

            return run_plugin_service_evict_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
                sandbox_id=metadata.sandbox_id,
            )

        return _plugin_service_evict, "Plugin service-evict probe passed."

    # --- ephemeral_workspace (non-cancellation actions; queue-bridge) ------
    # ephemeral_workspace_cancellation is a §C background rewrite — the bridge
    # rejects its background_task_id call, so it is intentionally not wired here.
    if action == "ephemeral_workspace_all_verbs":
        def _eph_all_verbs(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.ephemeral_workspace_probe import (
                run_ephemeral_all_verbs_probe,
            )

            return run_ephemeral_all_verbs_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
                sandbox_id=metadata.sandbox_id,
            )

        return _eph_all_verbs, "Ephemeral workspace all-verbs probe passed."

    if action == "ephemeral_workspace_concurrent_writes":
        def _eph_concurrent(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.ephemeral_workspace_probe import (
                run_ephemeral_concurrent_writes_probe,
            )

            return run_ephemeral_concurrent_writes_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
                sandbox_id=metadata.sandbox_id,
            )

        return _eph_concurrent, "Ephemeral workspace concurrent-writes probe passed."

    if action == "ephemeral_workspace_policy":
        def _eph_policy(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.ephemeral_workspace_probe import (
                run_ephemeral_policy_probe,
            )

            return run_ephemeral_policy_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
                sandbox_id=metadata.sandbox_id,
            )

        return _eph_policy, "Ephemeral workspace policy probe passed."

    if action == "ephemeral_workspace_o1_disk":
        def _eph_o1_disk(call_tool: Any) -> Awaitable[str]:
            from task_center_runner.agent.mock.ephemeral_workspace_probe import (
                run_ephemeral_o1_disk_probe,
            )

            return run_ephemeral_o1_disk_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
                sandbox_id=metadata.sandbox_id,
            )

        return _eph_o1_disk, "Ephemeral workspace O(1)-disk probe passed."

    # ephemeral_workspace_same_path_conflict is intentionally NOT bridged: its
    # probe asyncio.gathers 4 same-path writes and asserts >=1 OCC conflict, but
    # the queue-bridge serializes calls (one loop turn at a time) so no race /
    # no conflict occurs and the probe's "no typed conflicts" guard fires. It
    # needs a concurrency-preserving fan-out (N racing generators, like
    # high_concurrency's CONFLICT_WORKER_COUNT). Falling through to the adapter's
    # NotImplementedError keeps it a clean "not ported" signal, not a confusing
    # assertion failure. See FANOUT_HANDOFF §"Session 2026-05-29 cont."

    return None


__all__ = ["bridge_probe_for", "bridge_script_for", "bridge_turns"]
