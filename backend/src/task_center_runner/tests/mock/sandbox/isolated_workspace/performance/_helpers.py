"""Capability-gate guard + sample collectors shared by Tier 9 tests.

Centralises three repeating patterns:

  * Probe-aware skip vs fail per PLAN §18 (loud skip off-reference CI,
    fail on reference CI when the probe says we should be wired up).
  * ``LatencyBudget`` construction from the session baseline fixture.
  * Audit-event ``phases_ms`` extraction with SUBSET-COVER pre-checks.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from task_center_runner.tests.mock.sandbox.isolated_workspace import (
    _iws_invariants,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace.conftest import (
    reference_ci_host,
)


def gate_or_skip(
    iws_capability_probe: dict[str, bool], probe_name: str,
) -> None:
    """Skip (off-reference) or fail (on-reference) when a probe is False.

    Implements the §18 capability-probe policy. Caller passes the relevant
    probe name (``has_mount_overlay``, ``has_run_in_handle`` etc.).
    """
    if iws_capability_probe.get(probe_name, False):
        return
    reason = (
        f"capability '{probe_name}' not detected on this host; "
        f"this is a kernel-touching Tier 9 test"
    )
    if reference_ci_host():
        pytest.fail(reason)
    else:
        pytest.skip(reason)


def require_baseline(
    iws_latency_baseline: dict[str, float], op_name: str,
) -> None:
    """Skip when session baseline missing for ``op_name``.

    Off-reference hosts (laptops, dev shells) may not have the live tier
    reachable; in that case the baseline fixture returns an empty dict.
    The Tier 9 ratio tests cannot run without it.
    """
    if not iws_latency_baseline.get(op_name):
        pytest.skip(
            f"latency_baseline missing '{op_name}' "
            "(live tier unreachable on this host)",
        )


def build_budget(
    iws_latency_baseline: dict[str, float],
    iws_latency_budget_path: Path | None,
) -> _iws_invariants.LatencyBudget:
    return _iws_invariants.LatencyBudget.from_paths(
        baseline_ms=iws_latency_baseline,
        budget_path=iws_latency_budget_path,
    )


def event_payloads(
    jsonl_path: Path, event_type: str,
) -> list[dict[str, Any]]:
    rows = _iws_invariants.events_of_type(jsonl_path, event_type)
    return [row.get("payload") or {} for row in rows]
