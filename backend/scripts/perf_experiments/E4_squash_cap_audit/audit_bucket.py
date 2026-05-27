#!/usr/bin/env python3
"""Reproduce the E4 audit: bucket workspace.mount_s by manifest depth.

Usage:
    uv run python backend/scripts/perf_experiments/E4_squash_cap_audit/audit_bucket.py [--root .sweevo_runs]

Walks all performance_report.json under <root>/scenario_logs/*/*/ and joins
each ``workspace.mount_s`` measurement with the most-recent preceding
``resource.layer_stack.manifest_depth`` observation in the same event stream.

Prints the same depth-bucketed mount-latency table written in audit_analysis.md.
"""

from __future__ import annotations

import argparse
import glob
import json
from pathlib import Path

DEFAULT_ROOT = Path(__file__).resolve().parents[3].parent / ".sweevo_runs"

BUCKETS = (
    ("1-7", 1, 8),
    ("8-15", 8, 16),
    ("16-31", 16, 32),
    ("32-63", 32, 64),
    ("64-99", 64, 100),
    ("100-199", 100, 200),
    ("200+", 200, 1 << 31),
)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", type=Path, default=DEFAULT_ROOT)
    args = ap.parse_args()

    pattern = str(args.root / "scenario_logs" / "*" / "*" / "performance_report.json")
    files = sorted(glob.glob(pattern))
    print(f"reports: {len(files)} under {args.root}")

    samples: list[tuple[float, float, str]] = []
    no_depth = 0
    for f in files:
        try:
            data = json.loads(Path(f).read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        scenario = f.split("scenario_logs/")[1].split("/")[0]
        current_depth: float | None = None
        for event in data.get("sandbox", {}).get("events", []):
            t = event.get("timings") or {}
            if "resource.layer_stack.manifest_depth" in t:
                current_depth = float(t["resource.layer_stack.manifest_depth"])
            if "workspace.mount_s" in t:
                mount_s = float(t["workspace.mount_s"])
                if current_depth is not None:
                    samples.append((current_depth, mount_s, scenario))
                else:
                    no_depth += 1

    print(f"mount events joined with depth: {len(samples)}")
    print(f"mount events with no preceding depth observation: {no_depth}")

    buckets: dict[str, list[float]] = {}
    for depth, mount, _ in samples:
        for name, lo, hi in BUCKETS:
            if lo <= depth < hi:
                buckets.setdefault(name, []).append(mount * 1000.0)
                break

    print()
    print(f"{'depth bucket':<15s} | {'n':>6s} | {'p50 ms':>7s} | {'p95 ms':>7s} | {'p99 ms':>7s} | {'max ms':>7s}")
    print("-" * 70)
    for name, _, _ in BUCKETS:
        if name not in buckets:
            continue
        vals = sorted(buckets[name])
        n = len(vals)
        p50 = vals[n // 2]
        p95 = vals[int(n * 0.95)]
        p99 = vals[min(n - 1, int(n * 0.99))]
        mx = vals[-1]
        print(
            f"{name:<15s} | {n:6d} | {p50:7.2f} | {p95:7.2f} | {p99:7.2f} | {mx:7.2f}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
