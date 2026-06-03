"""Typed dataclass helpers for sandbox audit event emitters."""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any, Literal

Lane = Literal["critical", "normal", "sample"]


def _drop_none(section: Any, *, required: tuple[str, ...] = ()) -> dict[str, Any]:
    """Strip ``None`` values from ``asdict(section)``, preserving any keys listed
    in ``required`` even when their value is ``None``."""
    data = asdict(section)
    result = {k: v for k, v in data.items() if v is not None}
    for key in required:
        if key not in result and key in data:
            result[key] = data[key]
    return result


@dataclass
class DaemonSection:
    """Payload shape for ``daemon.*`` events."""

    boot_epoch_id: int | None = None
    pid: int | None = None
    pressure: float | None = None
    retained_events: int | None = None
    retained_bytes: int | None = None

    def as_dict(self) -> dict[str, Any]:
        return _drop_none(self)


@dataclass
class LayerStackSection:
    """Payload shape for ``layer_stack.*`` events."""

    operation_id: str | None = None
    operation_step: int | None = None
    lease_id: str | None = None
    owner_request_id: str | None = None
    manifest_version: int | None = None
    manifest_root_hash: str | None = None
    layer_count: int | None = None
    lease_wait_ms: float | None = None
    lock_wait_ms: float | None = None
    lease_hold_ms: float | None = None
    prepare_snapshot_ms: float | None = None
    squash_trigger_reason: str | None = None
    squash_input_layers: int | None = None
    squash_result_layers: int | None = None
    squash_failure_kind: str | None = None

    def as_dict(self) -> dict[str, Any]:
        return _drop_none(self)


def build_layer_stack_event(
    event_type: str, layer_stack: LayerStackSection
) -> dict[str, Any]:
    return {
        "type": event_type,
        "payload": {"layer_stack": layer_stack.as_dict()},
    }


@dataclass
class OverlayWorkspaceSection:
    """Payload shape for ``overlay_workspace.*`` events (ephemeral mode)."""

    operation_id: str | None = None
    workspace_mode: str = "ephemeral"
    workspace_handle_id: str | None = None
    lease_id: str | None = None
    manifest_root_hash: str | None = None
    mount_ms: float | None = None
    cleanup_ms: float | None = None
    scratch_removed: bool | None = None
    cleanup_failure_kind: str | None = None
    committed_layer_id: str | None = None
    publish_layer_ms: float | None = None
    changed_path_count: int | None = None
    upperdir_bytes: int | None = None

    def as_dict(self) -> dict[str, Any]:
        return _drop_none(self)


def build_overlay_workspace_event(
    event_type: str, section: OverlayWorkspaceSection
) -> dict[str, Any]:
    return {
        "type": event_type,
        "payload": {"overlay_workspace": section.as_dict()},
    }


@dataclass
class IsolatedWorkspaceSection:
    """Payload shape for ``isolated_workspace.*`` events."""

    operation_id: str | None = None
    workspace_mode: str = "isolated"
    workspace_handle_id: str | None = None
    agent_id: str | None = None
    holder_pid: int | None = None
    holder_pid_alive: bool | None = None
    cgroup_id: str | None = None
    cgroup_removed: bool | None = None
    scratch_removed: bool | None = None
    upperdir_bytes: int | None = None
    upperdir_cap_bytes: int | None = None
    memory_current_bytes: int | None = None
    memory_peak_bytes: int | None = None
    cpu_usage_usec_delta: int | None = None
    orphan_holder_count: int = 0
    orphan_cgroup_count: int = 0
    orphan_scratch_count: int = 0
    sampled_at_monotonic_s: float | None = None

    def as_dict(self) -> dict[str, Any]:
        return _drop_none(self)


def build_isolated_workspace_event(
    event_type: str, section: IsolatedWorkspaceSection
) -> dict[str, Any]:
    return {
        "type": event_type,
        "payload": {"isolated_workspace": section.as_dict()},
    }


@dataclass
class OccSection:
    """Payload shape for ``occ.*`` events."""

    operation_id: str | None = None
    operation_step: int | None = None
    changeset_id: str | None = None
    changed_path_count: int | None = None
    transaction_lock_wait_ms: float | None = None
    prepare_ms: float | None = None
    apply_ms: float | None = None
    commit_ms: float | None = None
    committed_layer_id: str | None = None
    publish_layer_ms: float | None = None
    committed_layer_bytes: int | None = None
    conflict_kind: str | None = None
    conflict_path: str | None = None
    conflict_reason: str | None = None
    base_manifest_version: int | None = None
    current_manifest_version: int | None = None

    def as_dict(self) -> dict[str, Any]:
        return _drop_none(self)


def build_occ_event(event_type: str, section: OccSection) -> dict[str, Any]:
    return {
        "type": event_type,
        "payload": {"occ": section.as_dict()},
    }


