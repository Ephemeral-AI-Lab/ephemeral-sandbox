"""Phase 3 implementation deferrals — verification tests.

Each test pins the behaviour described in
``docs/daemon-audit-pull-consolidation-v3/phase-3-implementation-deferrals.md``
§D1-§D16. Synthetic event rows mirror the wire payload shape so the
section builders can be exercised without spinning up the daemon.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from task_center_runner.audit.daemon_event_normalizer import FORENSIC_RAW_ENV
from task_center_runner.audit.performance_report import (
    _collect_artifact_inventory,
    _phase_bar,
    build_performance_report,
    render_performance_report_markdown,
)
from task_center_runner.audit.release_gates import evaluate_audit_overhead_gate


def _write_jsonl(path: Path, rows: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        for row in rows:
            handle.write(json.dumps(row) + "\n")


def _empty_tool_performance() -> dict[str, Any]:
    return {
        "tool_calls_total": 0,
        "tool_errors_total": 0,
        "per_tool": {},
        "slowest_calls": [],
    }


# ---------------------------------------------------------------------------
# D1 — mount/publish phase columns populate from phase_totals_rollup
# ---------------------------------------------------------------------------


def test_d1_mount_and_publish_phases_populate_when_emitted(tmp_path: Path) -> None:
    rows = [
        {
            "event_type": "tool_call.finished",
            "schema": "sandbox.daemon.audit.pull.v1",
            "lane": "normal",
            "seq": 1,
            "payload": {
                "tool_call": {
                    "tool_id": "t1",
                    "tool_name": "write_file",
                    "workspace_mode": "ephemeral",
                    "total_ms": 50.0,
                    "phase_totals_rollup": {
                        "queued_ms": 1.0,
                        "mount_ms": 12.5,
                        "exec_ms": 30.0,
                        "capture_ms": 1.5,
                        "publish_ms": 4.0,
                        "release_ms": 1.0,
                    },
                }
            },
        }
    ]
    _write_jsonl(tmp_path / "sandbox_events.jsonl", rows)
    report = build_performance_report(tmp_path, _empty_tool_performance())
    timing_rows = report["sandbox"]["sections"]["per_tool_timing"]["rows"]
    row = next(r for r in timing_rows if r["tool_name"] == "write_file")
    assert row["phases"]["mount"]["count"] == 1
    assert row["phases"]["mount"]["p50"] == pytest.approx(12.5)
    assert row["phases"]["publish"]["count"] == 1
    assert row["phases"]["publish"]["p50"] == pytest.approx(4.0)

    md = render_performance_report_markdown(report)
    timing_line = next(
        line for line in md.splitlines() if line.startswith("| write_file")
    )
    # mount/publish columns must render numerics, not "—".
    assert "—" not in timing_line


# ---------------------------------------------------------------------------
# D2 — occ.prepare_ms feeds §8 prepare_ms percentile
# ---------------------------------------------------------------------------


def test_d2_occ_prepare_ms_populates_percentile(tmp_path: Path) -> None:
    rows = [
        {
            "event_type": "occ.changeset_prepared",
            "schema": "sandbox.daemon.audit.pull.v1",
            "lane": "normal",
            "seq": i,
            "payload": {
                "occ": {
                    "changeset_id": f"c{i}",
                    "prepare_ms": 12.5,
                }
            },
        }
        for i in range(1, 4)
    ]
    _write_jsonl(tmp_path / "sandbox_events.jsonl", rows)
    report = build_performance_report(tmp_path, _empty_tool_performance())
    prepare_ms = report["sandbox"]["sections"]["occ"]["prepare_ms"]
    assert prepare_ms["count"] == 3
    assert prepare_ms["p50"] == pytest.approx(12.5)


# ---------------------------------------------------------------------------
# D3 — cgroup IO + CPU-throttle deltas surface in §10
# ---------------------------------------------------------------------------


def test_d3_os_resource_io_and_throttle_delta(tmp_path: Path) -> None:
    rows = [
        {
            "event_type": "os_resource.sampled",
            "schema": "sandbox.daemon.audit.pull.v1",
            "lane": "sample",
            "seq": 1,
            "payload": {
                "os_resource": {
                    "sampled_at_monotonic_s": 0.0,
                    "rss_bytes": 100,
                    "cpu_throttled_us": 1000,
                    "io_read_bytes": 0,
                    "io_write_bytes": 0,
                    "io_read_ops": 0,
                    "io_write_ops": 0,
                }
            },
        },
        {
            "event_type": "os_resource.sampled",
            "schema": "sandbox.daemon.audit.pull.v1",
            "lane": "sample",
            "seq": 2,
            "payload": {
                "os_resource": {
                    "sampled_at_monotonic_s": 1.0,
                    "rss_bytes": 200,
                    "cpu_throttled_us": 4500,
                    "io_read_bytes": 65536,
                    "io_write_bytes": 8192,
                    "io_read_ops": 17,
                    "io_write_ops": 3,
                }
            },
        },
    ]
    _write_jsonl(tmp_path / "sandbox_events.jsonl", rows)
    report = build_performance_report(tmp_path, _empty_tool_performance())
    os_resource = report["sandbox"]["sections"]["os_resource"]
    assert os_resource["cpu"]["throttled_us_delta"] == 3500
    assert os_resource["io"]["read_bytes"] == 65536
    assert os_resource["io"]["write_bytes"] == 8192
    assert os_resource["io"]["read_ops"] == 17
    assert os_resource["io"]["write_ops"] == 3


# ---------------------------------------------------------------------------
# D4 — ephemeral upperdir_bytes percentile records
# ---------------------------------------------------------------------------


def test_d4_ephemeral_upperdir_bytes_populates(tmp_path: Path) -> None:
    rows = [
        {
            "event_type": "overlay_workspace.published",
            "schema": "sandbox.daemon.audit.pull.v1",
            "lane": "critical",
            "seq": i,
            "payload": {
                "overlay_workspace": {
                    "workspace_handle_id": f"h{i}",
                    "publish_layer_ms": 1.0,
                    "upperdir_bytes": value,
                }
            },
        }
        for i, value in enumerate((1024, 2048, 8192), start=1)
    ]
    _write_jsonl(tmp_path / "sandbox_events.jsonl", rows)
    report = build_performance_report(tmp_path, _empty_tool_performance())
    ephemeral = report["sandbox"]["sections"]["overlay_workspace"]["ephemeral"]
    assert ephemeral["upperdir_bytes"]["count"] == 3
    assert ephemeral["upperdir_bytes"]["max"] == 8192


# ---------------------------------------------------------------------------
# D5 — §4 column header reads ``started_seq``
# ---------------------------------------------------------------------------


def test_d5_background_table_uses_started_seq(tmp_path: Path) -> None:
    rows = [
        {
            "event_type": "background_tool.started",
            "schema": "sandbox.daemon.audit.pull.v1",
            "lane": "critical",
            "seq": 42,
            "payload": {
                "background_tool": {
                    "background_task_id": "bg-1",
                    "tool_name": "shell",
                    "task_kind": "long_shell",
                }
            },
        },
    ]
    _write_jsonl(tmp_path / "sandbox_events.jsonl", rows)
    report = build_performance_report(tmp_path, _empty_tool_performance())
    bg_rows = report["sandbox"]["sections"]["background_tool_calls"]["rows"]
    assert bg_rows[0]["started_seq"] == 42
    md = render_performance_report_markdown(report)
    header_line = next(line for line in md.splitlines() if "started_seq" in line)
    assert "started_at" not in header_line


# ---------------------------------------------------------------------------
# D6 — memory_peak threshold honours central config
# ---------------------------------------------------------------------------


def test_d6_memory_peak_threshold_honours_central_config(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A tiny threshold makes any non-zero RSS fire the warning."""
    import config as config_module

    class _StubWarnings:
        memory_peak_warn_bytes = 1

    class _StubRunner:
        class daemon_audit_pull:  # noqa: N801
            enabled = True
            floor_ms = 100
            stream_fallback = True

        audit_warnings = _StubWarnings

    class _StubCentral:
        runner = _StubRunner

    monkeypatch.setattr(config_module, "get_central_config", lambda: _StubCentral())
    rows = [
        {
            "event_type": "os_resource.sampled",
            "schema": "sandbox.daemon.audit.pull.v1",
            "lane": "sample",
            "seq": 1,
            "payload": {
                "os_resource": {
                    "sampled_at_monotonic_s": 0.0,
                    "rss_bytes": 100,
                }
            },
        },
    ]
    _write_jsonl(tmp_path / "sandbox_events.jsonl", rows)
    report = build_performance_report(tmp_path, _empty_tool_performance())
    warnings = report["sandbox"]["sections"]["warnings"]["rows"]
    assert any(w["kind"] == "os_resource.memory_peak" for w in warnings)


