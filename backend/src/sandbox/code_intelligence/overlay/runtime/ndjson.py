"""NDJSON emission for overlay runtime results."""

from __future__ import annotations

import json
import os

from .types import ClassifyOutcome, PolicyRejectOutcome


def write_diff_ndjson(
    *,
    run_dir: str,
    snap: str,
    exit_code: int,
    outcome: ClassifyOutcome,
    upper_bytes: int,
    upper_files: int,
    warnings: list[str] | None = None,
    snapshot_timings: dict[str, float] | None = None,
    run_timings: dict[str, float] | None = None,
) -> str:
    """Write ``$RUN_DIR/diff.ndjson`` and return its absolute path."""
    path = os.path.join(run_dir, "diff.ndjson")
    os.makedirs(run_dir, exist_ok=True)
    lines: list[str] = []
    meta = {
        "_meta": {
            "snap": snap,
            "exit_code": exit_code,
            "upper_bytes": upper_bytes,
            "upper_files": upper_files,
            "gitinclude_changes": len(outcome.gitinclude),
            "gitignore_changes": len(outcome.gitignore_paths),
            "gitignore_paths": list(outcome.gitignore_paths),
            "whiteouts_gitinclude": outcome.whiteouts_gitinclude,
            "whiteouts_gitignore_refused": outcome.whiteouts_gitignore_refused,
            "dotgit_rejects": outcome.dotgit_rejects,
            "direct_merged_bytes": outcome.direct_merged_bytes,
            "snapshot_timings": dict(snapshot_timings or {}),
            "run_timings": dict(run_timings or {}),
            "warnings": list(warnings or ()),
        }
    }
    lines.append(json.dumps(meta, separators=(",", ":")))
    for change in outcome.gitinclude:
        lines.append(
            json.dumps(
                {
                    "path": change.path,
                    "kind": change.kind,
                    "base_content": change.base_content,
                    "base_existed": change.base_existed,
                    "final_content": change.final_content,
                    "strict_base": True,
                },
                separators=(",", ":"),
            )
        )
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines))
        fh.write("\n")
    return path


def write_reject_ndjson(
    *,
    run_dir: str,
    snap: str,
    reject: PolicyRejectOutcome,
    snapshot_timings: dict[str, float] | None = None,
    run_timings: dict[str, float] | None = None,
) -> str:
    path = os.path.join(run_dir, "diff.ndjson")
    os.makedirs(run_dir, exist_ok=True)
    payload = {
        "_reject": {
            "snap": snap,
            "reason": reject.reason,
            "paths": list(reject.paths),
            "snapshot_timings": dict(snapshot_timings or {}),
            "run_timings": dict(run_timings or {}),
        }
    }
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(json.dumps(payload, separators=(",", ":")))
        fh.write("\n")
    return path


__all__ = ["write_diff_ndjson", "write_reject_ndjson"]
