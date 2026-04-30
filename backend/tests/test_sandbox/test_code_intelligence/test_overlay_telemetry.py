"""Tests for the overlay counter recorder in :mod:`routing.telemetry`."""

from __future__ import annotations

import pytest

from sandbox.code_intelligence import telemetry


@pytest.fixture(autouse=True)
def _reset_counters() -> None:
    # Reset state between tests so counts are independent.
    with telemetry._OVERLAY_LOCK:
        for field_name in telemetry._OVERLAY_COUNTERS.__dataclass_fields__:
            setattr(telemetry._OVERLAY_COUNTERS, field_name, 0)
    yield
    with telemetry._OVERLAY_LOCK:
        for field_name in telemetry._OVERLAY_COUNTERS.__dataclass_fields__:
            setattr(telemetry._OVERLAY_COUNTERS, field_name, 0)


def test_record_overlay_op_is_additive() -> None:
    telemetry.record_overlay_op(ops_total=1, gitinclude_changes=3)
    telemetry.record_overlay_op(ops_total=1, gitinclude_changes=4, upper_bytes=1024)
    snap = telemetry.overlay_counters_snapshot()
    assert snap.ops_total == 2
    assert snap.gitinclude_changes == 7
    assert snap.upper_bytes == 1024


def test_record_overlay_op_ignores_unknown_fields() -> None:
    telemetry.record_overlay_op(ops_total=1, bogus_field=999)  # type: ignore[arg-type]
    snap = telemetry.overlay_counters_snapshot()
    assert snap.ops_total == 1
    assert not hasattr(snap, "bogus_field")


def test_snapshot_is_independent_of_live_state() -> None:
    telemetry.record_overlay_op(ops_total=1)
    snap = telemetry.overlay_counters_snapshot()
    telemetry.record_overlay_op(ops_total=1)
    # The first snapshot is not mutated by later records.
    assert snap.ops_total == 1
    assert telemetry.overlay_counters_snapshot().ops_total == 2


def test_reject_rollup_fields_increment() -> None:
    telemetry.record_overlay_op(
        ops_total=1,
        ops_rejected=1,
        dotgit_rejects=1,
    )
    snap = telemetry.overlay_counters_snapshot()
    assert snap.ops_rejected == 1
    assert snap.dotgit_rejects == 1