@dataclass
class PluginSection:
    """Payload shape for ``plugin.*`` events. Generic — no vendor names."""

    plugin_id: str
    plugin_kind: str
    plugin_version: str | None = None
    plugin_tool_name: str | None = None
    request_bytes: int | None = None
    response_bytes: int | None = None
    duration_ms: float | None = None
    status: str | None = None
    error_kind: str | None = None
    message_hash: str | None = None
    workspace_handle_id: str | None = None
    agent_id: str | None = None
    peak_resident_bytes: int | None = None

    def as_dict(self) -> dict[str, Any]:
        return _drop_none(self, required=("plugin_id", "plugin_kind"))


def build_plugin_event(event_type: str, section: PluginSection) -> dict[str, Any]:
    return {
        "type": event_type,
        "payload": {"plugin": section.as_dict()},
    }


@dataclass
class BackgroundToolSection:
    """Payload shape for ``background_tool.*`` events."""

    background_task_id: str
    task_kind: str | None = None
    tool_name: str | None = None
    agent_id: str | None = None
    uptime_ms: float | None = None
    status: str | None = None
    exit_code: int | None = None
    duration_ms: float | None = None
    error_kind: str | None = None
    cancel_reason: str | None = None
    delivery_latency_ms: float | None = None

    def as_dict(self) -> dict[str, Any]:
        return _drop_none(self, required=("background_task_id",))


def build_background_tool_event(
    event_type: str, section: BackgroundToolSection
) -> dict[str, Any]:
    return {
        "type": event_type,
        "payload": {"background_tool": section.as_dict()},
    }


@dataclass
class ToolCallSection:
    """Payload shape for ``tool_call.*`` events."""

    tool_use_id: str
    tool_name: str
    agent_id: str | None = None
    workspace_mode: str | None = None
    workspace_handle_id: str | None = None
    phase: str | None = None
    duration_ms: float | None = None
    total_ms: float | None = None
    exit_status: str | None = None
    bytes_in: int | None = None
    bytes_out: int | None = None
    phase_totals_rollup: dict[str, float] | None = field(default=None)

    def as_dict(self) -> dict[str, Any]:
        return _drop_none(self, required=("tool_use_id", "tool_name"))


def build_tool_call_event(
    event_type: str, section: ToolCallSection
) -> dict[str, Any]:
    return {
        "type": event_type,
        "payload": {"tool_call": section.as_dict()},
    }


@dataclass
class OsResourceSection:
    """Payload shape for ``os_resource.sampled`` events."""

    sampled_at_monotonic_s: float
    rss_bytes: int | None = None
    cpu_user_s: float | None = None
    cpu_system_s: float | None = None
    cpu_throttled_us: int | None = None
    io_read_bytes: int | None = None
    io_write_bytes: int | None = None
    io_read_ops: int | None = None
    io_write_ops: int | None = None

    def as_dict(self) -> dict[str, Any]:
        return _drop_none(self)


def build_daemon_event(event_type: str, daemon: DaemonSection) -> dict[str, Any]:
    return {
        "type": event_type,
        "payload": {"daemon": daemon.as_dict()},
    }


def build_os_resource_event(os_resource: OsResourceSection) -> dict[str, Any]:
    return {
        "type": "os_resource.sampled",
        "payload": {"os_resource": os_resource.as_dict()},
    }


def safe_emit(event: dict[str, Any], lane: Lane) -> None:
    """Best-effort audit emit hook for host-side Python code."""
    _ = (event, lane)


def safe_record_phase(phase: str, duration_ms: float) -> None:
    """Bridge to :func:`engine.tool_call.phase_buffer.record_phase`.

    Lazy-imported so the sandbox package does not carry an unconditional
    ``engine`` dependency at module-load time. ``record_phase`` no-ops when
    no per-call buffer is active, so callers outside the engine's tool
    dispatch (tests, ad-hoc scripts) see no side effect.

    Used by overlay/OCC publish boundaries (V3 §2/§3 mount/publish phase
    columns) — the framework's own ``record_phase`` calls cover queued /
    exec / capture / release.
    """
    try:
        from engine.tool_call.phase_buffer import record_phase

        record_phase(phase, duration_ms)
    except Exception:  # noqa: BLE001 — phase recording never breaks the hot path
        pass


__all__ = [
    "BackgroundToolSection",
    "DaemonSection",
    "IsolatedWorkspaceSection",
    "Lane",
    "LayerStackSection",
    "OccSection",
    "OsResourceSection",
    "OverlayWorkspaceSection",
    "PluginSection",
    "ToolCallSection",
    "build_background_tool_event",
    "build_daemon_event",
    "build_isolated_workspace_event",
    "build_layer_stack_event",
    "build_occ_event",
    "build_os_resource_event",
    "build_overlay_workspace_event",
    "build_plugin_event",
    "build_tool_call_event",
    "safe_emit",
    "safe_record_phase",
]
