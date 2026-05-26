"""Live E2E performance report — V3 layout (Phase 3).

Builds a per-run report from the daemon-audit pull events recorded in
``sandbox_events.jsonl`` (plus rotated ``.gz`` history). The V3 layout
follows
``docs/daemon-audit-pull-consolidation-v3/phase-3-report-and-release-gates.md``
§Fixed report layout: §1 Summary … §13 Warnings.

Single source of truth: the V3 section builders read ``payload.<section>``
ONLY — never ``payload.daemon_event`` (enforced by
``test_report_consumer_reads_promoted_payload_section_not_daemon_event``).

For backward compatibility the JSON still exposes the legacy ``tools`` and
``hotspots`` blocks that downstream dashboards consume; the V3 MD renderer
ignores them and renders solely from ``sandbox.sections``.
"""

from __future__ import annotations

import asyncio
import json
import logging
from collections import Counter, defaultdict
from collections.abc import Iterable, Mapping, Sequence
from datetime import UTC, datetime
from pathlib import Path
from statistics import median
from typing import Any

from task_center_runner.audit.io import atomic_write_pretty_json, atomic_write_text

logger = logging.getLogger(__name__)

REPORT_SCHEMA = "task_center_runner.performance_report.v3"
_SLOWEST_LIMIT = 25
_PHASE_BREAKDOWN_TOP_N = 10

# Allowed plugin_kind values per V3 README §Requirement 2 / Closer D enum.
_ALLOWED_PLUGIN_KINDS: tuple[str, ...] = (
    "language_server",
    "formatter",
    "indexer",
    "build_daemon",
    "mcp_bridge",
    "custom",
)

# Six framework-boundary phases. ``queued`` / ``exec`` / ``capture`` /
# ``release`` are recorded by the framework's dispatcher (Slice 7).
# ``mount`` is recorded by ``sandbox/overlay/lifecycle.py`` via
# :func:`safe_record_phase`; ``publish`` by ``sandbox/occ/service.py``.
# All six populate ``phase_totals_rollup`` on ``tool_call.finished`` and
# render as percentile columns in §2 / glyphs in §3.
_PHASE_ORDER: tuple[str, ...] = (
    "queued",
    "mount",
    "exec",
    "capture",
    "publish",
    "release",
)

# Release-gate thresholds (V3 §Gate matrix).
_OVERHEAD_GATE_LATENCY_DELTA_MS = 5.0
_OVERHEAD_GATE_DAEMON_RSS_DELTA_MIB = 16.0
_OVERHEAD_GATE_RUNNER_CPU_DELTA_PCT = 0.5
_OVERHEAD_GATE_SANDBOX_DISK_DELTA_BYTES = 0
_BUFFER_PRESSURE_WARNING = 0.8
_UPPERDIR_FRACTION_WARNING = 0.8
_FLOOR_ESCALATED_DEFAULT_MS = 100  # daemon_pull.DEFAULT_FLOOR_MS

