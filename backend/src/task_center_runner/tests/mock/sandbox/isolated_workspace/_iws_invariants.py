"""Audit-event + reachability + lifecycle assertion helpers.

Mirrors :mod:`_background_shell_invariants` in style: every reusable check
lives here so individual test files read as intent, not boilerplate.

Audit events are read from a ``sandbox_events.jsonl`` path. The format matches
the recorder's per-line JSON envelope (see
``task_center_runner.audit.recorder``).
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Iterable


# ---------------------------------------------------------------------------
# Audit-bus helpers
# ---------------------------------------------------------------------------


def read_events(jsonl_path: Path) -> list[dict[str, Any]]:
    """Read the JSONL audit log, tolerating partial last-line truncation."""
    if not jsonl_path.exists():
        return []
    rows: list[dict[str, Any]] = []
    raw = jsonl_path.read_text(encoding="utf-8", errors="replace")
    for line in raw.splitlines():
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            continue  # last-line truncation in crash tests
    return rows


def events_of_type(
    jsonl_path: Path,
    event_type: str,
    *,
    predicate: Callable[[dict[str, Any]], bool] | None = None,
) -> list[dict[str, Any]]:
    out = [row for row in read_events(jsonl_path) if row.get("type") == event_type]
    if predicate is None:
        return out
    return [row for row in out if predicate(row)]


def assert_audit_sequence(
    jsonl_path: Path,
    expected_types: Iterable[str],
    *,
    handle_id: str | None = None,
) -> None:
    """Assert that ``expected_types`` appear in order in the audit log.

    Extra events between the expected ones are tolerated — the assertion is
    "did these happen in this order", not "these and nothing else."
    """
    target: list[str] = list(expected_types)
    cursor = 0
    for row in read_events(jsonl_path):
        if cursor >= len(target):
            break
        if row.get("type") != target[cursor]:
            continue
        if handle_id is not None:
            payload = row.get("payload") or {}
            if payload.get("handle_id") != handle_id:
                continue
        cursor += 1
    assert cursor == len(target), (
        f"audit sequence not observed: expected {target[cursor:]} after "
        f"{target[:cursor]}; events were "
        f"{[row.get('type') for row in read_events(jsonl_path)]}"
    )


def assert_no_event(jsonl_path: Path, event_type: str) -> None:
    matches = events_of_type(jsonl_path, event_type)
    assert matches == [], (
        f"expected zero {event_type} events, found {len(matches)}: {matches}"
    )


def assert_event_payload(
    jsonl_path: Path,
    event_type: str,
    key: str,
    value: Any,
) -> None:
    matches = events_of_type(jsonl_path, event_type)
    assert matches, f"no {event_type} events in {jsonl_path}"
    seen = [row.get("payload", {}).get(key) for row in matches]
    assert value in seen, (
        f"{event_type} payload[{key}] expected to contain {value!r}; saw {seen}"
    )


def assert_handle_ids_unique_per_enter(jsonl_path: Path) -> None:
    enters = events_of_type(jsonl_path, "sandbox_isolated_workspace_enter")
    ids = [row.get("payload", {}).get("handle_id") for row in enters]
    duplicates = sorted({h for h in ids if ids.count(h) > 1})
    assert not duplicates, f"duplicate handle_ids in enter events: {duplicates}"


# ---------------------------------------------------------------------------
# Phase-timing invariants (v2 §14)
# ---------------------------------------------------------------------------


PHASE_TIMER_EPSILON_MS = 2.0


def assert_subset_cover(
    phases_ms: dict[str, float],
    total_ms: float,
    *,
    label: str = "",
) -> None:
    """Sum of observed phase ms must not exceed total_ms + epsilon.

    Conditional-key emission means absent phases are OK; the invariant is
    one-sided. Epsilon is ``max(2.0 ms, 5% × total_ms)`` per PLAN §14.
    """
    epsilon = max(PHASE_TIMER_EPSILON_MS, 0.05 * total_ms)
    observed = sum(phases_ms.values())
    assert observed <= total_ms + epsilon, (
        f"SUBSET-COVER violated for {label}: sum(phases_ms)={observed} > "
        f"total_ms={total_ms} + epsilon={epsilon}"
    )
    for name, value in phases_ms.items():
        assert value >= 0.0, f"{label} {name} negative phase time: {value}"


def assert_phases_within_keys(
    phases_ms: dict[str, float],
    allowed_keys: Iterable[str],
    *,
    label: str = "",
) -> None:
    allowed = set(allowed_keys)
    unexpected = sorted(set(phases_ms.keys()) - allowed)
    assert not unexpected, (
        f"{label} phases_ms contains unexpected keys: {unexpected}; "
        f"allowed={sorted(allowed)}"
    )


def phase_timing_extractor(event_payload: dict[str, Any]) -> dict[str, float]:
    """Return ``phases_ms`` from an event payload as a plain dict."""
    phases = event_payload.get("phases_ms")
    return dict(phases) if isinstance(phases, dict) else {}


# ---------------------------------------------------------------------------
# Latency helpers (v2 §15)
# ---------------------------------------------------------------------------


def median(values: list[float]) -> float:
    if not values:
        return 0.0
    s = sorted(values)
    mid = len(s) // 2
    if len(s) % 2:
        return s[mid]
    return (s[mid - 1] + s[mid]) / 2.0


def assert_within_ratio_band(
    value_ms: float,
    baseline_ms: float,
    *,
    low: float,
    high: float,
    label: str = "",
) -> None:
    """Assert ``baseline*low ≤ value ≤ baseline*high``.

    Used by the Tier 9 hybrid baseline tests to express "no major regression"
    while staying portable across CI hosts (per §15.1).
    """
    assert baseline_ms > 0, f"{label} baseline must be positive; got {baseline_ms}"
    assert (
        baseline_ms * low <= value_ms <= baseline_ms * high
    ), (
        f"{label} value {value_ms} outside ratio band "
        f"[{baseline_ms * low}, {baseline_ms * high}] (baseline {baseline_ms})"
    )


# ---------------------------------------------------------------------------
# Tier 9 latency budget (v2 §15.1, §17)
# ---------------------------------------------------------------------------


def _load_latency_budget(budget_path: Path | None) -> dict[str, Any] | None:
    """Read ``_data/latency_budget.json`` if it exists, else ``None``.

    Returning ``None`` lets each test ``pytest.skip`` with a precise reason
    (PR 7 hasn't landed) instead of synthesising fake budgets.
    """
    if budget_path is None or not budget_path.exists():
        return None
    try:
        return json.loads(budget_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


@dataclass(frozen=True)
class LatencyBudget:
    """HYBRID latency assertion — ratio-to-baseline AND absolute ceiling.

    Per PLAN §15.1:
      * Session baseline catches in-PR variance (portable).
      * ``latency_budget.json`` catches multi-PR drift + absolute hardware
        regression (committed artifact, refreshed once per ~10 PRs).

    Constructed via :py:func:`from_paths`; tests call
    :py:meth:`assert_stable_and_within_budget` once per operation per call.
    Operations: ``workspace_create``, ``tool_call``, ``kill_holder``,
    ``gc_orphan``. The first three carry ``total_ms_p95`` and
    ``total_ms_p99``; ``gc_orphan`` carries ``total_ms_p95_per_orphan``.
    """

    baseline_ms: dict[str, float]
    budget: dict[str, Any] | None
    ratio_low: float = 0.3
    ratio_high: float = 3.0
    absolute_p95_slack: float = 1.5

    @classmethod
    def from_paths(
        cls,
        *,
        baseline_ms: dict[str, float],
        budget_path: Path | None,
    ) -> LatencyBudget:
        return cls(
            baseline_ms=dict(baseline_ms),
            budget=_load_latency_budget(budget_path),
        )

    def has_baseline_for(self, op_name: str) -> bool:
        return op_name in self.baseline_ms and self.baseline_ms[op_name] > 0

    def assert_stable_and_within_budget(
        self,
        samples_ms: list[float],
        *,
        op_name: str,
    ) -> None:
        """Run both checks. Median ratio AND absolute p95 if budget present.

        ``samples_ms`` should be the per-operation durations (typically
        the audit ``total_ms`` per emit). Median is compared to the
        session baseline; p95 to the optional checked-in budget.
        """
        if not samples_ms:
            raise AssertionError(f"{op_name}: no samples to check")
        sample_median = median(samples_ms)
        if self.has_baseline_for(op_name):
            assert_within_ratio_band(
                sample_median, self.baseline_ms[op_name],
                low=self.ratio_low, high=self.ratio_high,
                label=f"{op_name}.median_vs_baseline",
            )
        if self.budget is not None:
            spec = self.budget.get(op_name) or {}
            ceiling = spec.get("total_ms_p95")
            if ceiling:
                sample_p95 = _percentile(samples_ms, 95.0)
                assert sample_p95 <= ceiling * self.absolute_p95_slack, (
                    f"{op_name}: p95 {sample_p95}ms exceeds "
                    f"{ceiling}ms x {self.absolute_p95_slack} slack",
                )


def _percentile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    s = sorted(values)
    k = (len(s) - 1) * (p / 100.0)
    lo = int(k)
    hi = min(lo + 1, len(s) - 1)
    if lo == hi:
        return s[lo]
    frac = k - lo
    return s[lo] * (1.0 - frac) + s[hi] * frac


__all__ = [
    "LatencyBudget",
    "PHASE_TIMER_EPSILON_MS",
    "assert_audit_sequence",
    "assert_event_payload",
    "assert_handle_ids_unique_per_enter",
    "assert_no_event",
    "assert_phases_within_keys",
    "assert_subset_cover",
    "assert_within_ratio_band",
    "events_of_type",
    "median",
    "phase_timing_extractor",
    "read_events",
]
