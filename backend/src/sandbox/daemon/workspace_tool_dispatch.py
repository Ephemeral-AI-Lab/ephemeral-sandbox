"""Route daemon tool calls to isolated, direct layer-stack, or overlay execution.

Phase 4 §D1–§D3 introduces per-agent dispatch quiesce. Every daemon RPC
that observes isolated-workspace routing (workspace tool dispatch, plugin
gate) acquires a short-held ``entry_lock`` to check ``exit_pending`` and
increment an ``inflight`` counter. The exit path drains that counter
before mutating routing state, closing the lockless-probe race documented
at ``docs/architecture/tools/isolated-workspace.html:166``.
"""

from __future__ import annotations

import asyncio
import os
from collections.abc import AsyncIterator, Mapping
from contextlib import asynccontextmanager
from dataclasses import dataclass, field
from typing import Any
from uuid import uuid4

from sandbox._shared.clock import monotonic_now
from sandbox._shared.models import Intent, ToolCallRequest, ToolCallResult
from sandbox.daemon.occ_runtime_services import get_occ_runtime_services
from sandbox.daemon.workspace_tool_payloads import (
    project_changeset_result,
    project_conflict_result,
    require_layer_stack_root,
    require_single_file_path,
)
from sandbox.ephemeral_workspace.overlay_registry import get_sandbox_overlay
from sandbox.isolated_workspace._control_plane.pipeline_registry import get_active_pipeline
from sandbox.layer_stack.workspace_binding import (
    WorkspaceBindingError,
    require_workspace_binding,
)
from sandbox.occ.changeset import EditChange, build_api_write_change, is_published_status


_LAYER_STACK_FILE_VERBS = {"edit_file", "read_file", "write_file"}


# ---------------------------------------------------------------------------
# Phase 4 §D1/§D2: per-agent dispatch quiesce primitive.
# ---------------------------------------------------------------------------


class LifecycleInProgressError(Exception):
    """Raised when a dispatch arrives while ``exit_isolated_workspace`` drains.

    The dispatcher converts this to a structured error response so the
    agent retry loop receives an actionable signal (kind:
    ``lifecycle_in_progress``).
    """

    def __init__(self, agent_id: str) -> None:
        super().__init__(
            "exit_isolated_workspace is draining for agent_id="
            f"{agent_id!r}; retry after exit completes"
        )
        self.agent_id = agent_id


@dataclass
class AgentDispatchState:
    """Per-agent quiesce state for daemon RPC paths that observe routing.

    ``entry_lock`` is short-held — it covers the ``exit_pending`` probe and
    the inflight-counter update, never the RPC body itself. The RPC body
    runs unlocked so concurrent agents do not serialize on each other.
    """

    entry_lock: "_OrderedLock" = field(default_factory=lambda: _OrderedLock("entry_lock"))
    inflight: int = 0
    inflight_zero: asyncio.Event = field(default_factory=asyncio.Event)
    exit_pending: bool = False

    def __post_init__(self) -> None:
        # New states default to ``inflight == 0`` so the drain fast-path
        # observes a set event.
        self.inflight_zero.set()


_AGENT_DISPATCH_STATES: dict[str, AgentDispatchState] = {}
_STATES_DICT_LOCK = asyncio.Lock()


async def _ensure_dispatch_state(agent_id: str) -> AgentDispatchState:
    """Lazy state creation. Lives until the first successful exit drains it.

    Calling this on every dispatch is intentional: the plan calls for
    state lifetime ``lazy-on-dispatch``. ``begin_exit_drain`` and
    ``finalize_exit_drain`` own the teardown side.
    """
    async with _STATES_DICT_LOCK:
        state = _AGENT_DISPATCH_STATES.get(agent_id)
        if state is None:
            state = AgentDispatchState()
            _AGENT_DISPATCH_STATES[agent_id] = state
        return state


async def _existing_dispatch_state(agent_id: str) -> AgentDispatchState | None:
    async with _STATES_DICT_LOCK:
        return _AGENT_DISPATCH_STATES.get(agent_id)


