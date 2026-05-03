"""NDJSON emission for overlay runtime results."""

from __future__ import annotations

import base64
import json
import os

from .types import PolicyRejectOutcome, UpperChange


def write_diff_ndjson(
    *,
    run_dir: str,
    exit_code: int,
    upper_changes: tuple[UpperChange, ...],
    upper_bytes: int,
    upper_files: int,
    warnings: list[str] | None = None,
    run_timings: dict[str, float] | None = None,
) -> str:
    """Write ``$RUN_DIR/diff.ndjson`` and return its absolute path."""
    path = os.path.join(run_dir, "diff.ndjson")
    os.makedirs(run_dir, exist_ok=True)
    lines: list[str] = []
    meta = {
        "_meta": {
            "exit_code": exit_code,
            "upper_bytes": upper_bytes,
            "upper_files": upper_files,
            "upper_changes": len(upper_changes),
            "run_timings": dict(run_timings or {}),
            "warnings": list(warnings or ()),
        }
    }
    lines.append(json.dumps(meta, separators=(",", ":")))
    for change in upper_changes:
        lines.append(
            json.dumps(
                {
                    "rel": change.rel,
                    "kind": change.kind,
                    "base_bytes_b64": _encode_bytes(change.base_bytes),
                    "upper_bytes_b64": _encode_bytes(change.upper_bytes),
                    "base_existed": change.base_existed,
                },
                separators=(",", ":"),
            )
        )
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines))
        fh.write("\n")
    return path


def _encode_bytes(value: bytes | None) -> str | None:
    if value is None:
        return None
    return base64.b64encode(value).decode("ascii")


def write_reject_ndjson(
    *,
    run_dir: str,
    reject: PolicyRejectOutcome,
    run_timings: dict[str, float] | None = None,
) -> str:
    path = os.path.join(run_dir, "diff.ndjson")
    os.makedirs(run_dir, exist_ok=True)
    payload = {
        "_reject": {
            "reason": reject.reason,
            "paths": list(reject.paths),
            "run_timings": dict(run_timings or {}),
        }
    }
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(json.dumps(payload, separators=(",", ":")))
        fh.write("\n")
    return path


def write_result_json(
    *,
    run_dir: str,
    exit_code: int,
    rejected: dict[str, object] | None,
    run_timings: dict[str, float],
) -> str:
    """Write the atomic completion marker consumed by direct-runtime callers."""
    path = os.path.join(run_dir, "result.json")
    tmp_path = f"{path}.tmp-{os.getpid()}"
    payload = {
        "exit_code": exit_code,
        "rejected": rejected,
        "run_timings": dict(run_timings),
    }
    with open(tmp_path, "w", encoding="utf-8") as fh:
        json.dump(payload, fh, separators=(",", ":"))
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp_path, path)
    return path


__all__ = ["write_diff_ndjson", "write_reject_ndjson", "write_result_json"]