# ---------------------------------------------------------------------------
# D7 — upperdir_cap warning can fire when cap is aggregated
# ---------------------------------------------------------------------------


def test_d7_upperdir_cap_warning_fires(tmp_path: Path) -> None:
    rows = [
        {
            "event_type": "isolated_workspace.sampled",
            "schema": "sandbox.daemon.audit.pull.v1",
            "lane": "sample",
            "seq": 1,
            "payload": {
                "isolated_workspace": {
                    "upperdir_bytes": int(0.95 * 1024 * 1024 * 1024),
                    "upperdir_cap_bytes": 1024 * 1024 * 1024,
                }
            },
        },
    ]
    _write_jsonl(tmp_path / "sandbox_events.jsonl", rows)
    report = build_performance_report(tmp_path, _empty_tool_performance())
    iso = report["sandbox"]["sections"]["isolated_workspace"]
    assert iso["upperdir_cap_bytes"] == 1024 * 1024 * 1024
    warnings = report["sandbox"]["sections"]["warnings"]["rows"]
    assert any(w["kind"] == "overlay_workspace.upperdir_cap" for w in warnings)


# ---------------------------------------------------------------------------
# D8 — events_count_drift warning when JSONL vs puller diverge
# ---------------------------------------------------------------------------


def test_d8_events_count_drift_warning(tmp_path: Path) -> None:
    rows = [
        {
            "event_type": "tool_call.finished",
            "schema": "sandbox.daemon.audit.pull.v1",
            "lane": "normal",
            "seq": i,
            "payload": {
                "tool_call": {
                    "tool_id": f"t{i}",
                    "tool_name": "read_file",
                    "workspace_mode": "ephemeral",
                    "total_ms": 5.0,
                    "phase_totals_rollup": {"exec_ms": 5.0},
                }
            },
        }
        for i in range(1, 13)
    ]
    _write_jsonl(tmp_path / "sandbox_events.jsonl", rows)
    report = build_performance_report(
        tmp_path,
        _empty_tool_performance(),
        daemon_audit_puller_stats={"events_pulled": 10},
    )
    warnings = report["sandbox"]["sections"]["warnings"]["rows"]
    drift = next(
        (w for w in warnings if w["kind"] == "audit.events_count_drift"),
        None,
    )
    assert drift is not None
    assert "delta 2" in drift["detail"]


