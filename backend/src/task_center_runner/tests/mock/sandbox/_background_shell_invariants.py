"""Shared invariants helper for the T1 – T8 background-shell live tests.

Operates on a ``sandbox_events.jsonl`` path produced by an
``AuditRecorder`` bound to a test run. Two consumption modes:

- **Standard** (T1, T2, T3, T5, T6, T7, T8): assert tight count equality
  and per-``job_id`` matching.
- **Truncated** (T4 engine-kill): the host's recorder dies with the engine
  before the daemon's TTL reap completes; only the pre-kill prefix is
  available, so the helper degrades to "no error lines before truncation."
"""

from __future__ import annotations

import json
from pathlib import Path

from task_center_runner.audit.events import EventType


_ERROR_NEEDLES = (
    "internal_error",
    "stale lowerdir",
    "mount_failed",
    "manifest references missing layer",
)


def _read_rows(jsonl_path: Path) -> list[dict[str, object]]:
    if not jsonl_path.exists():
        return []
    rows: list[dict[str, object]] = []
    raw = jsonl_path.read_text(encoding="utf-8", errors="replace")
    for line in raw.splitlines():
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            # Truncated JSON at the engine-kill cut point is expected in T4.
            continue
    return rows


def _shell_rows(rows: list[dict[str, object]], event_type: str) -> list[dict[str, object]]:
    return [row for row in rows if row.get("event_type") == event_type]


def _job_id(row: dict[str, object]) -> str | None:
    payload = row.get("payload")
    if not isinstance(payload, dict):
        return None
    job_id = payload.get("job_id")
    return str(job_id) if job_id else None


def assert_shell_audit_invariants(
    jsonl_path: Path,
    *,
    expect_truncated: bool = False,
) -> None:
    """Assert AC-12, AC-5, and AC-11 hold for this run's sandbox_events.jsonl.

    AC-12: count(SHELL_LAUNCHED) == count(SHELL_REAPED).
    AC-5:  every SHELL_CANCELLED has a matching SHELL_REAPED by job_id.
    AC-11: no internal_error / stale lowerdir / mount_failed / manifest
           references missing layer lines (substring match on raw text).
    """
    rows = _read_rows(jsonl_path)

    launched = _shell_rows(rows, EventType.SANDBOX_SHELL_LAUNCHED.value)
    cancelled = _shell_rows(rows, EventType.SANDBOX_SHELL_CANCELLED.value)
    reaped = _shell_rows(rows, EventType.SANDBOX_SHELL_REAPED.value)

    if not expect_truncated:
        assert len(launched) == len(reaped), (
            f"AC-12 violation: SHELL_LAUNCHED={len(launched)} vs "
            f"SHELL_REAPED={len(reaped)} in {jsonl_path}"
        )

    reaped_ids = {_job_id(row) for row in reaped}
    for cancel_row in cancelled:
        cid = _job_id(cancel_row)
        assert cid in reaped_ids, (
            f"AC-5 violation: SHELL_CANCELLED job_id={cid!r} has no "
            f"matching SHELL_REAPED in {jsonl_path}"
        )

    if jsonl_path.exists():
        raw_text = jsonl_path.read_text(encoding="utf-8", errors="replace")
        for needle in _ERROR_NEEDLES:
            assert needle not in raw_text, (
                f"AC-11 violation: '{needle}' appears in {jsonl_path}"
            )


__all__ = ["assert_shell_audit_invariants"]