@asynccontextmanager
async def acquire_dispatch_slot(
    agent_id: str,
) -> AsyncIterator[AgentDispatchState]:
    """Short-held entry_lock + inflight bookkeeping around any daemon RPC.

    The state's ``entry_lock`` is acquired ONLY for the probe + counter
    update; the caller's body runs after the lock is released so concurrent
    dispatches for the same agent do not serialize on the lock. The
    ``finally`` branch decrements ``inflight`` whether the body succeeded,
    raised, or was cancelled — exit drains rely on this invariant.
    """
    state = await _ensure_dispatch_state(agent_id)
    async with state.entry_lock:
        if state.exit_pending:
            raise LifecycleInProgressError(agent_id)
        state.inflight += 1
        state.inflight_zero.clear()
    try:
        yield state
    finally:
        async with state.entry_lock:
            state.inflight -= 1
            if state.inflight <= 0:
                state.inflight = 0
                state.inflight_zero.set()


async def begin_exit_drain(
    agent_id: str,
    *,
    grace_s: float,
) -> tuple[str, int]:
    """Mark exit as pending and wait for in-flight dispatches to drain.

    Returns ``(mode, inflight_observed)`` where ``mode`` is one of:

    * ``"fast_path"`` — no state existed (no dispatch ever ran for this
      agent). The caller should proceed straight to map mutation.
    * ``"drained"`` — inflight reached zero (snapshot 0 or wait completed).
      The caller should proceed to map mutation inside
      :func:`lifecycle_exit_critical_section`.
    * ``"timeout"`` — the drain exceeded ``grace_s``. ``exit_pending`` has
      been reset so the agent can retry; the caller should NOT mutate
      maps. The returned ``inflight_observed`` is the count at timeout.
    """
    state = await _existing_dispatch_state(agent_id)
    if state is None:
        return "fast_path", 0
    async with state.entry_lock:
        state.exit_pending = True
        snapshot = state.inflight
        if snapshot == 0:
            return "drained", 0
    try:
        await asyncio.wait_for(state.inflight_zero.wait(), timeout=grace_s)
        return "drained", snapshot
    except (asyncio.TimeoutError, TimeoutError):
        async with state.entry_lock:
            current_inflight = state.inflight
            # Reset so a follow-up exit attempt can re-arm the drain.
            state.exit_pending = False
        return "timeout", current_inflight


@asynccontextmanager
async def lifecycle_exit_critical_section(
    agent_id: str,
) -> AsyncIterator[None]:
    """Re-acquire ``entry_lock`` for the map-mutation phase of exit.

    The caller mutates ``IsolatedPipeline._by_agent`` / ``_handles`` inside
    this block; ``_teardown`` runs OUTSIDE this block per Phase 4 design.
    The lock-order rule (AC9) is ``entry_lock`` outer, ``_map_lock`` inner.
    """
    state = await _existing_dispatch_state(agent_id)
    if state is None:
        yield
        return
    async with state.entry_lock:
        yield


async def finalize_exit_drain(agent_id: str) -> None:
    """Delete the per-agent dispatch state after a successful exit.

    Safe to call when no state exists (fast-path exit). On drain timeout
    the caller MUST NOT call this — retained state is reused by the retry.
    """
    async with _STATES_DICT_LOCK:
        _AGENT_DISPATCH_STATES.pop(agent_id, None)


def reset_dispatch_states_for_test() -> None:
    """Test helper: synchronously clear all per-agent dispatch state.

    Used by ``conftest`` between tests so leaked states from one test do
    not poison the next. Not safe to call while dispatches are in flight.
    """
    _AGENT_DISPATCH_STATES.clear()


def _active_isolated_pipeline_for(agent_id: str) -> Any | None:
    isolated_pipeline = get_active_pipeline()
    if (
        isolated_pipeline is not None
        and isolated_pipeline.get_handle(agent_id) is not None
    ):
        return isolated_pipeline
    return None


