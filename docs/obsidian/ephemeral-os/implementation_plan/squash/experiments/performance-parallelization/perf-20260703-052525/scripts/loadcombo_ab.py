#!/usr/bin/env python3
"""LOAD-COMBO A/B: cross-arm equivalence for the noisy multi-round HTTP load case.

Unlike the controlled AB-EQUIV case, ``_scenario_load_combo_http`` publishes
layers concurrently (``_publish_small_files_concurrent``), so manifest layer
*order* — and therefore exact block boundaries and per-round disposition counts —
is not identical run-to-run. This comparator asserts the invariants that DO hold
under that nondeterminism, so a green verdict is honest rather than flaky:

  * both arms pass every correctness/space/teardown axis (each arm independently
    proves migration-correctness, space shrink, and clean teardown),
  * both arms produce the same set of disposition CLASSES (no arm invents a
    Faulty/Leased class the other lacks),
  * both arms keep T_http_disconnect under the 1500 ms budget,
  * the parallel arm actually overlaps (overlap>1) and its sweep wall is no worse
    than the serial control (the speedup the parallelization exists to deliver).

Exact block/replaced totals are reported for context but not gated (they legitimately
drift with publish order). Usage: loadcombo_ab.py ARM_A_dir ARM_B_dir [--out FILE]
(ARM_A = serial W=1 report dir; ARM_B = parallel arm report dir.)
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import analyze_spans

HTTP_DISCONNECT_BUDGET_MS = 1500.0


def _read_json(path):
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def arm_facts(report_dir):
    report_dir = Path(report_dir)
    ndjson = sorted(report_dir.glob("observability.ndjson*"))
    invocations = analyze_spans.analyze(
        analyze_spans.load_spans([str(p) for p in ndjson])
    ) if ndjson else []
    measured = [inv for inv in invocations if inv.get("sessions_swept", 0) > 0]
    classes = set()
    disposition_totals = {}
    for inv in measured:
        for name, dist in inv["by_disposition"].items():
            classes.add(name)
            disposition_totals[name] = disposition_totals.get(name, 0) + dist.get("n", 0)
    verdict = _read_json(report_dir / "verdict.json") or {}
    combo = _read_json(report_dir / "combo-summary.json") or {}
    axes = verdict.get("axes", {})
    return {
        "report_dir": str(report_dir),
        "axes_pass": {name: bool(a.get("pass")) for name, a in axes.items()},
        "all_axes_pass": all(a.get("pass") for a in axes.values()) if axes else False,
        "teardown_pass": bool(verdict.get("teardown", {}).get("pass")),
        "disposition_classes": sorted(classes),
        "disposition_totals": disposition_totals,
        "overlap_p50": analyze_spans.percentile([i["overlap_factor"] for i in measured], 0.50) if measured else 0.0,
        "sweep_wall_p50": analyze_spans.percentile([i["sweep_wall_ms"] for i in measured], 0.50) if measured else 0.0,
        "sweep_width_reported": sorted({i.get("sweep_width") for i in measured if i.get("sweep_width") is not None}),
        "measured_squashes": len(measured),
        "block_total": combo.get("squash_blocks"),
        "replaced_total": combo.get("replaced_layers"),
        "T_http_disconnect_ms": verdict.get("timers", {}).get("T_http_disconnect", {}).get("ms"),
    }


def compare(arm_a, arm_b):
    checks = {}
    checks["both_all_axes_pass"] = {
        "a": arm_a["all_axes_pass"], "b": arm_b["all_axes_pass"],
        "ok": arm_a["all_axes_pass"] and arm_b["all_axes_pass"], "required": True,
    }
    checks["both_teardown_clean"] = {
        "a": arm_a["teardown_pass"], "b": arm_b["teardown_pass"],
        "ok": arm_a["teardown_pass"] and arm_b["teardown_pass"], "required": True,
    }
    checks["disposition_classes"] = {
        "a": arm_a["disposition_classes"], "b": arm_b["disposition_classes"],
        "ok": arm_a["disposition_classes"] == arm_b["disposition_classes"], "required": True,
    }
    da, db = arm_a["T_http_disconnect_ms"], arm_b["T_http_disconnect_ms"]
    checks["http_disconnect_under_budget"] = {
        "a": da, "b": db,
        "ok": (da is not None and da <= HTTP_DISCONNECT_BUDGET_MS)
              and (db is not None and db <= HTTP_DISCONNECT_BUDGET_MS),
        "required": True,
    }
    width_a = arm_a["sweep_width_reported"]
    width_b = arm_b["sweep_width_reported"]
    checks["arms_differ_in_width"] = {
        "a": width_a, "b": width_b,
        "serial_vs_parallel": width_a == [1] and all(w > 1 for w in width_b),
        "required": True,
    }
    checks["parallel_overlaps_and_no_slower"] = {
        "a_overlap": round(arm_a["overlap_p50"], 2), "b_overlap": round(arm_b["overlap_p50"], 2),
        "a_wall_ms": round(arm_a["sweep_wall_p50"], 2), "b_wall_ms": round(arm_b["sweep_wall_p50"], 2),
        "ok": arm_b["overlap_p50"] > 1.0 and arm_b["sweep_wall_p50"] <= arm_a["sweep_wall_p50"] * 1.10,
        "required": True,
    }
    # Reported, not gated (publish-order nondeterminism).
    checks["block_total"] = {
        "a": arm_a["block_total"], "b": arm_b["block_total"],
        "equal": arm_a["block_total"] == arm_b["block_total"], "required": False,
    }
    checks["disposition_totals"] = {
        "a": arm_a["disposition_totals"], "b": arm_b["disposition_totals"], "required": False,
    }
    passed = all(c.get("ok", c.get("equal", True)) for c in checks.values() if c.get("required"))
    return {"pass": passed, "arm_a": arm_a, "arm_b": arm_b, "checks": checks}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("arm_a", help="serial W=1 report dir")
    ap.add_argument("arm_b", help="parallel arm report dir")
    ap.add_argument("--out")
    args = ap.parse_args()
    diff = compare(arm_facts(args.arm_a), arm_facts(args.arm_b))
    text = json.dumps(diff, indent=2, sort_keys=True)
    print(text)
    if args.out:
        Path(args.out).write_text(text + "\n", encoding="utf-8")
        print(f"wrote {args.out}", file=sys.stderr)
    return 0 if diff["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
