"""Tests for centralized sandbox timing helpers."""

from __future__ import annotations

import pytest
from sandbox._shared.timing_keys import TimingKey
from sandbox._shared.clock import normalize_timing_map, record_elapsed
from sandbox.audit.timing import timing_audit_signals


def test_normalize_timing_map_projects_string_float_dict() -> None:
    assert normalize_timing_map({"a": "0.25", 10: 2}) == {
        "a": 0.25,
        "10": 2.0,
    }


def test_normalize_timing_map_projects_timing_key_enum_values() -> None:
    assert normalize_timing_map({TimingKey.PREPARE_TOTAL: "0.25"}) == {
        "occ.prepare.total_s": 0.25,
    }


def test_normalize_timing_map_projects_stringified_timing_key_names() -> None:
    assert normalize_timing_map({"TimingKey.APPLY_COMMIT": "0.25"}) == {
        "occ.apply.commit_s": 0.25,
    }


def test_record_elapsed_writes_and_returns_elapsed_seconds(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    clock = iter([10.0, 12.5])
    monkeypatch.setattr("sandbox._shared.clock.monotonic_now", lambda: next(clock))

    started_at = next(clock)
    timings: dict[str, float] = {}

    assert record_elapsed(timings, "api.shell.total_s", started_at) == 2.5
    assert timings == {"api.shell.total_s": 2.5}


def test_timing_audit_signals_preserve_subsystem_event_order() -> None:
    signals = timing_audit_signals(
        {
            "occ.prepare.total_s": 0.01,
            "occ.apply.total_s": 0.02,
            "workspace.tool_s": 0.03,
            "layer_stack.lease_acquire_s": 0.04,
            "layer_stack.publish.total_s": 0.05,
            "layer_stack.auto_squash.total_s": 0.06,
        },
        status="ok",
    )

    assert signals == (
        "occ_prepared",
        "occ_committed",
        "overlay_executed",
        "layer_stack_lease_acquired",
        "layer_stack_layer_published",
        "layer_stack_auto_squashed",
    )


def test_timing_audit_signals_classify_occ_conflict_without_commit() -> None:
    signals = timing_audit_signals(
        {"occ.prepare.total_s": 0.01, "occ.apply.total_s": 0.02},
        status="conflict",
    )

    assert signals == ("occ_prepared", "occ_conflicted")


def test_timing_audit_signals_classify_public_api_occ_apply_as_commit() -> None:
    signals = timing_audit_signals(
        {"api.write.occ_apply_s": 0.02},
        status="ok",
    )

    assert signals == ("occ_committed",)