# ---------------------------------------------------------------------------
# D9 — artifact_bound_pass surfaces in §12 verdict
# ---------------------------------------------------------------------------


def test_d9_artifact_bound_pass_surfaces_in_verdict(tmp_path: Path) -> None:
    _write_jsonl(tmp_path / "sandbox_events.jsonl", [])
    report = build_performance_report(tmp_path, _empty_tool_performance())
    verdict = report["sandbox"]["sections"]["overhead"]["gate"]["verdict"]
    assert "artifact_bound_pass" in verdict
    assert verdict["artifact_bound_pass"] is True


def test_d9_artifact_inventory_helper_counts_rotations(tmp_path: Path) -> None:
    live = tmp_path / "sandbox_events.jsonl"
    live.write_text("x" * 128, encoding="utf-8")
    rotated = tmp_path / "sandbox_events.jsonl.1.gz"
    rotated.write_bytes(b"\x1f\x8b\x08\x00" + b"\x00" * 60)
    inventory = _collect_artifact_inventory(tmp_path)
    assert inventory["live_bytes"] == 128
    assert inventory["rotated_bytes"] == 64
    assert inventory["rotated_file_count"] == 1


# ---------------------------------------------------------------------------
# D10 — _phase_bar normalizes fractions
# ---------------------------------------------------------------------------


def test_d10_phase_bar_normalizes_overlapping_fractions() -> None:
    """When fractions sum > 1.0 the bar must keep the rightmost glyphs."""
    bar = _phase_bar({"queued": 0.6, "exec": 0.6}, width=40)
    assert "Q" in bar
    assert "E" in bar
    # Both glyph kinds present means no rightmost-truncation regression.


# ---------------------------------------------------------------------------
# D12 — AuditRecorder.start() refuses dual-disable
# ---------------------------------------------------------------------------


