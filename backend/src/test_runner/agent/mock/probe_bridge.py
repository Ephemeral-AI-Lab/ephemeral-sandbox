"""Queue-bridge: run an imperative ``call_tool``-based probe through the REAL loop.

The heavy probe modules (``high_concurrency_probe``, ``heavy_io_zoned_probe``,
``complex_project_build_probe``, …) accept an injected
``call_tool(tool_obj, raw_input, metadata, emit, *, allow_error=...)`` and call
it many times deep in their bodies. To run those bodies through the real
``query.py`` loop WITHOUT rewriting them as async generators, this module
injects a *bridging* ``call_tool`` that hands each call to the driving role
``TurnScript`` — one :class:`Turn` per call — so the ``ScenarioEventSource``
emits it as a scripted ``tool_use`` and the **real loop dispatches it**. The
bridge changes nothing about how tools execute; it only adapts an imperative
body into the scripted event stream. Mock vs. real still differ *only* in the
event source.

This is the "two-level coroutine bridge": the probe runs as a concurrent task;
:func:`bridge_turns` pulls each tool request off a queue and ``yield``s a
``Turn`` at the top level of the role ``TurnScript`` (Python forbids hiding an
async-generator yield inside a helper), resolving the probe's awaited future
with the loop-normalized :class:`~tools.ToolResult`.

Budget: a single agent is capped at its configured ``tool_call_limit`` plus the
engine's hard ceiling. Heavy probes exceed that, so the scenario planner fans
the work out into a generator DAG; each generator's tool stream is budget-sized
and routes through the loop here. Background dispatch
(``background_task_id``) maps onto command sessions: ``exec_command`` returns
``command_session_id`` and ``write_stdin`` polls or sends Ctrl-C.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import re
from collections.abc import AsyncGenerator, Awaitable, Callable
from typing import Any

from message.message import ToolResultBlock
from tools._framework.core.results import ToolResult

from test_runner.agent.mock.event_source import ToolCall, Turn

# A probe coroutine factory: given the bridging call_tool, returns the probe
# coroutine (which returns the artifact path string).
ProbeFactory = Callable[[Callable[..., Awaitable[ToolResult]]], Awaitable[str]]


async def _noop_emit(_event: Any) -> None:
    return None


_COMMAND_SESSION_ID_RE = re.compile(r'"command_session_id"\s*:\s*"([^"]+)"')
_BACKGROUND_POLL_INITIAL_S = 0.05
_BACKGROUND_POLL_MAX_S = 2.0
BackgroundCancelCallback = Callable[[dict[str, Any]], None]


class _CallToolBridge:
    """Provides the bridging ``call_tool`` + a request queue the driver drains."""

    __slots__ = ("_background_aliases", "_on_background_cancel", "_queue")

    def __init__(
        self,
        *,
        on_background_cancel: BackgroundCancelCallback | None = None,
    ) -> None:
        # items: ("call", tool_name, raw_input, future) | ("done", None, None, None)
        self._queue: asyncio.Queue[tuple[str, str | None, dict | None, Any]] = asyncio.Queue()
        self._on_background_cancel = on_background_cancel
        self._background_aliases: dict[str, str] = {}

    async def call_tool(
        self,
        tool_obj: Any,
        raw_input: dict[str, Any],
        metadata: Any = None,  # noqa: ARG002 — loop owns tool_metadata
        emit: Any = None,  # noqa: ARG002 — loop owns the event stream
        *,
        allow_error: bool = False,
        background_task_id: str | None = None,
        **_kwargs: Any,
    ) -> ToolResult:
        if background_task_id is not None:
            result = await self._call_background_tool(
                tool_name=tool_obj.name,
                raw_input=raw_input,
                allow_error=allow_error,
                requested_background_task_id=background_task_id,
            )
            if result.is_error and not allow_error:
                raise RuntimeError(f"{tool_obj.name} failed: {result.output}")
            return result
        result = await self._call_loop_tool(tool_obj.name, raw_input)
        # Probe bodies rely on fail-fast behavior unless the caller opted in to
        # tolerate errors.
        if result.is_error and not allow_error:
            raise RuntimeError(f"{tool_obj.name} failed: {result.output}")
        return result

    async def _call_loop_tool(self, tool_name: str, raw_input: dict[str, Any]) -> ToolResult:
        fut: asyncio.Future[ToolResult] = asyncio.get_running_loop().create_future()
        await self._queue.put(("call", tool_name, self._loop_tool_input(tool_name, raw_input), fut))
        return await asyncio.shield(fut)

    def _loop_tool_input(self, tool_name: str, raw_input: dict[str, Any]) -> dict[str, Any]:
        tool_input = dict(raw_input)
        if tool_name == "exec_command":
            _normalize_exec_command_input(tool_input)
        if tool_name == "write_stdin":
            session_id = str(tool_input.get("command_session_id") or "")
            real_session_id = self._background_aliases.get(session_id)
            if real_session_id:
                tool_input["command_session_id"] = real_session_id
        return tool_input

    async def _call_background_tool(
        self,
        *,
        tool_name: str,
        raw_input: dict[str, Any],
        allow_error: bool,
        requested_background_task_id: str,
    ) -> ToolResult:
        launch_input = dict(raw_input)
        _normalize_exec_command_input(launch_input)
        launch_input.setdefault("yield_time_ms", 50)
        launch = await self._call_loop_tool(
            tool_name,
            launch_input,
        )
        if launch.is_error:
            return launch
        launch_payload = _json_object(launch.output)
        status = str(launch_payload.get("status") or "")
        if status and status != "running":
            return launch
        command_session_id = _parse_command_session_id(launch.output)
        if not command_session_id:
            return ToolResult(
                output=(
                    "Background launch did not expose a command_session_id. "
                    f"requested_background_task_id={requested_background_task_id!r} "
                    f"output={launch.output!r}"
                ),
                is_error=True,
            )
        self._background_aliases[requested_background_task_id] = command_session_id
        try:
            return await self._await_command_session_result(
                command_session_id=command_session_id,
                allow_error=allow_error,
            )
        except asyncio.CancelledError:
            current = asyncio.current_task()
            if current is not None:
                with contextlib.suppress(AttributeError):
                    current.uncancel()
            cancel_result: ToolResult | None = None
            with contextlib.suppress(Exception):
                cancel_result = await self._call_loop_tool(
                    "write_stdin",
                    {
                        "command_session_id": command_session_id,
                        "chars": "\u0003",
                        "yield_time_ms": 50,
                    },
                )
            if cancel_result is not None and not cancel_result.is_error:
                self._publish_background_cancel(
                    tool_name=tool_name,
                    requested_background_task_id=requested_background_task_id,
                    command_session_id=command_session_id,
                )
            raise

    def _publish_background_cancel(
        self,
        *,
        tool_name: str,
        requested_background_task_id: str,
        command_session_id: str,
    ) -> None:
        if self._on_background_cancel is None:
            return
        with contextlib.suppress(Exception):
            self._on_background_cancel(
                {
                    "tool_name": tool_name,
                    "background_task_id": requested_background_task_id,
                    "command_session_id": command_session_id,
                    "invocation_id": command_session_id,
                }
            )

    async def _await_command_session_result(
        self,
        *,
        command_session_id: str,
        allow_error: bool,
    ) -> ToolResult:
        del allow_error
        poll_s = _BACKGROUND_POLL_INITIAL_S
        while True:
            checked = await self._call_loop_tool(
                "write_stdin",
                {
                    "command_session_id": command_session_id,
                    "chars": "",
                    "yield_time_ms": int(poll_s * 1000),
                },
            )
            payload = _json_object(checked.output)
            status = str(payload.get("status") or "")
            if status != "running":
                return checked
            poll_s = min(_BACKGROUND_POLL_MAX_S, poll_s * 2)


def _normalize_exec_command_input(tool_input: dict[str, Any]) -> None:
    if "cmd" not in tool_input and "command" in tool_input:
        tool_input["cmd"] = tool_input.pop("command")


def _parse_command_session_id(output: str) -> str:
    payload = _json_object(output)
    session_id = str(payload.get("command_session_id") or "")
    if session_id:
        return session_id
    match = _COMMAND_SESSION_ID_RE.search(output or "")
    return match.group(1) if match else ""


def _json_object(raw: str) -> dict[str, Any]:
    try:
        parsed = json.loads(raw or "{}")
    except json.JSONDecodeError:
        return {}
    return parsed if isinstance(parsed, dict) else {}


async def bridge_turns(
    factory: ProbeFactory,
    *,
    artifact_out: list[str],
    normalize: Callable[[list[ToolResultBlock]], ToolResult],
    on_background_cancel: BackgroundCancelCallback | None = None,
) -> AsyncGenerator[Turn, list[ToolResultBlock]]:
    """Drive an imperative probe, yielding one :class:`Turn` per tool call.

    ``factory(call_tool)`` builds the probe coroutine. Each ``await call_tool``
    inside it surfaces here as ``yield Turn(calls=(ToolCall,))``; the value sent
    back (the loop's trailing ``ToolResultBlock``s) is normalized and used to
    resolve the probe's awaited future. The probe's return value (artifact path)
    is appended to *artifact_out*. Probe exceptions propagate to the caller.
    """
    bridge = _CallToolBridge(on_background_cancel=on_background_cancel)

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

    def _engine_factory(build: Callable[[], Any], summary: str) -> tuple[ProbeFactory, str]:
        async def _run_script(call_tool: Any) -> str:
            from test_runner.agent.mock.tool_scripts import (
                PreparedToolScriptEngine,
            )

            engine = PreparedToolScriptEngine(call_tool)
            result = await engine.run(build(), metadata=ctx.metadata, emit=_noop_emit)
            return result.artifact

        return _run_script, summary

    if action == "inspect_user_input":

        def _build() -> Any:
            from test_runner.agent.mock.tool_scripts import (
                inspect_user_input_script,
            )

            return inspect_user_input_script(ctx)

        return _engine_factory(
            _build, "Requirement ledger and package DAG were written and read back."
        )

    if action.startswith("execute_package:"):
        package_id = action.split(":", 1)[1]

        def _build_execute() -> Any:
            from test_runner.agent.mock.tool_scripts import (
                execute_package_script,
            )

            return execute_package_script(ctx, package_id=package_id)

        return _engine_factory(
            _build_execute, f"Executed package {package_id} with sandbox evidence."
        )

    if action == "final_reconciliation":

        def _build_final() -> Any:
            from test_runner.agent.mock.tool_scripts import (
                final_reconciliation_script,
            )

            return final_reconciliation_script(ctx)

        return _engine_factory(_build_final, "Final coverage ledger and readback probe passed.")

    if action == "recursive_step":

        def _build_recursive() -> Any:
            from test_runner.agent.mock.tool_scripts import (
                recursive_step_script,
            )

            return recursive_step_script(ctx)

        return _engine_factory(
            _build_recursive, "Recursive workflow step completed with sandbox evidence."
        )

    # --- full_stack scripts -------------------------------------------------
    if action == "inspect_full_user_input":

        def _build_inspect_full() -> Any:
            from test_runner.agent.mock.full_stack_tool_scripts import (
                inspect_full_user_input_script,
            )

            return inspect_full_user_input_script(ctx)

        return _engine_factory(_build_inspect_full, "Full user-input inventory script passed.")

    if action == "occ_conflict_matrix":

        def _build_occ() -> Any:
            from test_runner.agent.mock.full_stack_tool_scripts import (
                occ_conflict_matrix_script,
            )

            return occ_conflict_matrix_script(ctx)

        return _engine_factory(_build_occ, "OCC conflict matrix script passed.")

    if action == "overlay_edge_matrix":

        def _build_overlay() -> Any:
            from test_runner.agent.mock.full_stack_tool_scripts import (
                overlay_edge_matrix_script,
            )

            return overlay_edge_matrix_script(ctx)

        return _engine_factory(_build_overlay, "Overlay edge matrix script passed.")

    if action == "layerstack_squash_lease":

        def _build_layerstack() -> Any:
            from test_runner.agent.mock.full_stack_tool_scripts import (
                layerstack_squash_lease_script,
            )

            return layerstack_squash_lease_script(ctx)

        return _engine_factory(_build_layerstack, "LayerStack squash-lease script passed.")

    if action == "lsp_refresh_semantics":

        def _build_lsp() -> Any:
            from test_runner.agent.mock.full_stack_tool_scripts import (
                lsp_refresh_semantics_script,
            )

            return lsp_refresh_semantics_script(ctx)

        return _engine_factory(_build_lsp, "LSP refresh-semantics script passed.")

    if action == "recursive_oversized_matrix":

        def _build_recursive_matrix() -> Any:
            from test_runner.agent.mock.full_stack_tool_scripts import (
                recursive_oversized_matrix_script,
            )

            return recursive_oversized_matrix_script(ctx)

        return _engine_factory(_build_recursive_matrix, "Recursive oversized matrix script passed.")

    if action == "full_stack_final_reconciliation":

        def _build_full_stack_final() -> Any:
            from test_runner.agent.mock.full_stack_tool_scripts import (
                final_reconciliation_script as full_stack_final_reconciliation_script,
            )

            return full_stack_final_reconciliation_script(ctx)

        return _engine_factory(
            _build_full_stack_final, "Full-stack final reconciliation script passed."
        )

    if action == "capacity_metrics_full_system":

        def _build_capacity() -> Any:
            from test_runner.agent.mock.capacity_actions import (
                full_system_capacity_metrics_script,
            )

            return full_system_capacity_metrics_script(ctx)

        return _engine_factory(_build_capacity, "Full-system capacity metrics script passed.")

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

    if action in {"complex_project_build", "complex_project_build_smoke"}:
        smoke = action.endswith("_smoke")

        def _complex_project_build(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.complex_project_build_probe import (
                run_complex_project_build_probe,
            )

            return run_complex_project_build_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                publish=probe_ctx.publish,
                publish_mock_record=probe_ctx.publish_mock_record,
                record_tool_check=probe_ctx.record_check,
                caller=probe_ctx.caller(),
                sandbox_id=probe_ctx.sandbox_id(),
                smoke=smoke,
            )

        suffix = " smoke" if smoke else ""
        return _complex_project_build, f"Complex project-build{suffix} probe passed."

    if action in {
        "complex_project_build_grep_glob",
        "complex_project_build_grep_glob_smoke",
    }:
        smoke = action.endswith("_smoke")

        def _grep_glob(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.complex_project_build_grep_glob_probe import (
                run_complex_project_build_grep_glob_probe,
            )

            return run_complex_project_build_grep_glob_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                publish=probe_ctx.publish,
                publish_mock_record=probe_ctx.publish_mock_record,
                record_tool_check=probe_ctx.record_check,
                caller=probe_ctx.caller(),
                sandbox_id=probe_ctx.sandbox_id(),
                smoke=smoke,
            )

        suffix = " smoke" if smoke else ""
        return _grep_glob, f"Complex project-build grep/glob{suffix} probe passed."

    if action in {
        "complex_project_build_shell_edit_lsp",
        "complex_project_build_shell_edit_lsp_smoke",
    }:
        smoke = action.endswith("_smoke")

        def _shell_edit_lsp(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.complex_project_build_shell_edit_lsp_probe import (
                run_complex_project_build_shell_edit_lsp_probe,
            )

            return run_complex_project_build_shell_edit_lsp_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                publish=probe_ctx.publish,
                publish_mock_record=probe_ctx.publish_mock_record,
                record_tool_check=probe_ctx.record_check,
                caller=probe_ctx.caller(),
                sandbox_id=probe_ctx.sandbox_id(),
                smoke=smoke,
            )

        suffix = " smoke" if smoke else ""
        return (
            _shell_edit_lsp,
            f"Complex project-build shell-edit LSP{suffix} probe passed.",
        )

    background_probe = _background_probe_factory(action, metadata, probe_ctx)
    if background_probe is not None:
        return background_probe

    if action == "high_concurrency_seed":

        def _seed(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.high_concurrency_probe import (
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
            from test_runner.agent.mock.high_concurrency_probe import (
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
            from test_runner.agent.mock.high_concurrency_probe import (
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
            from test_runner.agent.mock.heavy_io_zoned_probe import (
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
            from test_runner.agent.mock.heavy_io_zoned_probe import (
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
            from test_runner.agent.mock.heavy_io_zoned_probe import (
                run_heavy_io_zoned_reconcile_probe,
            )

            return run_heavy_io_zoned_reconcile_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _hiz_reconcile, "Heavy-IO zoned sandbox reconciliation passed."

    if action == "complex_project_build_shell_edit_lsp_shared_bootstrap":

        def _complex_shared_bootstrap(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.complex_project_build_shell_edit_lsp_probe import (
                run_complex_project_build_shell_edit_lsp_shared_bootstrap_probe,
            )

            return run_complex_project_build_shell_edit_lsp_shared_bootstrap_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                publish=probe_ctx.publish,
                publish_mock_record=probe_ctx.publish_mock_record,
                record_tool_check=probe_ctx.record_check,
                caller=probe_ctx.caller(),
                sandbox_id=probe_ctx.sandbox_id(),
            )

        return (
            _complex_shared_bootstrap,
            "Complex project-build shell-edit LSP shared-bootstrap smoke probe passed.",
        )

    # --- auto_squash_commit_resume fan-out -------------------------------
    if action == "auto_squash_seed":

        def _auto_seed(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.auto_squash_probe import (
                run_auto_squash_seed_probe,
            )

            return run_auto_squash_seed_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _auto_seed, "Auto-squash seed passed."

    if action == "auto_squash_squash_a":

        def _auto_squash_a(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.auto_squash_probe import (
                run_auto_squash_squash_a_probe,
            )

            return run_auto_squash_squash_a_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _auto_squash_a, "Auto-squash depth slice A passed."

    if action == "auto_squash_squash_b":

        def _auto_squash_b(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.auto_squash_probe import (
                run_auto_squash_squash_b_probe,
            )

            return run_auto_squash_squash_b_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _auto_squash_b, "Auto-squash depth slice B passed."

    if action == "auto_squash_independent":

        def _auto_independent(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.auto_squash_probe import (
                run_auto_squash_independent_probe,
            )

            return run_auto_squash_independent_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _auto_independent, "Auto-squash independent generator passed."

    if action == "auto_squash_reconcile":

        def _auto_reconcile(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.auto_squash_probe import (
                run_auto_squash_reconcile_probe,
            )

            return run_auto_squash_reconcile_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                publish=probe_ctx.publish,
                publish_mock_record=probe_ctx.publish_mock_record,
                record_tool_check=probe_ctx.record_check,
            )

        return _auto_reconcile, "Auto-squash reconciliation passed."

    # --- plugin_workspace (single-action scenarios; all queue-bridge) -------
    if action == "plugin_read_only_lsp_refresh":

        def _plugin_read_only(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.plugin_workspace_probe import (
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
            from test_runner.agent.mock.plugin_workspace_probe import (
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
            from test_runner.agent.mock.plugin_workspace_probe import (
                run_plugin_intent_contract_probe,
            )

            return run_plugin_intent_contract_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _plugin_intent, "Plugin intent-contract probe passed."

    if action == "plugin_setup_failure":

        def _plugin_setup_failure(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.plugin_workspace_probe import (
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
            from test_runner.agent.mock.plugin_workspace_probe import (
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

    # --- ephemeral_workspace actions; queue-bridge -------------------------
    if action == "ephemeral_workspace_all_verbs":

        def _eph_all_verbs(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.ephemeral_workspace_probe import (
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
            from test_runner.agent.mock.ephemeral_workspace_probe import (
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
            from test_runner.agent.mock.ephemeral_workspace_probe import (
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
            from test_runner.agent.mock.ephemeral_workspace_probe import (
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

    if action == "ephemeral_same_path_conflict_seed":

        def _eph_same_path_seed(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.ephemeral_workspace_probe import (
                run_ephemeral_same_path_conflict_seed_probe,
            )

            return run_ephemeral_same_path_conflict_seed_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _eph_same_path_seed, "Ephemeral same-path conflict seed passed."

    if action.startswith("ephemeral_same_path_conflict_writer:"):
        index = int(action.split(":", 1)[1])

        def _eph_same_path_writer(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.ephemeral_workspace_probe import (
                run_ephemeral_same_path_conflict_writer_probe,
            )

            return run_ephemeral_same_path_conflict_writer_probe(
                index=index,
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return _eph_same_path_writer, f"Ephemeral same-path writer {index} passed."

    if action == "ephemeral_same_path_conflict_reconcile":

        def _eph_same_path_reconcile(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.ephemeral_workspace_probe import (
                run_ephemeral_same_path_conflict_reconcile_probe,
            )

            return run_ephemeral_same_path_conflict_reconcile_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
            )

        return (
            _eph_same_path_reconcile,
            "Ephemeral same-path conflict reconciliation passed.",
        )

    return None


def _background_probe_factory(
    action: str,
    metadata: Any,
    probe_ctx: Any,
) -> tuple[ProbeFactory, str] | None:
    background_modes = {
        "background_shell_golden": (
            "run_background_shell_golden_probe",
            "Background-shell golden probe passed.",
        ),
        "background_shell_stop": (
            "run_background_shell_stop_probe",
            "Background-shell cancel probe passed.",
        ),
        "background_shell_interleave": (
            "run_background_shell_interleave_probe",
            "Background-shell interleave probe passed.",
        ),
        "background_shell_exhaustion": (
            "run_background_shell_exhaustion_probe",
            "Background-shell exhaustion probe passed.",
        ),
        "background_shell_partial_write_cancel": (
            "run_background_shell_partial_write_cancel_probe",
            "Background-shell partial-write-cancel probe passed.",
        ),
        "background_shell_stop_during_maintenance": (
            "run_background_shell_maintenance_probe",
            "Background-shell cancel-during-maintenance probe passed.",
        ),
        "background_shell_late_cancel_race": (
            "run_background_shell_late_cancel_probe",
            "Background-shell late-cancel-race probe passed.",
        ),
        "background_mixed_fg_bg_same_path_conflict": (
            "run_background_mixed_fg_bg_same_path_conflict_probe",
            "Background-shell mixed foreground/background conflict probe passed.",
        ),
        "background_heartbeat_loss_reaps_only_stale_bg": (
            "run_background_heartbeat_loss_probe",
            "Background-shell heartbeat-loss probe passed.",
        ),
        "background_exit_iws_drains_agent_tasks": (
            "run_background_exit_iws_drains_agent_tasks_probe",
            "Background-shell isolated-workspace drain probe passed.",
        ),
        "background_engine_restart_no_lease_leak": (
            "run_background_engine_restart_no_lease_leak_probe",
            "Background-shell engine-restart cleanup probe passed.",
        ),
        "background_many_small_writes_do_not_starve_dispatcher": (
            "run_background_many_small_writes_probe",
            "Background-shell many-small-writes probe passed.",
        ),
        "background_mixed_op_concurrent": (
            "run_background_mixed_op_concurrent_probe",
            "Background-shell mixed-op concurrent probe passed.",
        ),
    }
    if action == "ephemeral_workspace_cancellation":

        def _ephemeral_cancel(call_tool: Any) -> Awaitable[str]:
            from test_runner.agent.mock.ephemeral_workspace_probe import (
                run_ephemeral_cancellation_probe,
            )

            return run_ephemeral_cancellation_probe(
                metadata=metadata,
                emit=_noop_emit,
                call_tool=call_tool,
                record_tool_check=probe_ctx.record_check,
                sandbox_id=probe_ctx.sandbox_id(),
            )

        return _ephemeral_cancel, "Ephemeral workspace cancellation probe passed."

    item = background_modes.get(action)
    if item is None:
        return None
    function_name, summary = item

    def _background(call_tool: Any) -> Awaitable[str]:
        from test_runner.agent.mock import background_shell_probe

        probe = getattr(background_shell_probe, function_name)
        return probe(
            metadata=metadata,
            emit=_noop_emit,
            call_tool=call_tool,
            record_tool_check=probe_ctx.record_check,
        )

    return _background, summary


__all__ = ["bridge_probe_for", "bridge_script_for", "bridge_turns"]