async def _dispatch_via_workspace_pipeline(
    request: ToolCallRequest,
    isolated_pipeline: Any | None,
) -> ToolCallResult:
    if isolated_pipeline is not None:
        return await isolated_pipeline.run_tool_call(request)
    pipeline = await get_sandbox_overlay(
        require_layer_stack_root(request.args),
        start=False,
    )
    return await pipeline.run_tool_call(request)


async def dispatch_workspace_tool_call(
    args: dict[str, Any],
    *,
    verb: str,
    intent: Intent,
) -> ToolCallResult:
    if verb in _LAYER_STACK_FILE_VERBS:
        require_single_file_path(args)
    agent_id = _request_agent_id(args)
    request = ToolCallRequest(
        invocation_id=str(args.get("invocation_id") or uuid4().hex),
        agent_id=agent_id,
        verb=verb,
        intent=intent,
        args=args,
        background=bool(args.get("background", False)),
    )
    try:
        async with acquire_dispatch_slot(agent_id):
            isolated_pipeline = _active_isolated_pipeline_for(agent_id)
            if isolated_pipeline is None:
                layer_stack_result = await _dispatch_layer_stack_file_request(request)
                if layer_stack_result is not None:
                    return layer_stack_result
            return await _dispatch_via_workspace_pipeline(request, isolated_pipeline)
    except LifecycleInProgressError as exc:
        return _lifecycle_in_progress_payload(exc.agent_id)


def _lifecycle_in_progress_payload(agent_id: str) -> ToolCallResult:
    return {
        "success": False,
        "warnings": [],
        "timings": {},
        "error": {
            "kind": "lifecycle_in_progress",
            "message": (
                "exit_isolated_workspace is draining; retry after exit completes"
            ),
            "details": {"agent_id": agent_id},
        },
    }


# ---------------------------------------------------------------------------
# Phase 4 §AC9: lock-order assertion (test-only) and lock wrapper.
# ---------------------------------------------------------------------------


_TEST_MODE_ENV = "EOS_TEST_MODE"
# Per-task acquisition stack. Each entry is (lock_name, monotonic_ts).
_LOCK_ACQUISITIONS: dict[int, list[tuple[str, float]]] = {}
# Established lock-order rules: ``entry_lock`` must be outer when both
# ``entry_lock`` and ``_map_lock`` are held by the same task.
_OUTER_LOCKS: dict[str, frozenset[str]] = {
    "_map_lock": frozenset({"entry_lock"}),
}


def _is_test_mode() -> bool:
    return os.environ.get(_TEST_MODE_ENV, "").strip().lower() == "true"


def _task_acquisition_stack() -> list[tuple[str, float]] | None:
    task = asyncio.current_task()
    if task is None:
        return None
    return _LOCK_ACQUISITIONS.setdefault(id(task), [])


def _assert_lock_order_in_test_mode(lock_name: str) -> None:
    if not _is_test_mode():
        return
    stack = _task_acquisition_stack()
    if stack is None:
        return
    required_outer = _OUTER_LOCKS.get(lock_name, frozenset())
    if not required_outer:
        return
    held = {name for name, _ in stack}
    missing = required_outer - held
    if missing:
        raise AssertionError(
            f"lock-order violation: acquiring {lock_name!r} requires "
            f"outer {sorted(missing)!r} (task already holds {sorted(held)!r})"
        )


def _register_lock_acquisition(lock_name: str) -> None:
    if not _is_test_mode():
        return
    stack = _task_acquisition_stack()
    if stack is None:
        return
    stack.append((lock_name, monotonic_now()))


def _unregister_lock_acquisition(lock_name: str) -> None:
    if not _is_test_mode():
        return
    task = asyncio.current_task()
    if task is None:
        return
    stack = _LOCK_ACQUISITIONS.get(id(task))
    if stack is None:
        return
    for index in range(len(stack) - 1, -1, -1):
        if stack[index][0] == lock_name:
            del stack[index]
            break
    if not stack:
        _LOCK_ACQUISITIONS.pop(id(task), None)