def test_d12_recorder_start_refuses_dual_disable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from task_center_runner.audit.recorder import AuditRecorder

    monkeypatch.setenv("EOS_DAEMON_AUDIT_PULL_ENABLED", "false")
    monkeypatch.setenv("EOS_AUDIT_STREAM_FALLBACK", "false")
    monkeypatch.setenv("EOS_ISOLATED_WORKSPACE_ENABLED", "true")

    recorder = AuditRecorder(tmp_path, task_center_run_id="run-1")
    with pytest.raises(RuntimeError, match="refuses to start"):
        recorder.start()


# ---------------------------------------------------------------------------
# D13 — floor_ms from central config when env unset
# ---------------------------------------------------------------------------


def test_d13_floor_ms_central_config_overrides_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import config as config_module
    from task_center_runner.audit.daemon_pull import DaemonAuditPuller

    monkeypatch.delenv("EOS_DAEMON_AUDIT_PULL_FLOOR_MS", raising=False)

    class _StubRunner:
        class daemon_audit_pull:  # noqa: N801
            enabled = True
            floor_ms = 250
            stream_fallback = True

    class _StubCentral:
        runner = _StubRunner

    monkeypatch.setattr(
        config_module, "get_central_config", lambda: _StubCentral()
    )

    async def _stub_pull(_after_seq: int, _limit: int) -> dict[str, Any]:
        return {"events": [], "cursor": {}}

    puller = DaemonAuditPuller(_stub_pull, emit=lambda _events, _resp: None)
    assert puller.floor_ms == 250


def test_d13_stream_fallback_central_config(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import config as config_module
    from task_center_runner.core.engine import _stream_fallback_enabled

    monkeypatch.delenv("EOS_AUDIT_STREAM_FALLBACK", raising=False)

    class _StubRunner:
        class daemon_audit_pull:  # noqa: N801
            enabled = True
            floor_ms = 100
            stream_fallback = False

    class _StubCentral:
        runner = _StubRunner

    monkeypatch.setattr(
        config_module, "get_central_config", lambda: _StubCentral()
    )
    assert _stream_fallback_enabled() is False

    # Env wins over central config.
    monkeypatch.setenv("EOS_AUDIT_STREAM_FALLBACK", "true")
    assert _stream_fallback_enabled() is True


# ---------------------------------------------------------------------------
# D14 — methodology_present sentinel + gate requires it
# ---------------------------------------------------------------------------


def test_d14_methodology_present_false_when_missing(tmp_path: Path) -> None:
    _write_jsonl(tmp_path / "sandbox_events.jsonl", [])
    report = build_performance_report(tmp_path, _empty_tool_performance())
    methodology = report["sandbox"]["sections"]["overhead"]["methodology"]
    assert methodology["methodology_present"] is False


def test_d14_overhead_gate_fails_when_methodology_absent() -> None:
    verdict = evaluate_audit_overhead_gate({})
    assert verdict["passed"] is False
    assert verdict["methodology_present"] is False


# ---------------------------------------------------------------------------
# D15 — forensic-raw delta surfacer (debug mode)
# ---------------------------------------------------------------------------


def test_d15_forensic_deltas_surface_when_enabled(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv(FORENSIC_RAW_ENV, "true")
    rows = [
        {
            "event_type": "tool_call.finished",
            "schema": "sandbox.daemon.audit.pull.v1",
            "lane": "normal",
            "seq": 7,
            "payload": {
                "tool_call": {
                    "tool_id": "t7",
                    "tool_name": "read_file",
                    "workspace_mode": "ephemeral",
                    "total_ms": 10.0,
                    "phase_totals_rollup": {"exec_ms": 10.0},
                },
                "daemon_event": {
                    "type": "tool_call.finished",
                    "payload": {
                        "tool_call": {
                            "tool_name": "DRIFT",
                            "total_ms": 99.0,
                        }
                    },
                },
            },
        },
    ]
    _write_jsonl(tmp_path / "sandbox_events.jsonl", rows)
    report = build_performance_report(tmp_path, _empty_tool_performance())
    forensic = report["sandbox"]["sections"].get("forensic_deltas")
    assert forensic is not None
    keys = {row["key"] for row in forensic["rows"]}
    assert "tool_call.tool_name" in keys
    assert "tool_call.total_ms" in keys