# Legacy v2 family mapping — kept so the back-compat ``sandbox.families``
# block continues to populate for dashboards that still read it.
_SANDBOX_FAMILY_BY_EVENT: Mapping[str, str] = {
    "sandbox_conflict_detected": "occ",
    "sandbox_occ_changeset_received": "occ",
    "sandbox_occ_changes_committed": "occ",
    "pipeline_executed": "overlay",
    "sandbox_layer_stack_lease_acquired": "layer_stack",
    "sandbox_layer_stack_layer_created": "layer_stack",
    "sandbox_layer_stack_layers_squashed": "layer_stack",
    "sandbox_write_committed": "sandbox_tool",
    "sandbox_edit_committed": "sandbox_tool",
    "sandbox_shell_committed": "sandbox_tool",
    "sandbox_batch_edit_applied": "sandbox_tool",
    "sandbox_resource_snapshot": "resource",
}
_RUN_DELTA_RESOURCE_KEYS = frozenset(
    {
        "resource.cgroup.cpu_usage_usec",
        "resource.cgroup.cpu_user_usec",
        "resource.cgroup.cpu_system_usec",
        "resource.cgroup.cpu_nr_periods",
        "resource.cgroup.cpu_nr_throttled",
        "resource.cgroup.cpu_throttled_usec",
        "resource.cgroup.cpu_nr_bursts",
        "resource.cgroup.cpu_burst_usec",
        "resource.cgroup.io_rbytes",
        "resource.cgroup.io_wbytes",
        "resource.cgroup.io_rios",
        "resource.cgroup.io_wios",
        "resource.cgroup.io_dbytes",
        "resource.cgroup.io_dios",
    }
)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def build_performance_report(
    run_dir: Path,
    tool_performance: Mapping[str, Any],
    *,
    daemon_audit_puller_stats: Mapping[str, Any] | None = None,
    overhead_metadata: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Build the V3 performance report.

    Parameters
    ----------
    run_dir:
        Run directory holding ``sandbox_events.jsonl`` (+ rotated ``.gz``).
    tool_performance:
        Legacy ``MetricsAggregator.performance_snapshot()`` output. The V3
        sections do NOT consume this; it lives on for backward compat under
        ``tools`` / ``hotspots``.
    daemon_audit_puller_stats:
        Optional final puller stats dict (from
        :meth:`AuditRecorder.final_daemon_audit_puller_stats`). Drives §11.
    overhead_metadata:
        Optional measurement output from the release-gate harness. Shape
        described in :func:`_default_overhead_section`.
    """
    run_path = Path(run_dir)
    rows = list(_iter_jsonl(run_path / "sandbox_events.jsonl"))
    legacy_sandbox = _build_legacy_sandbox_report(rows)
    tool_report = dict(tool_performance)
    artifact_inventory = _collect_artifact_inventory(run_path)
    sections = _build_v3_sections(
        rows,
        daemon_audit_puller_stats=daemon_audit_puller_stats,
        overhead_metadata=overhead_metadata,
        artifact_inventory=artifact_inventory,
    )
    forensic_deltas = _collect_forensic_deltas(rows)
    if forensic_deltas:
        sections["forensic_deltas"] = forensic_deltas
    report: dict[str, Any] = {
        "schema": REPORT_SCHEMA,
        "generated_at": datetime.now(UTC).isoformat(),
        "run": _read_json(run_path / "run.json"),
        "artifacts": {
            "run_dir": str(run_path),
            "metrics_json": "metrics.json",
            "sandbox_events_jsonl": "sandbox_events.jsonl",
            "performance_report_json": "performance_report.json",
            "performance_report_md": "performance_report.md",
        },
        "totals": _build_totals(tool_report, legacy_sandbox),
        "tools": tool_report,
        "sandbox": {
            "sections": sections,
            # Legacy back-compat: family/timing/resource rollups derived
            # from the v2 event types.
            **legacy_sandbox,
        },
        "hotspots": _build_hotspots(tool_report, legacy_sandbox),
    }
    return report


def write_performance_reports(
    run_dir: Path,
    tool_performance: Mapping[str, Any],
    *,
    daemon_audit_puller_stats: Mapping[str, Any] | None = None,
    overhead_metadata: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Write ``performance_report.json`` and ``performance_report.md``."""
    report = build_performance_report(
        run_dir,
        tool_performance,
        daemon_audit_puller_stats=daemon_audit_puller_stats,
        overhead_metadata=overhead_metadata,
    )
    _atomic_write_json(Path(run_dir) / "performance_report.json", report)
    _atomic_write_text(
        Path(run_dir) / "performance_report.md",
        render_performance_report_markdown(report),
    )
    return report


def render_performance_report_markdown(report: Mapping[str, Any]) -> str:
    """Render the V3 §1-§13 layout from ``sandbox.sections``."""
    run = _as_mapping(report.get("run"))
    sandbox = _as_mapping(report.get("sandbox"))
    sections = _as_mapping(sandbox.get("sections"))
    run_id = (
        run.get("task_center_run_id")
        or run.get("instance_id")
        or "(unknown-run)"
    )

    lines: list[str] = [f"# Performance & Resource Report — {run_id}", ""]
    lines.extend(_render_section_1_summary(sections))
    lines.extend(_render_section_2_per_tool_timing(sections))
    lines.extend(_render_section_3_per_tool_phase_breakdown(sections))
    lines.extend(_render_section_4_background_tool_calls(sections))
    lines.extend(_render_section_5_plugin_activity(sections))
    lines.extend(_render_section_6_overlay_workspace(sections))
    lines.extend(_render_section_7_layer_stack(sections))
    lines.extend(_render_section_8_occ(sections))
    lines.extend(_render_section_9_isolated_workspace(sections))
    lines.extend(_render_section_10_os_resource(sections))
    lines.extend(_render_section_11_daemon_audit_pull(sections))
    lines.extend(_render_section_12_overhead(sections))
    lines.extend(_render_section_13_warnings(sections))
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# V3 sandbox.sections builder
# ---------------------------------------------------------------------------


def _build_v3_sections(
    rows: Sequence[Mapping[str, Any]],
    *,
    daemon_audit_puller_stats: Mapping[str, Any] | None,
    overhead_metadata: Mapping[str, Any] | None,
    artifact_inventory: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Build §1-§13 from pulled events (``payload.<section>`` only).

    Per V3 §Dual-write authoritativeness: this function MUST NOT touch
    ``payload.daemon_event``. The slice-1 module-boundary lint
    (``test_daemon_event_writer_module_boundary``) enforces the rule by
    grepping the source.
    """
    indexed = _index_rows_by_event_type(rows)

    summary = _section_summary(rows, indexed)
    per_tool_timing = _section_per_tool_timing(indexed)
    per_tool_phase_breakdown = _section_per_tool_phase_breakdown(indexed)
    background_tool_calls = _section_background_tool_calls(indexed)
    plugin_activity = _section_plugin_activity(indexed)
    overlay_workspace = _section_overlay_workspace(indexed)
    layer_stack = _section_layer_stack(indexed)
    occ = _section_occ(indexed)
    isolated_workspace = _section_isolated_workspace(indexed)
    os_resource = _section_os_resource(indexed)
    daemon_audit_pull = _section_daemon_audit_pull(
        daemon_audit_puller_stats, indexed
    )
    overhead = _section_overhead(
        overhead_metadata,
        daemon_audit_pull,
        artifact_inventory=artifact_inventory,
    )
    # Promote puller counters into §1's audit_summary so the one-glance
    # summary stays consistent with §11.
    summary["audit_summary"]["events_pulled"] = int(
        daemon_audit_pull.get("events_pulled") or 0
    )
    summary["audit_summary"]["dropped_event_count"] = int(
        daemon_audit_pull.get("dropped_event_count") or 0
    )
    summary["audit_summary"]["max_buffer_pressure"] = max(
        float(summary["audit_summary"].get("max_buffer_pressure") or 0.0),
        float(daemon_audit_pull.get("max_buffer_pressure") or 0.0),
    )
    summary["audit_summary"]["floor_raises"] = int(
        daemon_audit_pull.get("floor_raises") or 0
    )
    warnings = _collect_warnings(
        summary=summary,
        daemon_audit_pull=daemon_audit_pull,
        isolated_workspace=isolated_workspace,
        overlay_workspace=overlay_workspace,
        layer_stack=layer_stack,
        occ=occ,
        os_resource=os_resource,
        event_count=len(rows),
    )
    return {
        "summary": summary,
        "per_tool_timing": per_tool_timing,
        "per_tool_phase_breakdown": per_tool_phase_breakdown,
        "background_tool_calls": background_tool_calls,
        "plugin_activity": plugin_activity,
        "overlay_workspace": overlay_workspace,
        "layer_stack": layer_stack,
        "occ": occ,
        "isolated_workspace": isolated_workspace,
        "os_resource": os_resource,
        "daemon_audit_pull": daemon_audit_pull,
        "overhead": overhead,
        "warnings": warnings,
    }


# ---------------------------------------------------------------------------
# §1 Summary
# ---------------------------------------------------------------------------


def _section_summary(
    rows: Sequence[Mapping[str, Any]],
    indexed: Mapping[str, list[Mapping[str, Any]]],
) -> dict[str, Any]:
    """Build §1 from the JSONL row stream + indexed event lookup.

    Phase 3 deferral D8: ``event_count`` is the raw count of normalized
    rows in ``sandbox_events.jsonl`` (live + rotated history), while
    ``audit_summary.events_pulled`` (mirrored from §11) reflects the
    runner-side puller's counter. After a daemon restart or partial
    flush these can drift; the §13 ``audit.events_count_drift`` warning
    surfaces the divergence and points at §11's
    ``daemon_restarts_observed`` for root-cause.
    """
    tool_finished = indexed.get("tool_call.finished", [])
    tools_called = len(tool_finished)
    background_completed = (
        len(indexed.get("background_tool.completed", []))
        + len(indexed.get("background_tool.failed", []))
        + len(indexed.get("background_tool.cancelled", []))
    )
    sandbox_ops = sum(
        len(indexed.get(name, []))
        for name in (
            "overlay_workspace.mounted",
            "overlay_workspace.published",
            "overlay_workspace.cleaned",
            "occ.apply_committed",
            "occ.publish_layer",
            "layer_stack.lease_acquired",
        )
    )
    durations = [
        _tool_call_total_ms(_payload_section(row, "tool_call"))
        for row in tool_finished
    ]
    duration_total_ms = float(sum(d or 0.0 for d in durations))

    rss_peak = _peak_int(indexed.get("os_resource.sampled", []), "rss_bytes")
    upperdir_peak = _peak_int(
        indexed.get("isolated_workspace.sampled", []), "upperdir_bytes"
    )
    layer_count_peak = _peak_int(
        indexed.get("layer_stack.lease_acquired", [])
        + indexed.get("layer_stack.snapshot_prepared", []),
        "layer_count",
        section_key="layer_stack",
    )

    pressure_events = indexed.get("daemon.audit_buffer_pressure", [])
    max_buffer_pressure = 0.0
    for event_row in pressure_events:
        daemon = _payload_section(event_row, "daemon")
        if not daemon:
            continue
        try:
            pressure = float(daemon.get("pressure") or 0.0)
        except (TypeError, ValueError):
            pressure = 0.0
        if pressure > max_buffer_pressure:
            max_buffer_pressure = pressure

    return {
        "duration_total_ms": duration_total_ms,
        "tools_called": tools_called,
        "background_tools": background_completed,
        "sandbox_ops": sandbox_ops,
        "event_count": len(rows),
        "peak": {
            "rss_bytes": rss_peak,
            "upperdir_bytes_total": upperdir_peak,
            "layer_count": layer_count_peak,
        },
        "audit_summary": {
            # Mirrored from §11 — convenient one-glance view in §1.
            "events_pulled": 0,
            "dropped_event_count": 0,
            "max_buffer_pressure": max_buffer_pressure,
            "floor_raises": 0,
        },
    }


# ---------------------------------------------------------------------------
# §2 Per-tool timing (split by workspace_mode)
# ---------------------------------------------------------------------------


def _section_per_tool_timing(
    indexed: Mapping[str, list[Mapping[str, Any]]],
) -> dict[str, Any]:
    """Per (tool_name, workspace_mode) row with phase columns.

    The rollup is computed from ``tool_call.finished.phase_totals_rollup``
    so the percentiles stay accurate even when individual phase events
    were dropped under the slow-tail rule.
    """
    rows: dict[tuple[str, str], dict[str, Any]] = {}
    for event_row in indexed.get("tool_call.finished", []):
        section = _payload_section(event_row, "tool_call")
        if not section:
            continue
        tool_name = str(section.get("tool_name") or "")
        workspace_mode = str(section.get("workspace_mode") or "default")
        key = (tool_name, workspace_mode)
        bucket = rows.setdefault(
            key,
            {
                "tool_name": tool_name,
                "workspace_mode": workspace_mode,
                "calls": 0,
                "_phase_samples": {phase: [] for phase in _PHASE_ORDER},
                "_total_ms_samples": [],
            },
        )
        bucket["calls"] += 1
        rollup = _as_mapping(section.get("phase_totals_rollup"))
        for phase in _PHASE_ORDER:
            value = rollup.get(f"{phase}_ms")
            if isinstance(value, (int, float)):
                bucket["_phase_samples"][phase].append(float(value))
        total_ms = _tool_call_total_ms(section)
        if total_ms is not None:
            bucket["_total_ms_samples"].append(total_ms)
    serialized = []
    for bucket in rows.values():
        record: dict[str, Any] = {
            "tool_name": bucket["tool_name"],
            "workspace_mode": bucket["workspace_mode"],
            "calls": bucket["calls"],
            "phases": {},
            "total_ms": _percentile_record(bucket["_total_ms_samples"]),
        }
        for phase in _PHASE_ORDER:
            samples = bucket["_phase_samples"][phase]
            record["phases"][phase] = _percentile_record(samples)
        serialized.append(record)
    serialized.sort(
        key=lambda row: (
            -float(row["total_ms"].get("p95") or 0.0),
            row["tool_name"],
        )
    )
    return {"rows": serialized}


# ---------------------------------------------------------------------------
# §3 Per-tool phase breakdown (top-10 by total_ms)
# ---------------------------------------------------------------------------


def _section_per_tool_phase_breakdown(
    indexed: Mapping[str, list[Mapping[str, Any]]],
) -> dict[str, Any]:
    finished = indexed.get("tool_call.finished", [])
    phase_totals: dict[tuple[str, str], dict[str, float]] = defaultdict(
        lambda: dict.fromkeys(_PHASE_ORDER, 0.0)
    )
    overall_total: dict[tuple[str, str], float] = defaultdict(float)
    for event_row in finished:
        section = _payload_section(event_row, "tool_call")
        if not section:
            continue
        tool_name = str(section.get("tool_name") or "")
        workspace_mode = str(section.get("workspace_mode") or "default")
        key = (tool_name, workspace_mode)
        rollup = _as_mapping(section.get("phase_totals_rollup"))
        for phase in _PHASE_ORDER:
            value = rollup.get(f"{phase}_ms")
            if isinstance(value, (int, float)):
                phase_totals[key][phase] += float(value)
                overall_total[key] += float(value)
    ranked = sorted(
        overall_total.items(), key=lambda kv: kv[1], reverse=True
    )[:_PHASE_BREAKDOWN_TOP_N]
    rows = []
    for (tool_name, workspace_mode), total_ms in ranked:
        phases = phase_totals[(tool_name, workspace_mode)]
        rows.append(
            {
                "tool_name": tool_name,
                "workspace_mode": workspace_mode,
                "total_ms": total_ms,
                "phases_ms": dict(phases),
                "phases_fraction": {
                    phase: (value / total_ms if total_ms > 0 else 0.0)
                    for phase, value in phases.items()
                },
            }
        )
    return {"rows": rows, "top_n": _PHASE_BREAKDOWN_TOP_N}


# ---------------------------------------------------------------------------
# §4 Background tool calls
# ---------------------------------------------------------------------------


def _section_background_tool_calls(
    indexed: Mapping[str, list[Mapping[str, Any]]],
) -> dict[str, Any]:
    started = indexed.get("background_tool.started", [])
    terminal = (
        indexed.get("background_tool.completed", [])
        + indexed.get("background_tool.failed", [])
        + indexed.get("background_tool.cancelled", [])
    )
    delivered = indexed.get("background_tool.delivered", [])
    heartbeats = indexed.get("background_tool.heartbeat", [])

    rows_by_task: dict[str, dict[str, Any]] = {}
    for event_row in started:
        section = _payload_section(event_row, "background_tool")
        task_id = str(section.get("background_task_id") or "")
        if not task_id:
            continue
        rows_by_task.setdefault(
            task_id,
            {
                "background_task_id": task_id,
                "tool_name": section.get("tool_name") or "",
                "task_kind": section.get("task_kind") or "",
                # ``started_seq`` is the daemon-ring sequence number for the
                # ``background_tool.started`` event — a stable cross-event
                # ordering proxy. Pulled events do not carry a top-level
                # ``ts`` today, so this is the canonical ordering field
                # (Phase 3 deferral D5, option b).
                "started_seq": event_row.get("seq"),
                "status": "started",
                "duration_ms": None,
                "delivery_latency_ms": None,
            },
        )
    for event_row in terminal:
        section = _payload_section(event_row, "background_tool")
        task_id = str(section.get("background_task_id") or "")
        if not task_id:
            continue
        row = rows_by_task.setdefault(
            task_id,
            {
                "background_task_id": task_id,
                "tool_name": section.get("tool_name") or "",
                "task_kind": section.get("task_kind") or "",
                "started_seq": None,
                "status": "",
                "duration_ms": None,
                "delivery_latency_ms": None,
            },
        )
        row["status"] = str(section.get("status") or event_row.get("event_type", ""))
        duration_ms = section.get("duration_ms")
        if isinstance(duration_ms, (int, float)):
            row["duration_ms"] = float(duration_ms)
        row["tool_name"] = row["tool_name"] or section.get("tool_name") or ""
        row["task_kind"] = row["task_kind"] or section.get("task_kind") or ""
    for event_row in delivered:
        section = _payload_section(event_row, "background_tool")
        task_id = str(section.get("background_task_id") or "")
        if not task_id:
            continue
        row = rows_by_task.get(task_id)
        if row is None:
            continue
        delivery = section.get("delivery_latency_ms")
        if isinstance(delivery, (int, float)):
            row["delivery_latency_ms"] = float(delivery)

    rows = list(rows_by_task.values())
    rows.sort(key=lambda r: (r.get("duration_ms") or 0.0), reverse=True)

    heartbeat_by_task: Counter[str] = Counter()
    for event_row in heartbeats:
        section = _payload_section(event_row, "background_tool")
        task_id = str(section.get("background_task_id") or "")
        if task_id:
            heartbeat_by_task[task_id] += 1

    longest = max(
        rows,
        key=lambda r: float(r.get("duration_ms") or 0.0),
        default=None,
    )
    return {
        "rows": rows,
        "heartbeat": {
            "emitted": sum(heartbeat_by_task.values()),
            "tasks_with_heartbeat": len(heartbeat_by_task),
            "tasks_expected": len(rows_by_task),
        },
        "longest_running": (
            {
                "background_task_id": longest["background_task_id"],
                "tool_name": longest["tool_name"],
                "duration_ms": longest.get("duration_ms"),
            }
            if longest
            else None
        ),
    }


# ---------------------------------------------------------------------------
# §5 Plugin activity (generic by plugin_kind — no vendor names in keys)
# ---------------------------------------------------------------------------


def _section_plugin_activity(
    indexed: Mapping[str, list[Mapping[str, Any]]],
) -> dict[str, Any]:
    invocations: dict[tuple[str, str], dict[str, Any]] = {}
    for event_row in (
        indexed.get("plugin.tool_invoked", [])
        + indexed.get("plugin.tool_completed", [])
    ):
        section = _payload_section(event_row, "plugin")
        plugin_id = str(section.get("plugin_id") or "")
        plugin_kind = str(section.get("plugin_kind") or "custom")
        if plugin_kind not in _ALLOWED_PLUGIN_KINDS:
            plugin_kind = "custom"
        key = (plugin_id, plugin_kind)
        bucket = invocations.setdefault(
            key,
            {
                "plugin_id": plugin_id,
                "plugin_kind": plugin_kind,
                "invocations": 0,
                "_duration_samples": [],
                "peak_resident_bytes": 0,
                "errors": 0,
            },
        )
        if event_row.get("event_type") == "plugin.tool_invoked":
            bucket["invocations"] += 1
        duration_ms = section.get("duration_ms")
        if isinstance(duration_ms, (int, float)):
            bucket["_duration_samples"].append(float(duration_ms))
    for event_row in indexed.get("plugin.error", []):
        section = _payload_section(event_row, "plugin")
        plugin_id = str(section.get("plugin_id") or "")
        plugin_kind = str(section.get("plugin_kind") or "custom")
        if plugin_kind not in _ALLOWED_PLUGIN_KINDS:
            plugin_kind = "custom"
        bucket = invocations.setdefault(
            (plugin_id, plugin_kind),
            {
                "plugin_id": plugin_id,
                "plugin_kind": plugin_kind,
                "invocations": 0,
                "_duration_samples": [],
                "peak_resident_bytes": 0,
                "errors": 0,
            },
        )
        bucket["errors"] += 1
    for event_row in indexed.get("plugin.peak_resident_sampled", []):
        section = _payload_section(event_row, "plugin")
        plugin_id = str(section.get("plugin_id") or "")
        plugin_kind = str(section.get("plugin_kind") or "custom")
        if plugin_kind not in _ALLOWED_PLUGIN_KINDS:
            plugin_kind = "custom"
        bucket = invocations.setdefault(
            (plugin_id, plugin_kind),
            {
                "plugin_id": plugin_id,
                "plugin_kind": plugin_kind,
                "invocations": 0,
                "_duration_samples": [],
                "peak_resident_bytes": 0,
                "errors": 0,
            },
        )
        peak = section.get("peak_resident_bytes")
        if isinstance(peak, (int, float)) and float(peak) > bucket["peak_resident_bytes"]:
            bucket["peak_resident_bytes"] = float(peak)

    rows = []
    for bucket in invocations.values():
        latency = _percentile_record(bucket.pop("_duration_samples"))
        rows.append(
            {
                "plugin_id": bucket["plugin_id"],
                "plugin_kind": bucket["plugin_kind"],
                "invocations": bucket["invocations"],
                "p50_ms": latency.get("p50") or 0.0,
                "p95_ms": latency.get("p95") or 0.0,
                "p99_ms": latency.get("p99") or 0.0,
                "peak_resident_bytes": bucket["peak_resident_bytes"],
                "errors": bucket["errors"],
            }
        )
    rows.sort(
        key=lambda r: (-int(r.get("invocations") or 0), r.get("plugin_id") or "")
    )
    return {"rows": rows, "allowed_plugin_kinds": list(_ALLOWED_PLUGIN_KINDS)}


# ---------------------------------------------------------------------------
# §6 Overlay workspace — ephemeral vs isolated
# ---------------------------------------------------------------------------


def _section_overlay_workspace(
    indexed: Mapping[str, list[Mapping[str, Any]]],
) -> dict[str, Any]:
    """Side-by-side ephemeral vs isolated.

    Ephemeral data comes from ``overlay_workspace.*`` events; isolated data
    comes from ``isolated_workspace.*`` events. Both modes share the
    workspace_mode key so they render in the same table.
    """
    ephemeral_mount = _samples(
        indexed.get("overlay_workspace.mounted", []), "overlay_workspace", "mount_ms"
    )
    ephemeral_cleanup = _samples(
        indexed.get("overlay_workspace.cleaned", []), "overlay_workspace", "cleanup_ms"
    )
    ephemeral_upperdir = _samples(
        indexed.get("overlay_workspace.published", []),
        "overlay_workspace",
        "upperdir_bytes",
    )
    isolated_mount: list[float] = []
    isolated_cleanup: list[float] = []
    isolated_upperdir = _samples(
        indexed.get("isolated_workspace.sampled", []),
        "isolated_workspace",
        "upperdir_bytes",
    )
    ephemeral_changed_paths = sum(
        int(
            _payload_section(row, "overlay_workspace").get("changed_path_count")
            or 0
        )
        for row in indexed.get("overlay_workspace.published", [])
    )
    isolated_changed_paths = sum(
        int(
            _payload_section(row, "isolated_workspace").get("changed_path_count")
            or 0
        )
        for row in indexed.get("isolated_workspace.exited", [])
    )
    lifecycle_distribution: dict[str, dict[str, int]] = {
        "ephemeral": {
            "mounted": len(indexed.get("overlay_workspace.mounted", [])),
            "published": len(indexed.get("overlay_workspace.published", [])),
            "cleaned": len(indexed.get("overlay_workspace.cleaned", [])),
            "cleanup_failed": len(indexed.get("overlay_workspace.cleanup_failed", [])),
        },
        "isolated": {
            "entered": len(indexed.get("isolated_workspace.entered", [])),
            "exited": len(indexed.get("isolated_workspace.exited", [])),
            "evicted": len(indexed.get("isolated_workspace.evicted", [])),
        },
    }
    return {
        "ephemeral": {
            "mount_ms_total": float(sum(ephemeral_mount)),
            "cleanup_ms_total": float(sum(ephemeral_cleanup)),
            "upperdir_bytes": _percentile_record(ephemeral_upperdir),
            "changed_path_count": ephemeral_changed_paths,
            "lifecycle_distribution": lifecycle_distribution["ephemeral"],
        },
        "isolated": {
            "mount_ms_total": float(sum(isolated_mount)),
            "cleanup_ms_total": float(sum(isolated_cleanup)),
            "upperdir_bytes": _percentile_record(isolated_upperdir),
            "changed_path_count": isolated_changed_paths,
            "lifecycle_distribution": lifecycle_distribution["isolated"],
        },
    }


# ---------------------------------------------------------------------------
# §7 LayerStack
# ---------------------------------------------------------------------------


def _section_layer_stack(
    indexed: Mapping[str, list[Mapping[str, Any]]],
) -> dict[str, Any]:
    lease_wait = _samples(
        indexed.get("layer_stack.lease_acquired", []),
        "layer_stack",
        "lease_wait_ms",
    )
    lease_hold = _samples(
        indexed.get("layer_stack.lease_released", []),
        "layer_stack",
        "lease_hold_ms",
    )
    lock_wait = _samples(
        indexed.get("layer_stack.lock_acquired", []),
        "layer_stack",
        "lock_wait_ms",
    )
    manifest_depth: list[int] = []
    for event_row in (
        indexed.get("layer_stack.lease_acquired", [])
        + indexed.get("layer_stack.snapshot_prepared", [])
    ):
        section = _payload_section(event_row, "layer_stack")
        depth = section.get("layer_count")
        if isinstance(depth, (int, float)):
            manifest_depth.append(int(depth))
    return {
        "leases": {
            "count": len(indexed.get("layer_stack.lease_acquired", [])),
            "wait_ms": _percentile_record(lease_wait),
            "hold_ms": _percentile_record(lease_hold),
        },
        "locks": {
            "count": len(indexed.get("layer_stack.lock_acquired", [])),
            "wait_ms": _percentile_record(lock_wait),
        },
        "manifest_depth_series": manifest_depth,
        "squashes": {
            "triggered": len(indexed.get("layer_stack.squash_triggered", [])),
            "completed": len(indexed.get("layer_stack.squash_completed", [])),
            "failed": len(indexed.get("layer_stack.squash_failed", [])),
            "input_layers": [
                int(
                    _payload_section(row, "layer_stack").get(
                        "squash_input_layers"
                    )
                    or 0
                )
                for row in indexed.get("layer_stack.squash_completed", [])
            ],
            "result_layers": [
                int(
                    _payload_section(row, "layer_stack").get(
                        "squash_result_layers"
                    )
                    or 0
                )
                for row in indexed.get("layer_stack.squash_completed", [])
            ],
        },
    }


# ---------------------------------------------------------------------------
# §8 OCC
# ---------------------------------------------------------------------------


def _section_occ(
    indexed: Mapping[str, list[Mapping[str, Any]]],
) -> dict[str, Any]:
    conflict_kinds: Counter[str] = Counter()
    conflict_paths: Counter[str] = Counter()
    for event_row in indexed.get("occ.conflict_rejected", []):
        section = _payload_section(event_row, "occ")
        kind = str(section.get("conflict_kind") or "unknown")
        conflict_kinds[kind] += 1
        path = section.get("conflict_path")
        if path:
            conflict_paths[str(path)] += 1
    return {
        "transactions": {
            "prepared": len(indexed.get("occ.changeset_prepared", [])),
            "committed": len(indexed.get("occ.apply_committed", [])),
            "rejected": len(indexed.get("occ.conflict_rejected", [])),
        },
        "conflicts": {
            "kinds": dict(conflict_kinds),
            "top_paths": conflict_paths.most_common(10),
        },
        "prepare_ms": _percentile_record(
            _samples(
                indexed.get("occ.changeset_prepared", []), "occ", "prepare_ms"
            )
        ),
        "apply_ms": _percentile_record(
            _samples(indexed.get("occ.apply_committed", []), "occ", "apply_ms")
        ),
        "commit_ms": _percentile_record(
            _samples(indexed.get("occ.apply_committed", []), "occ", "commit_ms")
        ),
        "publish_layer_ms": _percentile_record(
            _samples(
                indexed.get("occ.publish_layer", []), "occ", "publish_layer_ms"
            )
        ),
    }


# ---------------------------------------------------------------------------
# §9 Isolated workspace (release-gate surface)
# ---------------------------------------------------------------------------


def _section_isolated_workspace(
    indexed: Mapping[str, list[Mapping[str, Any]]],
) -> dict[str, Any]:
    handles_opened = len(indexed.get("isolated_workspace.entered", []))
    handles_closed = len(indexed.get("isolated_workspace.exited", []))
    handles_evicted = len(indexed.get("isolated_workspace.evicted", []))
    orphan_holder = 0
    orphan_cgroup = 0
    orphan_scratch = 0
    holder_pid_alive_after_exit = 0
    upperdir_samples: list[float] = []
    upperdir_cap_max = 0
    for event_row in indexed.get("isolated_workspace.exited", []):
        section = _payload_section(event_row, "isolated_workspace")
        orphan_holder += int(section.get("orphan_holder_count") or 0)
        orphan_cgroup += int(section.get("orphan_cgroup_count") or 0)
        orphan_scratch += int(section.get("orphan_scratch_count") or 0)
        if bool(section.get("holder_pid_alive")):
            holder_pid_alive_after_exit += 1
    for event_row in indexed.get("isolated_workspace.orphan_check_completed", []):
        section = _payload_section(event_row, "isolated_workspace")
        orphan_holder += int(section.get("orphan_holder_count") or 0)
        orphan_cgroup += int(section.get("orphan_cgroup_count") or 0)
        orphan_scratch += int(section.get("orphan_scratch_count") or 0)
    for event_row in indexed.get("isolated_workspace.sampled", []):
        section = _payload_section(event_row, "isolated_workspace")
        upperdir = section.get("upperdir_bytes")
        if isinstance(upperdir, (int, float)):
            upperdir_samples.append(float(upperdir))
        cap = section.get("upperdir_cap_bytes")
        if isinstance(cap, (int, float)) and int(cap) > upperdir_cap_max:
            upperdir_cap_max = int(cap)
    return {
        "handles": {
            "opened": handles_opened,
            "closed": handles_closed,
            "evicted": handles_evicted,
            "open_handle_count": handles_opened - handles_closed - handles_evicted,
        },
        "upperdir_bytes": _percentile_record(upperdir_samples),
        # Phase 3 deferral D7 — max ``upperdir_cap_bytes`` across sampled
        # exits powers the §13 ``overlay_workspace.upperdir_cap`` warning.
        "upperdir_cap_bytes": upperdir_cap_max,
        "orphan": {
            "orphan_holder_count": orphan_holder,
            "orphan_cgroup_count": orphan_cgroup,
            "orphan_scratch_count": orphan_scratch,
        },
        "holder_pid_alive_after_exit": holder_pid_alive_after_exit,
    }


# ---------------------------------------------------------------------------
# §10 OS resource
# ---------------------------------------------------------------------------


def _section_os_resource(
    indexed: Mapping[str, list[Mapping[str, Any]]],
) -> dict[str, Any]:
    rss_samples: list[int] = []
    cpu_user_samples: list[float] = []
    cpu_system_samples: list[float] = []
    cpu_throttled_samples: list[int] = []
    io_read_bytes_samples: list[int] = []
    io_write_bytes_samples: list[int] = []
    io_read_ops_samples: list[int] = []
    io_write_ops_samples: list[int] = []
    for event_row in indexed.get("os_resource.sampled", []):
        section = _payload_section(event_row, "os_resource")
        rss = section.get("rss_bytes")
        if isinstance(rss, (int, float)):
            rss_samples.append(int(rss))
        cpu_user = section.get("cpu_user_s")
        if isinstance(cpu_user, (int, float)):
            cpu_user_samples.append(float(cpu_user))
        cpu_system = section.get("cpu_system_s")
        if isinstance(cpu_system, (int, float)):
            cpu_system_samples.append(float(cpu_system))
        cpu_throttled = section.get("cpu_throttled_us")
        if isinstance(cpu_throttled, (int, float)):
            cpu_throttled_samples.append(int(cpu_throttled))
        io_rbytes = section.get("io_read_bytes")
        if isinstance(io_rbytes, (int, float)):
            io_read_bytes_samples.append(int(io_rbytes))
        io_wbytes = section.get("io_write_bytes")
        if isinstance(io_wbytes, (int, float)):
            io_write_bytes_samples.append(int(io_wbytes))
        io_rios = section.get("io_read_ops")
        if isinstance(io_rios, (int, float)):
            io_read_ops_samples.append(int(io_rios))
        io_wios = section.get("io_write_ops")
        if isinstance(io_wios, (int, float)):
            io_write_ops_samples.append(int(io_wios))

    def _monotonic_delta_int(samples: list[int]) -> int:
        if len(samples) < 2:
            return 0
        # Monotonic cgroup counters — delta is last minus first.
        return max(0, samples[-1] - samples[0])

    cpu_user_delta = (
        (cpu_user_samples[-1] - cpu_user_samples[0]) if len(cpu_user_samples) >= 2 else 0.0
    )
    cpu_system_delta = (
        (cpu_system_samples[-1] - cpu_system_samples[0])
        if len(cpu_system_samples) >= 2
        else 0.0
    )
    return {
        "cpu": {
            "user_s_delta": cpu_user_delta,
            "system_s_delta": cpu_system_delta,
            "throttled_us_delta": _monotonic_delta_int(cpu_throttled_samples),
        },
        "memory": {
            "rss_peak_bytes": max(rss_samples) if rss_samples else 0,
        },
        "io": {
            "read_bytes": _monotonic_delta_int(io_read_bytes_samples),
            "write_bytes": _monotonic_delta_int(io_write_bytes_samples),
            "read_ops": _monotonic_delta_int(io_read_ops_samples),
            "write_ops": _monotonic_delta_int(io_write_ops_samples),
        },
    }


# ---------------------------------------------------------------------------
# §11 Daemon audit pull
# ---------------------------------------------------------------------------


def _section_daemon_audit_pull(
    puller_stats: Mapping[str, Any] | None,
    indexed: Mapping[str, list[Mapping[str, Any]]],
) -> dict[str, Any]:
    """Materialize the puller-side observability block.

    If ``puller_stats`` is ``None`` the section still renders with zero-
    valued counters so the schema-shape test stays stable; the renderer's
    §11 will note the missing puller in §13 warnings.
    """
    if puller_stats is None:
        puller_stats = {}
    return {
        "pull_count": int(puller_stats.get("pull_count") or 0),
        "empty_pull_count": int(puller_stats.get("empty_pull_count") or 0),
        "events_pulled": int(puller_stats.get("events_pulled") or 0),
        "dropped_event_count": int(puller_stats.get("dropped_event_count") or 0),
        "lost_before_seq": int(puller_stats.get("lost_before_seq") or 0),
        "max_buffer_pressure": float(
            puller_stats.get("max_buffer_pressure") or 0.0
        ),
        "final_cursor": int(puller_stats.get("final_cursor") or -1),
        "floor_raises": int(puller_stats.get("floor_raises") or 0),
        "pull_ms": _ensure_percentile_record(puller_stats.get("pull_ms")),
        "daemon_restarts_observed": int(
            puller_stats.get("daemon_restarts_observed") or 0
        ),
        "puller_attached": bool(puller_stats),
    }


# ---------------------------------------------------------------------------
# §12 Audit path overhead (release gate)
# ---------------------------------------------------------------------------


def _section_overhead(
    overhead_metadata: Mapping[str, Any] | None,
    daemon_audit_pull: Mapping[str, Any],
    artifact_inventory: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Audit overhead measurement payload + gate verdict.

    ``overhead_metadata`` is produced by the release-gate harness
    (paired-run runner). Recognised keys:

    - ``daemon_ring_memory_retained_bytes`` (int)
    - ``daemon_ring_memory_max_bytes`` (int)
    - ``daemon_cpu_pct_p99`` (float)
    - ``runner_cpu_pct_p99`` (float)
    - ``tool_latency_p95_delta_ms`` (float)
    - ``p95_delta_ci_upper`` (float — paired-bootstrap 95% CI upper bound)
    - ``daemon_rss_delta_mib`` (float)
    - ``sandbox_disk_delta_bytes`` (int)
    - ``artifact_disk_live_bytes`` (int)
    - ``artifact_disk_rotated_bytes`` (int)
    - Methodology block: ``n_calls`` (int), ``n_paired_runs`` (int),
      ``warmup_s`` (float), ``bootstrap_resamples`` (int)

    When ``None``, the section renders with the methodology metadata
    keys present but zero-valued so the
    ``test_overhead_gate_methodology_recorded_in_json`` schema assertion
    stays satisfied.
    """
    methodology_present = overhead_metadata is not None
    if overhead_metadata is None:
        overhead_metadata = {}
    daemon_ring_retained = int(
        overhead_metadata.get("daemon_ring_memory_retained_bytes") or 0
    )
    daemon_ring_max = int(
        overhead_metadata.get("daemon_ring_memory_max_bytes") or 0
    )
    daemon_cpu = float(overhead_metadata.get("daemon_cpu_pct_p99") or 0.0)
    runner_cpu = float(overhead_metadata.get("runner_cpu_pct_p99") or 0.0)
    latency_delta = float(
        overhead_metadata.get("tool_latency_p95_delta_ms") or 0.0
    )
    p95_delta_ci_upper = float(
        overhead_metadata.get("p95_delta_ci_upper") or latency_delta
    )
    artifact_disk_total = int(
        overhead_metadata.get("artifact_disk_live_bytes") or 0
    ) + int(overhead_metadata.get("artifact_disk_rotated_bytes") or 0)

    # Methodology metadata (required by test
    # ``test_overhead_gate_methodology_recorded_in_json``). Phase 3
    # deferral D14: ``methodology_present`` distinguishes "no measurement
    # supplied" from "0 paired runs"; the §12 gate cannot pass when
    # missing.
    methodology = {
        "methodology_present": methodology_present,
        "n_calls": int(overhead_metadata.get("n_calls") or 0),
        "n_paired_runs": int(overhead_metadata.get("n_paired_runs") or 0),
        "warmup_s": float(overhead_metadata.get("warmup_s") or 0.0),
        "bootstrap_resamples": int(
            overhead_metadata.get("bootstrap_resamples") or 0
        ),
        "p95_delta_ci_upper": p95_delta_ci_upper,
    }

    # Per V3 §Gate matrix.
    gate_thresholds = {
        "latency_p95_delta_ms": _OVERHEAD_GATE_LATENCY_DELTA_MS,
        "daemon_rss_delta_mib": _OVERHEAD_GATE_DAEMON_RSS_DELTA_MIB,
        "runner_cpu_delta_pct": _OVERHEAD_GATE_RUNNER_CPU_DELTA_PCT,
        "sandbox_disk_delta_bytes": _OVERHEAD_GATE_SANDBOX_DISK_DELTA_BYTES,
    }
    # Phase 3 deferral D9 — wire the artifact-bound gate into §12 so all
    # four V3 release gates surface in the verdict block. When
    # ``artifact_inventory`` is None the verdict reflects the absence by
    # reporting passed=True (no JSONL → 0 bytes ≤ cap is trivially OK).
    from task_center_runner.audit.release_gates import evaluate_artifact_bound_gate

    inventory = artifact_inventory or {
        "live_bytes": 0,
        "rotated_bytes": 0,
        "rotated_file_count": 0,
    }
    artifact_verdict = evaluate_artifact_bound_gate(
        live_bytes=int(inventory.get("live_bytes") or 0),
        rotated_bytes=int(inventory.get("rotated_bytes") or 0),
        rotated_file_count=int(inventory.get("rotated_file_count") or 0),
    )
    gate_verdict = {
        "latency_p95_delta_pass": (
            p95_delta_ci_upper <= _OVERHEAD_GATE_LATENCY_DELTA_MS
        ),
        "runner_cpu_pass": runner_cpu <= _OVERHEAD_GATE_RUNNER_CPU_DELTA_PCT,
        "daemon_cpu_pass": daemon_cpu < 1.0,
        "sandbox_disk_pass": int(
            overhead_metadata.get("sandbox_disk_delta_bytes") or 0
        )
        == 0,
        "artifact_bound_pass": bool(artifact_verdict.get("passed")),
        "puller_attached": bool(daemon_audit_pull.get("puller_attached")),
    }
    return {
        "daemon_ring_memory": {
            "retained_bytes": daemon_ring_retained,
            "max_bytes": daemon_ring_max,
        },
        "daemon_cpu_pct_p99": daemon_cpu,
        "runner_cpu_pct_p99": runner_cpu,
        "tool_latency_p95_delta_ms": latency_delta,
        "artifact_disk_total_bytes": artifact_disk_total,
        "methodology": methodology,
        "artifact_inventory": dict(inventory),
        "gate": {
            "thresholds": gate_thresholds,
            "verdict": gate_verdict,
            "artifact_bound": artifact_verdict,
        },
    }


def _collect_artifact_inventory(run_dir: Path) -> dict[str, int]:
    """Walk ``run_dir`` for ``sandbox_events.jsonl*`` files.

    Returns ``live_bytes`` (the live JSONL size), ``rotated_bytes`` (sum
    over rotated ``.<N>.gz`` files), and ``rotated_file_count`` so
    :func:`evaluate_artifact_bound_gate` can decide whether the host-
    side artifact footprint stayed within ``64 MiB + 8 × rotated``.
    """
    live_bytes = 0
    rotated_bytes = 0
    rotated_file_count = 0
    base = run_dir / "sandbox_events.jsonl"
    try:
        live_bytes = base.stat().st_size if base.exists() else 0
    except OSError:
        live_bytes = 0
    parent = run_dir
    if parent.is_dir():
        prefix = base.name + "."
        for child in parent.iterdir():
            if not child.is_file():
                continue
            if not child.name.startswith(prefix) or not child.name.endswith(".gz"):
                continue
            try:
                rotated_bytes += child.stat().st_size
            except OSError:
                continue
            rotated_file_count += 1
    return {
        "live_bytes": live_bytes,
        "rotated_bytes": rotated_bytes,
        "rotated_file_count": rotated_file_count,
    }


# ---------------------------------------------------------------------------
# §13 Warnings
# ---------------------------------------------------------------------------


def _collect_warnings(
    *,
    summary: Mapping[str, Any],
    daemon_audit_pull: Mapping[str, Any],
    isolated_workspace: Mapping[str, Any],
    overlay_workspace: Mapping[str, Any],
    layer_stack: Mapping[str, Any],
    occ: Mapping[str, Any],
    os_resource: Mapping[str, Any],
    event_count: int,
) -> dict[str, Any]:
    warnings: list[dict[str, Any]] = []
    audit = _as_mapping(summary.get("audit_summary"))
    if int(daemon_audit_pull.get("dropped_event_count") or 0) > 0:
        warnings.append(
            {
                "kind": "audit.dropped",
                "detail": f"daemon dropped {daemon_audit_pull['dropped_event_count']} events",
            }
        )
    pressure = float(
        max(
            audit.get("max_buffer_pressure") or 0.0,
            daemon_audit_pull.get("max_buffer_pressure") or 0.0,
        )
    )
    if pressure > _BUFFER_PRESSURE_WARNING:
        warnings.append(
            {
                "kind": "audit.pressure",
                "detail": f"max buffer pressure {pressure:.2f} > 0.80",
            }
        )
    # Phase 3 deferral D8: divergence between JSONL row count and the
    # puller's events_pulled counter indicates a daemon restart or partial
    # flush; surface it so operators can resolve the §1 vs §11 numbers
    # without source reading.
    events_pulled = int(daemon_audit_pull.get("events_pulled") or 0)
    delta = event_count - events_pulled
    if events_pulled and abs(delta) > 0:
        warnings.append(
            {
                "kind": "audit.events_count_drift",
                "detail": (
                    f"JSONL row count {event_count} vs puller events_pulled "
                    f"{events_pulled} (delta {delta}); likely daemon restart "
                    "or partial-flush — check §11 daemon_restarts_observed"
                ),
            }
        )
    orphan = _as_mapping(isolated_workspace.get("orphan"))
    if (
        int(orphan.get("orphan_holder_count") or 0) > 0
        or int(orphan.get("orphan_cgroup_count") or 0) > 0
        or int(orphan.get("orphan_scratch_count") or 0) > 0
    ):
        warnings.append(
            {
                "kind": "isolated_workspace.gate_failure",
                "detail": (
                    f"orphans detected: "
                    f"holder={orphan.get('orphan_holder_count', 0)} "
                    f"cgroup={orphan.get('orphan_cgroup_count', 0)} "
                    f"scratch={orphan.get('orphan_scratch_count', 0)}"
                ),
            }
        )
    if int(isolated_workspace.get("holder_pid_alive_after_exit") or 0) > 0:
        warnings.append(
            {
                "kind": "isolated_workspace.holder_alive_after_exit",
                "detail": "isolated_workspace exit observed holder_pid_alive=true",
            }
        )
    if int(layer_stack.get("squashes", {}).get("failed") or 0) > 0:
        warnings.append(
            {
                "kind": "layer_stack.squash_failed",
                "detail": "layer_stack.squash_failed observed",
            }
        )
    if int(daemon_audit_pull.get("floor_raises") or 0) > 0:
        warnings.append(
            {
                "kind": "audit.floor_escalated",
                "detail": (
                    f"pull floor escalated {daemon_audit_pull['floor_raises']} "
                    f"times above default {_FLOOR_ESCALATED_DEFAULT_MS} ms"
                ),
            }
        )
    upperdir = _as_mapping(isolated_workspace.get("upperdir_bytes"))
    rss_peak = int(_as_mapping(os_resource.get("memory")).get("rss_peak_bytes") or 0)
    memory_threshold = _memory_peak_warn_bytes()
    if rss_peak and rss_peak > memory_threshold:
        warnings.append(
            {
                "kind": "os_resource.memory_peak",
                "detail": (
                    f"rss peak {rss_peak} bytes (> {memory_threshold} bytes)"
                ),
            }
        )
    upperdir_max = float(upperdir.get("max") or 0.0)
    cap = float(isolated_workspace.get("upperdir_cap_bytes") or 0.0)
    if upperdir_max and cap > 0 and upperdir_max / cap > _UPPERDIR_FRACTION_WARNING:
        warnings.append(
            {
                "kind": "overlay_workspace.upperdir_cap",
                "detail": (
                    f"isolated upperdir {upperdir_max} > "
                    f"{int(_UPPERDIR_FRACTION_WARNING * 100)}% of cap {cap}"
                ),
            }
        )
    occ_conflicts = sum(_as_mapping(occ.get("conflicts", {}).get("kinds")).values())
    if occ_conflicts > 0:
        warnings.append(
            {
                "kind": "occ.conflict_cluster",
                "detail": f"{occ_conflicts} OCC conflicts observed",
            }
        )
    return {"rows": warnings}


def _memory_peak_warn_bytes() -> int:
    """Read ``RunnerConfig.audit_warnings.memory_peak_warn_bytes`` defensively.

    Falls back to the V3 spec default (4 GiB) when central config is
    unavailable (unit-test contexts). Phase 3 deferral D6.
    """
    try:
        from config import get_central_config

        return int(get_central_config().runner.audit_warnings.memory_peak_warn_bytes)
    except Exception:  # noqa: BLE001 — central config is best-effort here
        return 4 * 1024 * 1024 * 1024


# ---------------------------------------------------------------------------
# §X — markdown renderers
# ---------------------------------------------------------------------------


def _render_section_1_summary(sections: Mapping[str, Any]) -> list[str]:
    summary = _as_mapping(sections.get("summary"))
    peak = _as_mapping(summary.get("peak"))
    audit = _as_mapping(summary.get("audit_summary"))
    return [
        "## 1. Summary",
        "",
        f"- duration_total_ms: {summary.get('duration_total_ms', 0):.1f}",
        f"- tools_called: {summary.get('tools_called', 0)}",
        f"- background_tools: {summary.get('background_tools', 0)}",
        f"- sandbox_ops: {summary.get('sandbox_ops', 0)}",
        f"- peak rss_bytes: {int(peak.get('rss_bytes') or 0)}",
        f"- peak upperdir_bytes_total: {int(peak.get('upperdir_bytes_total') or 0)}",
        f"- peak layer_count: {int(peak.get('layer_count') or 0)}",
        f"- audit events_pulled: {int(audit.get('events_pulled') or 0)}",
        f"- audit dropped_event_count: {int(audit.get('dropped_event_count') or 0)}",
        f"- audit max_buffer_pressure: {float(audit.get('max_buffer_pressure') or 0.0):.2f}",
        f"- audit floor_raises: {int(audit.get('floor_raises') or 0)}",
        "",
    ]


def _render_section_2_per_tool_timing(
    sections: Mapping[str, Any],
) -> list[str]:
    timing = _as_mapping(sections.get("per_tool_timing"))
    rows = _as_sequence(timing.get("rows"))
    lines: list[str] = [
        "## 2. Per-tool timing (foreground, split by workspace_mode)",
        "",
        "| tool_name | workspace_mode | calls | queued_ms p50/95/99 | "
        "mount_ms p50/95/99 | exec_ms p50/95/99 | capture_ms p50/95/99 | "
        "publish_ms p50/95/99 | release_ms p50/95/99 | total_ms p50/95/99 |",
        "| --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- |",
    ]
    if not rows:
        lines.append("| (no foreground tool calls recorded) |  |  |  |  |  |  |  |  |  |")
        lines.append("")
        return lines
    for row in rows:
        row_map = _as_mapping(row)
        phases = _as_mapping(row_map.get("phases"))
        total = _as_mapping(row_map.get("total_ms"))
        lines.append(
            "| "
            + " | ".join(
                [
                    str(row_map.get("tool_name") or ""),
                    str(row_map.get("workspace_mode") or ""),
                    str(int(row_map.get("calls") or 0)),
                    _format_phase_cell(_as_mapping(phases.get("queued"))),
                    _format_phase_cell(_as_mapping(phases.get("mount"))),
                    _format_phase_cell(_as_mapping(phases.get("exec"))),
                    _format_phase_cell(_as_mapping(phases.get("capture"))),
                    _format_phase_cell(_as_mapping(phases.get("publish"))),
                    _format_phase_cell(_as_mapping(phases.get("release"))),
                    _format_phase_cell(total),
                ]
            )
            + " |"
        )
    lines.append("")
    return lines


def _render_section_3_per_tool_phase_breakdown(
    sections: Mapping[str, Any],
) -> list[str]:
    breakdown = _as_mapping(sections.get("per_tool_phase_breakdown"))
    rows = _as_sequence(breakdown.get("rows"))
    lines: list[str] = [
        "## 3. Per-tool phase breakdown (top-10 by total_ms)",
        "",
    ]
    if not rows:
        lines.extend(["(no foreground tool calls recorded)", ""])
        return lines
    for row in rows:
        row_map = _as_mapping(row)
        fractions = _as_mapping(row_map.get("phases_fraction"))
        bar = _phase_bar(fractions)
        lines.append(
            f"- `{row_map.get('tool_name')}` "
            f"[{row_map.get('workspace_mode')}] "
            f"total={float(row_map.get('total_ms') or 0.0):.1f} ms : "
            f"{bar}"
        )
    lines.append("")
    return lines


def _render_section_4_background_tool_calls(
    sections: Mapping[str, Any],
) -> list[str]:
    section = _as_mapping(sections.get("background_tool_calls"))
    rows = _as_sequence(section.get("rows"))
    heartbeat = _as_mapping(section.get("heartbeat"))
    longest = _as_mapping(section.get("longest_running"))
    lines: list[str] = [
        "## 4. Background tool calls",
        "",
        "| task_id | tool_name | task_kind | started_seq | duration_ms | "
        "status | delivery_latency_ms |",
        "| --- | --- | --- | ---: | ---: | --- | ---: |",
    ]
    if not rows:
        lines.append("| (no background tool calls recorded) |  |  |  |  |  |  |")
    for row in rows:
        row_map = _as_mapping(row)
        lines.append(
            "| "
            + " | ".join(
                [
                    str(row_map.get("background_task_id") or ""),
                    str(row_map.get("tool_name") or ""),
                    str(row_map.get("task_kind") or ""),
                    str(row_map.get("started_seq") or ""),
                    _fmt_ms_or_dash(row_map.get("duration_ms")),
                    str(row_map.get("status") or ""),
                    _fmt_ms_or_dash(row_map.get("delivery_latency_ms")),
                ]
            )
            + " |"
        )
    emitted = int(heartbeat.get("emitted") or 0)
    expected = int(heartbeat.get("tasks_expected") or 0)
    coverage = (emitted / max(1, expected)) * 100.0
    lines.append("")
    lines.append(
        f"- heartbeat coverage: {emitted} / {expected} = {coverage:.1f} %"
    )
    if longest:
        lines.append(
            f"- longest-running task: `{longest.get('background_task_id')}` "
            f"({longest.get('tool_name')}) duration_ms="
            f"{float(longest.get('duration_ms') or 0.0):.1f}"
        )
    lines.append("")
    return lines


def _render_section_5_plugin_activity(
    sections: Mapping[str, Any],
) -> list[str]:
    section = _as_mapping(sections.get("plugin_activity"))
    rows = _as_sequence(section.get("rows"))
    lines: list[str] = [
        "## 5. Plugin activity (generic; per plugin_id × plugin_kind)",
        "",
        "| plugin_id | plugin_kind | invocations | p50_ms | p95_ms | p99_ms | "
        "peak_resident_bytes | errors |",
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    if not rows:
        lines.append("| (no plugin activity recorded) |  |  |  |  |  |  |  |")
    for row in rows:
        row_map = _as_mapping(row)
        lines.append(
            "| "
            + " | ".join(
                [
                    str(row_map.get("plugin_id") or ""),
                    str(row_map.get("plugin_kind") or ""),
                    str(int(row_map.get("invocations") or 0)),
                    f"{float(row_map.get('p50_ms') or 0.0):.1f}",
                    f"{float(row_map.get('p95_ms') or 0.0):.1f}",
                    f"{float(row_map.get('p99_ms') or 0.0):.1f}",
                    str(int(row_map.get("peak_resident_bytes") or 0)),
                    str(int(row_map.get("errors") or 0)),
                ]
            )
            + " |"
        )
    lines.append("")
    return lines


def _render_section_6_overlay_workspace(
    sections: Mapping[str, Any],
) -> list[str]:
    section = _as_mapping(sections.get("overlay_workspace"))
    ephemeral = _as_mapping(section.get("ephemeral"))
    isolated = _as_mapping(section.get("isolated"))
    eph_upper = _as_mapping(ephemeral.get("upperdir_bytes"))
    iso_upper = _as_mapping(isolated.get("upperdir_bytes"))
    return [
        "## 6. Overlay workspace — ephemeral vs isolated",
        "",
        "| property | ephemeral | isolated |",
        "| --- | ---: | ---: |",
        f"| mount_ms total | {float(ephemeral.get('mount_ms_total') or 0.0):.1f} | "
        f"{float(isolated.get('mount_ms_total') or 0.0):.1f} |",
        f"| cleanup_ms total | {float(ephemeral.get('cleanup_ms_total') or 0.0):.1f} | "
        f"{float(isolated.get('cleanup_ms_total') or 0.0):.1f} |",
        f"| upperdir_bytes p50 | {float(eph_upper.get('p50') or 0.0):.0f} | "
        f"{float(iso_upper.get('p50') or 0.0):.0f} |",
        f"| upperdir_bytes p95 | {float(eph_upper.get('p95') or 0.0):.0f} | "
        f"{float(iso_upper.get('p95') or 0.0):.0f} |",
        f"| upperdir_bytes max | {float(eph_upper.get('max') or 0.0):.0f} | "
        f"{float(iso_upper.get('max') or 0.0):.0f} |",
        f"| changed_path_count | {int(ephemeral.get('changed_path_count') or 0)} | "
        f"{int(isolated.get('changed_path_count') or 0)} |",
        "",
    ]


def _render_section_7_layer_stack(
    sections: Mapping[str, Any],
) -> list[str]:
    section = _as_mapping(sections.get("layer_stack"))
    leases = _as_mapping(section.get("leases"))
    lease_wait = _as_mapping(leases.get("wait_ms"))
    lease_hold = _as_mapping(leases.get("hold_ms"))
    locks = _as_mapping(section.get("locks"))
    lock_wait = _as_mapping(locks.get("wait_ms"))
    depth_series = _as_sequence(section.get("manifest_depth_series"))
    sparkline = _sparkline([int(v or 0) for v in depth_series])
    squashes = _as_mapping(section.get("squashes"))
    return [
        "## 7. LayerStack",
        "",
        f"- leases: count={int(leases.get('count') or 0)} "
        f"wait_ms p50/p95={float(lease_wait.get('p50') or 0.0):.1f}/"
        f"{float(lease_wait.get('p95') or 0.0):.1f} "
        f"hold_ms p50/p95={float(lease_hold.get('p50') or 0.0):.1f}/"
        f"{float(lease_hold.get('p95') or 0.0):.1f}",
        f"- locks: count={int(locks.get('count') or 0)} "
        f"wait_ms p50/p95={float(lock_wait.get('p50') or 0.0):.1f}/"
        f"{float(lock_wait.get('p95') or 0.0):.1f}",
        f"- manifest depth: {sparkline}",
        f"- squashes: triggered={int(squashes.get('triggered') or 0)} "
        f"completed={int(squashes.get('completed') or 0)} "
        f"failed={int(squashes.get('failed') or 0)}",
        "",
    ]


def _render_section_8_occ(
    sections: Mapping[str, Any],
) -> list[str]:
    section = _as_mapping(sections.get("occ"))
    txn = _as_mapping(section.get("transactions"))
    apply_ms = _as_mapping(section.get("apply_ms"))
    commit_ms = _as_mapping(section.get("commit_ms"))
    publish_ms = _as_mapping(section.get("publish_layer_ms"))
    conflict = _as_mapping(section.get("conflicts"))
    kinds = _as_mapping(conflict.get("kinds"))
    return [
        "## 8. OCC",
        "",
        f"- transactions: prepared={int(txn.get('prepared') or 0)} "
        f"committed={int(txn.get('committed') or 0)} "
        f"rejected={int(txn.get('rejected') or 0)}",
        "- conflict matrix: " + (
            ", ".join(f"{k}={v}" for k, v in sorted(kinds.items())) or "(none)"
        ),
        f"- apply_ms p50/p95={float(apply_ms.get('p50') or 0.0):.1f}/"
        f"{float(apply_ms.get('p95') or 0.0):.1f}",
        f"- commit_ms p50/p95={float(commit_ms.get('p50') or 0.0):.1f}/"
        f"{float(commit_ms.get('p95') or 0.0):.1f}",
        f"- publish_layer_ms p50/p95={float(publish_ms.get('p50') or 0.0):.1f}/"
        f"{float(publish_ms.get('p95') or 0.0):.1f}",
        "",
    ]


def _render_section_9_isolated_workspace(
    sections: Mapping[str, Any],
) -> list[str]:
    section = _as_mapping(sections.get("isolated_workspace"))
    handles = _as_mapping(section.get("handles"))
    upperdir = _as_mapping(section.get("upperdir_bytes"))
    orphan = _as_mapping(section.get("orphan"))
    return [
        "## 9. Isolated workspace (release gate surface)",
        "",
        f"- handles: opened={int(handles.get('opened') or 0)} "
        f"closed={int(handles.get('closed') or 0)} "
        f"evicted={int(handles.get('evicted') or 0)} "
        f"open_handle_count={int(handles.get('open_handle_count') or 0)}",
        f"- upperdir_bytes p50/p95/max="
        f"{float(upperdir.get('p50') or 0.0):.0f}/"
        f"{float(upperdir.get('p95') or 0.0):.0f}/"
        f"{float(upperdir.get('max') or 0.0):.0f}",
        f"- orphan counts after exit (MUST be 0): "
        f"holder={int(orphan.get('orphan_holder_count') or 0)} "
        f"cgroup={int(orphan.get('orphan_cgroup_count') or 0)} "
        f"scratch={int(orphan.get('orphan_scratch_count') or 0)}",
        f"- holder_pid_alive after exit (MUST be false): "
        f"observed_alive={int(section.get('holder_pid_alive_after_exit') or 0)}",
        "",
    ]


def _render_section_10_os_resource(
    sections: Mapping[str, Any],
) -> list[str]:
    section = _as_mapping(sections.get("os_resource"))
    cpu = _as_mapping(section.get("cpu"))
    memory = _as_mapping(section.get("memory"))
    io = _as_mapping(section.get("io"))
    return [
        "## 10. OS resource (process / cgroup)",
        "",
        f"- CPU user_s_delta={float(cpu.get('user_s_delta') or 0.0):.3f} "
        f"system_s_delta={float(cpu.get('system_s_delta') or 0.0):.3f} "
        f"throttled_us_delta={int(cpu.get('throttled_us_delta') or 0)}",
        f"- Memory rss_peak_bytes={int(memory.get('rss_peak_bytes') or 0)}",
        f"- IO read_bytes={int(io.get('read_bytes') or 0)} "
        f"write_bytes={int(io.get('write_bytes') or 0)} "
        f"read_ops={int(io.get('read_ops') or 0)} "
        f"write_ops={int(io.get('write_ops') or 0)}",
        "",
    ]


def _render_section_11_daemon_audit_pull(
    sections: Mapping[str, Any],
) -> list[str]:
    section = _as_mapping(sections.get("daemon_audit_pull"))
    pull_ms = _as_mapping(section.get("pull_ms"))
    return [
        "## 11. Daemon audit pull",
        "",
        f"- pull_count={int(section.get('pull_count') or 0)} "
        f"empty_pull_count={int(section.get('empty_pull_count') or 0)} "
        f"events_pulled={int(section.get('events_pulled') or 0)}",
        f"- dropped_event_count={int(section.get('dropped_event_count') or 0)} "
        f"lost_before_seq={int(section.get('lost_before_seq') or 0)}",
        f"- max_buffer_pressure={float(section.get('max_buffer_pressure') or 0.0):.2f} "
        f"final_cursor={int(section.get('final_cursor') or -1)}",
        f"- floor_raises={int(section.get('floor_raises') or 0)}",
        f"- pull_ms p50/p95/p99={float(pull_ms.get('p50') or 0.0):.1f}/"
        f"{float(pull_ms.get('p95') or 0.0):.1f}/"
        f"{float(pull_ms.get('p99') or 0.0):.1f}",
        f"- daemon_restarts_observed={int(section.get('daemon_restarts_observed') or 0)}",
        f"- puller_attached={bool(section.get('puller_attached'))}",
        "",
    ]


def _render_section_12_overhead(
    sections: Mapping[str, Any],
) -> list[str]:
    section = _as_mapping(sections.get("overhead"))
    daemon_ring = _as_mapping(section.get("daemon_ring_memory"))
    methodology = _as_mapping(section.get("methodology"))
    gate = _as_mapping(section.get("gate"))
    verdict = _as_mapping(gate.get("verdict"))
    return [
        "## 12. Audit path overhead (release gate)",
        "",
        f"- daemon ring memory: retained={int(daemon_ring.get('retained_bytes') or 0)} / "
        f"max={int(daemon_ring.get('max_bytes') or 0)}",
        f"- daemon CPU attributable to audit p99: "
        f"{float(section.get('daemon_cpu_pct_p99') or 0.0):.3f}% "
        f"(gate < 1.000%)",
        f"- runner CPU attributable to puller p99: "
        f"{float(section.get('runner_cpu_pct_p99') or 0.0):.3f}% "
        f"(gate < 0.500%)",
        f"- tool-call wall-time p95 delta: "
        f"{float(section.get('tool_latency_p95_delta_ms') or 0.0):.2f} ms "
        f"(gate <= 5.00 ms, CI upper {float(methodology.get('p95_delta_ci_upper') or 0.0):.2f})",
        f"- artifact disk total: {int(section.get('artifact_disk_total_bytes') or 0)} bytes",
        f"- methodology: n_calls={int(methodology.get('n_calls') or 0)} "
        f"n_paired_runs={int(methodology.get('n_paired_runs') or 0)} "
        f"warmup_s={float(methodology.get('warmup_s') or 0.0):.1f} "
        f"bootstrap_resamples={int(methodology.get('bootstrap_resamples') or 0)}",
        f"- gate verdict: latency_p95_delta_pass={bool(verdict.get('latency_p95_delta_pass'))} "
        f"runner_cpu_pass={bool(verdict.get('runner_cpu_pass'))} "
        f"daemon_cpu_pass={bool(verdict.get('daemon_cpu_pass'))} "
        f"sandbox_disk_pass={bool(verdict.get('sandbox_disk_pass'))} "
        f"artifact_bound_pass={bool(verdict.get('artifact_bound_pass'))}",
        "",
    ]


def _render_section_13_warnings(
    sections: Mapping[str, Any],
) -> list[str]:
    warnings = _as_sequence(_as_mapping(sections.get("warnings")).get("rows"))
    lines: list[str] = ["## 13. Warnings", ""]
    if not warnings:
        lines.extend(["- (none)", ""])
    else:
        for warning in warnings:
            warning_map = _as_mapping(warning)
            lines.append(
                f"- [{warning_map.get('kind') or ''}] "
                f"{warning_map.get('detail') or ''}"
            )
        lines.append("")
    forensic = _as_sequence(_as_mapping(sections.get("forensic_deltas")).get("rows"))
    if forensic:
        # Phase 3 deferral D15: an opt-in (gated by
        # ``EOS_AUDIT_FORENSIC_RAW_ENABLED=true``) block showing
        # (seq, key, promoted_value, daemon_event_value) drift between
        # promoted sections and the daemon_event forensic raw. Helps
        # diagnose "report looks wrong" cases.
        lines.append("### 13.1 Forensic-raw drift (debug-mode)")
        lines.append("")
        lines.append("| seq | key | promoted | daemon_event |")
        lines.append("| ---: | --- | --- | --- |")
        for row in forensic:
            row_map = _as_mapping(row)
            lines.append(
                "| "
                + " | ".join(
                    [
                        str(row_map.get("seq") or ""),
                        str(row_map.get("key") or ""),
                        repr(row_map.get("promoted_value")),
                        repr(row_map.get("daemon_event_value")),
                    ]
                )
                + " |"
            )
        lines.append("")
    return lines


def _collect_forensic_deltas(
    rows: Sequence[Mapping[str, Any]],
) -> dict[str, Any] | None:
    """Phase 3 deferral D15 — opt-in forensic-raw delta surfacer.

    Delegates to the normalizer module so the daemon-event boundary stays
    enforced (only :mod:`daemon_event_normalizer` may read the forensic
    raw field). Returns ``None`` unless
    ``EOS_AUDIT_FORENSIC_RAW_ENABLED=true``.
    """
    from task_center_runner.audit.daemon_event_normalizer import (
        collect_forensic_deltas,
    )

    return collect_forensic_deltas(rows)


# ---------------------------------------------------------------------------
# Indexing + payload helpers — these are the ONLY readers of
# ``payload["<section>"]``. They never touch ``payload.daemon_event``.
# ---------------------------------------------------------------------------


def _index_rows_by_event_type(
    rows: Sequence[Mapping[str, Any]],
) -> dict[str, list[Mapping[str, Any]]]:
    index: dict[str, list[Mapping[str, Any]]] = defaultdict(list)
    for row in rows:
        event_type = row.get("event_type")
        if isinstance(event_type, str):
            index[event_type].append(row)
    return index


def _payload_section(
    row: Mapping[str, Any], section_key: str
) -> Mapping[str, Any]:
    payload = row.get("payload")
    if not isinstance(payload, Mapping):
        return {}
    section = payload.get(section_key)
    return section if isinstance(section, Mapping) else {}


def _samples(
    rows: Iterable[Mapping[str, Any]],
    section_key: str,
    field_name: str,
) -> list[float]:
    out: list[float] = []
    for row in rows:
        value = _payload_section(row, section_key).get(field_name)
        if isinstance(value, (int, float)):
            out.append(float(value))
    return out


def _peak_int(
    rows: Iterable[Mapping[str, Any]],
    field_name: str,
    *,
    section_key: str | None = None,
) -> int:
    """Highest integer value of ``field_name`` across the rows.

    For rows without an explicit ``section_key`` (e.g. ``os_resource``),
    auto-detect by walking ``payload`` looking for the field.
    """
    peak = 0
    for row in rows:
        payload = row.get("payload") if isinstance(row.get("payload"), Mapping) else {}
        if section_key is not None:
            value = _payload_section(row, section_key).get(field_name)
        else:
            value = None
            for section_value in payload.values():
                if isinstance(section_value, Mapping):
                    candidate = section_value.get(field_name)
                    if isinstance(candidate, (int, float)):
                        value = candidate
                        break
        if isinstance(value, (int, float)) and int(value) > peak:
            peak = int(value)
    return peak


def _tool_call_total_ms(section: Mapping[str, Any]) -> float | None:
    total = section.get("total_ms")
    if isinstance(total, (int, float)):
        return float(total)
    duration = section.get("duration_ms")
    if isinstance(duration, (int, float)):
        return float(duration)
    return None


# ---------------------------------------------------------------------------
# Stats + formatting
# ---------------------------------------------------------------------------


def _percentile_record(values: Sequence[float]) -> dict[str, float | int]:
    if not values:
        return {"count": 0, "p50": 0.0, "p95": 0.0, "p99": 0.0, "max": 0.0}
    ordered = sorted(float(v) for v in values)
    return {
        "count": len(ordered),
        "p50": float(median(ordered)),
        "p95": _percentile(ordered, 95.0),
        "p99": _percentile(ordered, 99.0),
        "max": ordered[-1],
    }


def _ensure_percentile_record(value: object) -> dict[str, float | int]:
    if isinstance(value, Mapping):
        return {
            "count": int(value.get("count") or 0),
            "p50": float(value.get("p50") or 0.0),
            "p95": float(value.get("p95") or 0.0),
            "p99": float(value.get("p99") or 0.0),
            "max": float(value.get("max") or 0.0),
        }
    return _percentile_record([])


def _format_phase_cell(percentiles: Mapping[str, Any]) -> str:
    """`"p50/p95/p99"` ms, or `"—"` when no samples were recorded.

    The dash here is by-design: per FU#5, the framework does not yet
    record ``mount`` / ``publish`` from overlay/OCC.
    """
    count = int(percentiles.get("count") or 0)
    if count == 0:
        return "—"
    return (
        f"{float(percentiles.get('p50') or 0.0):.1f}/"
        f"{float(percentiles.get('p95') or 0.0):.1f}/"
        f"{float(percentiles.get('p99') or 0.0):.1f}"
    )


def _fmt_ms_or_dash(value: object) -> str:
    if value is None:
        return "—"
    try:
        return f"{float(value):.1f}"
    except (TypeError, ValueError):
        return "—"


def _phase_bar(fractions: Mapping[str, Any], width: int = 40) -> str:
    """Render the §3 ASCII bar from a per-phase fraction map.

    Phase 3 deferral D10: fractions are renormalised so they always sum
    to ≤ 1 before glyph allocation. Without normalisation the bar would
    silently lose its rightmost glyphs whenever phase totals overlap
    (e.g. mount and publish recorded inside exec).
    """
    glyph_map = {
        "queued": "Q",
        "mount": "M",
        "exec": "E",
        "capture": "C",
        "publish": "P",
        "release": "R",
    }
    raw_fractions: dict[str, float] = {}
    for phase in _PHASE_ORDER:
        try:
            raw_fractions[phase] = max(0.0, float(fractions.get(phase) or 0.0))
        except (TypeError, ValueError):
            raw_fractions[phase] = 0.0
    total = sum(raw_fractions.values())
    if total > 1.0:
        scale = 1.0 / total
        normalized = {phase: value * scale for phase, value in raw_fractions.items()}
    else:
        normalized = raw_fractions
    parts: list[str] = []
    for phase in _PHASE_ORDER:
        cells = max(0, round(normalized[phase] * width))
        parts.append(glyph_map[phase] * cells)
    bar = "".join(parts) or "·"
    if len(bar) > width:
        bar = bar[:width]
    return bar


def _sparkline(values: Sequence[int]) -> str:
    if not values:
        return "(no samples)"
    glyphs = "▁▂▃▄▅▆▇█"
    lo, hi = min(values), max(values)
    span = max(1, hi - lo)
    return "".join(glyphs[min(7, int((v - lo) / span * 7))] for v in values[:64])


def _percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    rank = max(1, int(round(pct / 100.0 * len(values))))
    return float(values[min(rank, len(values)) - 1])


# ---------------------------------------------------------------------------
# Legacy v2 sandbox report (preserved for back-compat consumers)
# ---------------------------------------------------------------------------


def _build_totals(
    tool_report: Mapping[str, Any],
    sandbox_report: Mapping[str, Any],
) -> dict[str, Any]:
    per_tool = _as_mapping(tool_report.get("per_tool"))
    total_ms = 0.0
    for item in per_tool.values():
        total_ms += float(_as_mapping(item).get("total_ms") or 0.0)
    return {
        "tool_calls_total": int(tool_report.get("tool_calls_total") or 0),
        "tool_errors_total": int(tool_report.get("tool_errors_total") or 0),
        "tool_latency_total_ms": total_ms,
        "sandbox_event_count": int(sandbox_report.get("event_count") or 0),
        "sandbox_duration_total_s": float(
            sandbox_report.get("duration_total_s") or 0.0
        ),
        "incomplete_tool_calls": len(_as_sequence(tool_report.get("incomplete_calls"))),
    }


def _build_legacy_sandbox_report(rows: list[dict[str, Any]]) -> dict[str, Any]:
    """Legacy ``sandbox.families`` / ``timing_keys`` / ``resource_keys`` rollup.

    Retained so dashboards that consumed the v2 perf-report keep working
    without a coordinated cutover. V3 §1-§13 do NOT read this block.
    """
    family_events: dict[str, list[dict[str, Any]]] = {}
    timing_values: dict[str, list[float]] = {}
    non_duration_values: dict[str, list[float]] = {}
    latest_non_duration_values: dict[str, float] = {}
    resource_values: dict[str, list[float]] = {}
    latest_resource_values: dict[str, float] = {}
    detailed_events: list[dict[str, Any]] = []
    event_type_counts: Counter[str] = Counter()
    tool_counts: Counter[str] = Counter()
    status_counts: Counter[str] = Counter()
    conflict_events: list[dict[str, Any]] = []

    for row in rows:
        event = _normalize_sandbox_event(row)
        family_events.setdefault(event["family"], []).append(event)
        detailed_events.append(event)
        event_type_counts.update([event["event_type"]])
        if event["tool_name"]:
            tool_counts.update([event["tool_name"]])
        if event["status"]:
            status_counts.update([event["status"]])
        if event["conflict_reason"] or event["event_type"] == "sandbox_conflict_detected":
            conflict_events.append(event)
        for key, value in _as_mapping(event.get("timings")).items():
            key_text = str(key)
            if (
                key_text.startswith("resource.")
                and event["event_type"] != "sandbox_resource_snapshot"
            ):
                continue
            try:
                number = float(value)
            except (TypeError, ValueError):
                continue
            if _looks_like_duration(key_text):
                timing_values.setdefault(key_text, []).append(number)
            else:
                non_duration_values.setdefault(key_text, []).append(number)
                latest_non_duration_values[key_text] = number
                if (
                    event["event_type"] == "sandbox_resource_snapshot"
                    and key_text.startswith("resource.")
                ):
                    resource_values.setdefault(key_text, []).append(number)
                    latest_resource_values[key_text] = number

    families = {
        family: _build_legacy_family_report(events)
        for family, events in sorted(family_events.items())
    }
    duration_total = sum(float(item["duration_s"]["total"]) for item in families.values())
    non_duration_observations = {
        key: _stats_legacy(values) for key, values in sorted(non_duration_values.items())
    }
    for key, latest in latest_non_duration_values.items():
        if key in non_duration_observations:
            non_duration_observations[key]["latest"] = latest
    resource_keys = {
        key: _resource_stats_legacy(key, values)
        for key, values in sorted(resource_values.items())
    }
    for key, latest in latest_resource_values.items():
        if key in resource_keys and key not in _RUN_DELTA_RESOURCE_KEYS:
            resource_keys[key]["latest"] = latest
    return {
        "event_count": len(rows),
        "duration_total_s": duration_total,
        "families": families,
        "timing_keys": {
            key: _stats_legacy(values) for key, values in sorted(timing_values.items())
        },
        "non_duration_observations": non_duration_observations,
        "resource_keys": resource_keys,
        "event_type_counts": dict(sorted(event_type_counts.items())),
        "tool_counts": dict(sorted(tool_counts.items())),
        "status_counts": dict(sorted(status_counts.items())),
        "conflict_events": conflict_events,
        "slowest_events": _slowest_sandbox_events(detailed_events),
        "events": detailed_events,
    }


def _normalize_sandbox_event(row: Mapping[str, Any]) -> dict[str, Any]:
    payload = _as_mapping(row.get("payload"))
    node = _as_mapping(row.get("node"))
    timings = _float_mapping(payload.get("timings"))
    event_type = str(row.get("event_type") or "sandbox_unknown")
    duration_total = sum(
        value
        for key, value in timings.items()
        if _looks_like_duration(key)
        and (
            not str(key).startswith("resource.")
            or event_type == "sandbox_resource_snapshot"
        )
    )
    changed_paths = _string_list(payload.get("changed_paths"))
    return {
        "ts": row.get("ts"),
        "event_type": event_type,
        "family": _SANDBOX_FAMILY_BY_EVENT.get(event_type, "sandbox_other"),
        "tool_name": payload.get("tool_name") or node.get("tool_name"),
        "tool_id": payload.get("tool_id"),
        "agent_name": node.get("agent_name"),
        "agent_run_id": node.get("agent_run_id"),
        "task_center_run_id": node.get("task_center_run_id"),
        "status": payload.get("status"),
        "conflict_reason": payload.get("conflict_reason"),
        "changed_paths": changed_paths,
        "changed_path_count": len(changed_paths),
        "timings": timings,
        "duration_s_total": duration_total,
        "correlation_id": row.get("correlation_id"),
    }


def _build_legacy_family_report(events: list[dict[str, Any]]) -> dict[str, Any]:
    event_types: Counter[str] = Counter()
    tools: Counter[str] = Counter()
    statuses: Counter[str] = Counter()
    timing_values: dict[str, list[float]] = {}
    duration_values: list[float] = []
    changed_paths_total = 0
    conflict_count = 0

    for event in events:
        event_types.update([str(event.get("event_type") or "")])
        tool_name = event.get("tool_name")
        if tool_name:
            tools.update([str(tool_name)])
        status = event.get("status")
        if status:
            statuses.update([str(status)])
        if event.get("conflict_reason") or event.get("event_type") == (
            "sandbox_conflict_detected"
        ):
            conflict_count += 1
        changed_paths_total += int(event.get("changed_path_count") or 0)
        duration_values.append(float(event.get("duration_s_total") or 0.0))
        for key, value in _as_mapping(event.get("timings")).items():
            try:
                timing_values.setdefault(str(key), []).append(float(value))
            except (TypeError, ValueError):
                continue

    return {
        "event_count": len(events),
        "duration_s": _stats_legacy(duration_values),
        "event_type_counts": dict(sorted(event_types.items())),
        "tool_counts": dict(sorted(tools.items())),
        "status_counts": dict(sorted(statuses.items())),
        "changed_paths_total": changed_paths_total,
        "conflict_count": conflict_count,
        "timing_keys": {
            key: _stats_legacy(values) for key, values in sorted(timing_values.items())
        },
        "slowest_events": _slowest_sandbox_events(events),
    }


def _build_hotspots(
    tool_report: Mapping[str, Any],
    sandbox_report: Mapping[str, Any],
) -> dict[str, Any]:
    per_tool = _as_mapping(tool_report.get("per_tool"))
    slowest_timing_keys = sorted(
        _as_mapping(sandbox_report.get("timing_keys")).items(),
        key=lambda item: float(_as_mapping(item[1]).get("total") or 0.0),
        reverse=True,
    )[:_SLOWEST_LIMIT]
    tool_rank = [
        {
            "tool_name": name,
            "count": _as_mapping(item).get("count", 0),
            "errors": _as_mapping(item).get("errors", 0),
            "total_ms": _as_mapping(item).get("total_ms", 0.0),
            "p95_ms": _as_mapping(item).get("p95_ms", 0.0),
        }
        for name, item in sorted(
            per_tool.items(),
            key=lambda kv: float(_as_mapping(kv[1]).get("total_ms") or 0.0),
            reverse=True,
        )
    ]
    family_rank = [
        {
            "family": family,
            "event_count": _as_mapping(item).get("event_count", 0),
            "duration_s_total": _as_mapping(
                _as_mapping(item).get("duration_s")
            ).get("total", 0.0),
            "p95_s": _as_mapping(_as_mapping(item).get("duration_s")).get("p95", 0.0),
        }
        for family, item in sorted(
            _as_mapping(sandbox_report.get("families")).items(),
            key=lambda kv: float(
                _as_mapping(_as_mapping(kv[1]).get("duration_s")).get("total") or 0.0
            ),
            reverse=True,
        )
    ]
    return {
        "tool_rank_by_total_ms": tool_rank,
        "sandbox_family_rank_by_total_s": family_rank,
        "slowest_tool_calls": _as_sequence(tool_report.get("slowest_calls")),
        "slowest_sandbox_events": _as_sequence(sandbox_report.get("slowest_events")),
        "slowest_sandbox_timing_keys": [
            {"timing_key": key, **_as_mapping(value)}
            for key, value in slowest_timing_keys
        ],
    }


def _stats_legacy(values: Iterable[float]) -> dict[str, float | int]:
    ordered = sorted(float(value) for value in values)
    if not ordered:
        return {
            "count": 0,
            "total": 0.0,
            "min": 0.0,
            "mean": 0.0,
            "p50": 0.0,
            "p75": 0.0,
            "p90": 0.0,
            "p95": 0.0,
            "p99": 0.0,
            "max": 0.0,
        }
    total = float(sum(ordered))
    return {
        "count": len(ordered),
        "total": total,
        "min": ordered[0],
        "mean": total / float(len(ordered)),
        "p50": float(median(ordered)),
        "p75": _percentile(ordered, 75.0),
        "p90": _percentile(ordered, 90.0),
        "p95": _percentile(ordered, 95.0),
        "p99": _percentile(ordered, 99.0),
        "max": ordered[-1],
    }


def _resource_stats_legacy(key: str, values: Iterable[float]) -> dict[str, Any]:
    ordered = [float(value) for value in values]
    if key not in _RUN_DELTA_RESOURCE_KEYS:
        return _stats_legacy(ordered)
    if not ordered:
        stats = _stats_legacy(())
        stats["source"] = "run_delta"
        stats["first_lifetime"] = 0.0
        stats["latest_lifetime"] = 0.0
        stats["latest"] = 0.0
        return stats
    first = ordered[0]
    deltas = [max(0.0, value - first) for value in ordered]
    stats = _stats_legacy(deltas)
    stats["source"] = "run_delta"
    stats["first_lifetime"] = first
    stats["latest_lifetime"] = ordered[-1]
    stats["latest"] = deltas[-1]
    return stats


def _slowest_sandbox_events(
    events: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    return sorted(
        events,
        key=lambda item: float(item.get("duration_s_total") or -1.0),
        reverse=True,
    )[:_SLOWEST_LIMIT]


def _looks_like_duration(key: object) -> bool:
    text = str(key)
    return text.endswith(("_s", ".total_s", ".s"))


def _float_mapping(value: object) -> dict[str, float]:
    if not isinstance(value, dict):
        return {}
    result: dict[str, float] = {}
    for key, raw in value.items():
        if not isinstance(key, str):
            continue
        try:
            result[key] = float(raw)
        except (TypeError, ValueError):
            continue
    return result


def _string_list(value: object) -> list[str]:
    if not isinstance(value, (list, tuple, set)):
        return []
    return [str(item) for item in value if str(item or "").strip()]


def _as_mapping(value: object) -> Mapping[str, Any]:
    return value if isinstance(value, dict) else {}


def _as_sequence(value: object) -> list[Any]:
    return list(value) if isinstance(value, list) else []


# ---------------------------------------------------------------------------
# IO helpers
# ---------------------------------------------------------------------------


def _iter_jsonl(path: Path) -> Iterable[dict[str, Any]]:
    """Concatenate the live JSONL file with any ``.<N>.gz`` rotated history."""
    from task_center_runner.audit.sandbox_events_sink import iter_rotated_jsonl

    yield from iter_rotated_jsonl(path)


def _read_json(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {}
    return value if isinstance(value, dict) else {}


_atomic_write_json = atomic_write_pretty_json
_atomic_write_text = atomic_write_text


async def _write_perf_report_safe(
    run_dir: Path,
    snapshot: Mapping[str, Any],
    *,
    daemon_audit_puller_stats: Mapping[str, Any] | None = None,
    overhead_metadata: Mapping[str, Any] | None = None,
) -> Path:
    """Run :func:`write_performance_reports` off the event loop.

    Failures are logged with ``exc_info=True`` and never propagate — perf reports
    are observability, not correctness. Returns the expected report path so the
    caller can ``await`` the task and check existence; if the write failed, the
    file may not exist.
    """
    try:
        await asyncio.to_thread(
            write_performance_reports,
            run_dir,
            snapshot,
            daemon_audit_puller_stats=daemon_audit_puller_stats,
            overhead_metadata=overhead_metadata,
        )
    except BaseException as exc:  # noqa: BLE001 — never crash the run on perf-report failures
        logger.warning(
            "Async perf-report failed for %s: %s", run_dir, exc, exc_info=True
        )
    return run_dir / "performance_report.json"


__all__ = [
    "REPORT_SCHEMA",
    "_write_perf_report_safe",
    "build_performance_report",
    "render_performance_report_markdown",
    "write_performance_reports",
]