class _OrderedLock:
    """``asyncio.Lock`` wrapper that records acquisitions for AC9 assertions.

    Outside ``EOS_TEST_MODE=true`` the wrapper is a thin pass-through —
    one extra attribute lookup per ``async with``. The assertion path
    (and per-task stack accounting) is gated entirely behind the env
    check so production overhead stays at zero.
    """

    def __init__(self, name: str) -> None:
        self._name = name
        self._lock = asyncio.Lock()

    @property
    def name(self) -> str:
        return self._name

    def locked(self) -> bool:
        return self._lock.locked()

    async def acquire(self) -> bool:
        _assert_lock_order_in_test_mode(self._name)
        await self._lock.acquire()
        _register_lock_acquisition(self._name)
        return True

    def release(self) -> None:
        _unregister_lock_acquisition(self._name)
        self._lock.release()

    async def __aenter__(self) -> "_OrderedLock":
        await self.acquire()
        return self

    async def __aexit__(self, exc_type, exc, tb) -> None:
        self.release()


async def _dispatch_layer_stack_file_request(
    request: ToolCallRequest,
) -> ToolCallResult | None:
    if request.verb not in _LAYER_STACK_FILE_VERBS:
        return None
    bound_request = _bound_file_request(request)
    if bound_request is None:
        return None
    layer_stack_root, path = bound_request
    if request.verb == "read_file":
        return _read_file_from_layer_stack(layer_stack_root, path)
    if request.verb == "write_file":
        return await _write_file_to_layer_stack(request, layer_stack_root, path)
    return await _edit_file_in_layer_stack(request, layer_stack_root, path)


def _read_file_from_layer_stack(layer_stack_root: str, path: str) -> ToolCallResult:
    total_start = monotonic_now()
    services = get_occ_runtime_services(layer_stack_root)
    read_start = monotonic_now()
    content, exists = services.manager.read_text(path)
    return {
        "success": True,
        "workspace": "ephemeral",
        "content": content if exists else "",
        "exists": exists,
        "encoding": "utf-8",
        "timings": {
            **_layer_stack_file_resource_timings(services, changed_path_count=0),
            "api.read.layer_stack_read_s": monotonic_now() - read_start,
            "api.read.total_s": monotonic_now() - total_start,
        },
    }


async def _write_file_to_layer_stack(
    request: ToolCallRequest,
    layer_stack_root: str,
    path: str,
) -> ToolCallResult:
    total_start = monotonic_now()
    services = get_occ_runtime_services(layer_stack_root)
    content = str(
        request.args.get("content") if request.args.get("content") is not None else ""
    )
    if not bool(request.args.get("overwrite", True)):
        _current, exists = services.manager.read_text(path)
        if exists:
            return {
                **project_conflict_result(
                    verb="write",
                    status="rejected",
                    reason="create_only_existing",
                    path=path,
                    message="file already exists",
                    total_start=total_start,
                    timings_extra=_layer_stack_file_resource_timings(
                        services,
                        changed_path_count=0,
                    ),
                ),
                "workspace": "ephemeral",
            }
    result = await services.occ_service.apply_changeset(
        [build_api_write_change(path=path, final_content=content)]
    )
    payload = project_changeset_result(
        result,
        verb="write",
        total_start=total_start,
        gitignore=services.gitignore,
        timings_extra=_layer_stack_file_resource_timings(
            services,
            changed_path_count=_published_file_count(result),
        ),
    )
    payload["workspace"] = "ephemeral"
    return payload


async def _edit_file_in_layer_stack(
    request: ToolCallRequest,
    layer_stack_root: str,
    path: str,
) -> ToolCallResult:
    total_start = monotonic_now()
    services = get_occ_runtime_services(layer_stack_root)
    changes = _edit_changes(request.args, path)
    result = await services.occ_service.apply_changeset(changes)
    payload = project_changeset_result(
        result,
        verb="edit",
        total_start=total_start,
        gitignore=services.gitignore,
        timings_extra=_layer_stack_file_resource_timings(
            services,
            changed_path_count=_published_file_count(result),
        ),
    )
    payload["workspace"] = "ephemeral"
    payload["applied_edits"] = len(changes) if result.success else 0
    return payload


