"""Shared helpers for per-cell streaming + resume-on-restart artifacts.

Implements the JSONL artifact contract from
``progressive-live-test-tiers-design-20260508.md`` §§4-5:

- ``resolve_run_id()`` honours the ``EOS_TIER_RUN_ID`` env var so the
  tier runner can pin artifact filenames; falls back to the existing
  ISO+pid pattern for standalone pytest invocations.
- ``stream_row()`` appends one JSONL row, flushes, fsyncs — kill-9
  mid-loop preserves prior cells.
- ``load_prior_data_rows()`` returns prior data rows from a partial
  artifact, dropping any trailing summary so a rebuild can recompute
  it.
- ``rewrite_artifact()`` truncate-rewrites the artifact with full
  rows + an optional trailing summary row.
"""

from __future__ import annotations

import json
import os
from datetime import datetime, timezone
from pathlib import Path


def resolve_run_id() -> str:
    """Return the runner-pinned run id when set, else a timestamp+pid."""
    env_run_id = os.environ.get("EOS_TIER_RUN_ID")
    if env_run_id:
        return env_run_id
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + f"-{os.getpid()}"


def load_prior_data_rows(artifact: Path) -> list[dict[str, object]]:
    """Read data rows from a prior partial artifact; drop trailing summary.

    Rows whose ``schema`` ends with ``summary.v1`` (any prefix) are
    treated as summary rows and discarded so the caller can recompute
    them from the full row set.
    """
    if not artifact.exists():
        return []
    rows: list[dict[str, object]] = []
    with artifact.open(encoding="utf-8") as fh:
        for line in fh:
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                continue
            schema = str(row.get("schema", ""))
            if schema.endswith("summary.v1"):
                continue
            rows.append(row)
    return rows


def stream_row(artifact: Path, row: dict[str, object]) -> None:
    """Append one JSONL row, flush, fsync — mid-loop kill-9 durability."""
    with artifact.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps(row, sort_keys=True, separators=(",", ":")))
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())


def rewrite_artifact(
    artifact: Path,
    rows: list[dict[str, object]],
    summary_row: dict[str, object] | None,
) -> None:
    """Truncate-rewrite artifact with rows + optional trailing summary."""
    with artifact.open("w", encoding="utf-8") as fh:
        for row in rows:
            fh.write(json.dumps(row, sort_keys=True, separators=(",", ":")))
            fh.write("\n")
        if summary_row is not None:
            fh.write(json.dumps(summary_row, sort_keys=True, separators=(",", ":")))
            fh.write("\n")


__all__ = [
    "resolve_run_id",
    "load_prior_data_rows",
    "stream_row",
    "rewrite_artifact",
]