def _edit_changes(args: Mapping[str, object], path: str) -> list[EditChange]:
    raw_edits = args.get("edits")
    if not isinstance(raw_edits, list):
        raise ValueError("edits must be a list")
    changes: list[EditChange] = []
    for raw in raw_edits:
        if not isinstance(raw, dict):
            raise ValueError("each edit must be an object")
        expected_raw = raw.get("expected_occurrences")
        expected = 1 if expected_raw is None else int(expected_raw)
        if expected < 0:
            raise ValueError("expected_occurrences must be >= 0")
        old_text = str(raw.get("old_text") if raw.get("old_text") is not None else "")
        if not old_text:
            raise ValueError(f"edit anchor old_text must be non-empty for {path}")
        changes.append(
            EditChange(
                path=path,
                old_text=old_text,
                new_text=str(raw.get("new_text") if raw.get("new_text") is not None else ""),
                expected_occurrences=expected,
            )
        )
    return changes


def _bound_file_request(request: ToolCallRequest) -> tuple[str, str] | None:
    try:
        layer_stack_root = require_layer_stack_root(request.args)
        path = _bound_layer_path(
            layer_stack_root,
            require_single_file_path(request.args),
        )
    except WorkspaceBindingError:
        return None
    return layer_stack_root, path


def _bound_layer_path(layer_stack_root: str, raw_path: str) -> str:
    binding = require_workspace_binding(layer_stack_root)
    if raw_path.startswith("/"):
        return binding.layer_path_from_absolute(raw_path)
    return binding.layer_path_from_relative(raw_path)


def _published_file_count(result: object) -> int:
    files = getattr(result, "files", ())
    return sum(1 for file in files if is_published_status(file.status))


def _layer_stack_file_resource_timings(
    services: Any,
    *,
    changed_path_count: int,
) -> dict[str, float]:
    manifest = services.manager.read_active_manifest()
    layers = tuple(getattr(manifest, "layers", ()) or ())
    return {
        "resource.command_exec.changed_path_count": float(changed_path_count),
        "resource.layer_stack.manifest_depth": float(len(layers)),
        "resource.layer_stack.manifest_path_count": float(len(layers)),
        "resource.command_exec.run_dir_tree_exists": 0.0,
        "resource.command_exec.run_dir_tree_bytes": 0.0,
        "resource.command_exec.run_dir_tree_file_count": 0.0,
        "resource.command_exec.run_dir_tree_dir_count": 0.0,
        "resource.command_exec.run_dir_tree_entry_count": 0.0,
        "resource.command_exec.run_dir_tree_truncated": 0.0,
        "resource.command_exec.workspace_tree_exists": 0.0,
        "resource.command_exec.workspace_tree_bytes": 0.0,
        "resource.command_exec.workspace_tree_file_count": 0.0,
        "resource.command_exec.workspace_tree_dir_count": 0.0,
        "resource.command_exec.workspace_tree_entry_count": 0.0,
        "resource.command_exec.workspace_tree_truncated": 0.0,
        "resource.command_exec.upperdir_tree_exists": 0.0,
        "resource.command_exec.upperdir_tree_bytes": 0.0,
        "resource.command_exec.upperdir_tree_file_count": 0.0,
        "resource.command_exec.upperdir_tree_dir_count": 0.0,
        "resource.command_exec.upperdir_tree_entry_count": 0.0,
        "resource.command_exec.upperdir_tree_truncated": 0.0,
    }


def _request_agent_id(args: dict[str, Any]) -> str:
    caller = args.get("caller")
    raw = ""
    if isinstance(caller, dict):
        raw = str(caller.get("agent_id") or caller.get("agent_run_id") or "")
    if not raw:
        raw = str(args.get("agent_id") or "default")
    raw = raw.strip()
    return raw or "default"


__all__ = [
    "AgentDispatchState",
    "LifecycleInProgressError",
    "acquire_dispatch_slot",
    "begin_exit_drain",
    "dispatch_workspace_tool_call",
    "finalize_exit_drain",
    "lifecycle_exit_critical_section",
    "reset_dispatch_states_for_test",
]
