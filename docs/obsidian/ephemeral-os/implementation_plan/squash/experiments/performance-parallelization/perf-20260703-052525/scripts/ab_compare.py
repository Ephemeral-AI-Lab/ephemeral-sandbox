#!/usr/bin/env python3
"""CORR-AB-EQUIV comparator: diff two arms' ab-facts.json for LOGICAL equivalence.

The serial control (``EOS_REMOUNT_SWEEP_WIDTH=1``) and the parallel arm
(``=cores``) run the identical deterministic workload; the only permitted
difference is timing. Squashed layer ids carry a per-process nonce
(``S{ver}-{nonce}``), so manifests are NOT byte-identical and ``manifest_root_hash``
differs run-to-run — this asserts logical outcome instead:

  * identical disposition multiset (Identity/Migrated/Leased/Faulty counts),
  * identical squashed-block count,
  * identical final manifest layer count,
  * identical surviving/final layer breakdown by prefix (L published / S squashed
    / B base) — plus the exact surviving pre-squash id set when nonces line up
    (reported, not required),
  * identical space (layer-dir delta) and clean staging,
  * and the two arms genuinely ran at different widths (guards a no-op A/B).

Usage: ab_compare.py ARM_A_ab-facts.json ARM_B_ab-facts.json [--out ab-diff.json]
       (ARM_A is the serial W=1 control; ARM_B is the parallel arm.)
"""
from __future__ import annotations

import argparse
import json
import re
import sys


NONCE_SUFFIX = re.compile(r"-[0-9a-f]{8}$")


def _strip_nonce(layer_id):
    return NONCE_SUFFIX.sub("", str(layer_id))


def _signature_multiset(layer_ids):
    counts = {}
    for layer_id in layer_ids:
        key = _strip_nonce(layer_id)
        counts[key] = counts.get(key, 0) + 1
    return counts


def compare(facts_a, facts_b):
    checks = {}

    def record(name, a_value, b_value, *, required=True):
        equal = a_value == b_value
        checks[name] = {"a": a_value, "b": b_value, "equal": equal, "required": required}
        return equal

    record("disposition_multiset", facts_a["measured_dispositions"], facts_b["measured_dispositions"])
    record("block_count", facts_a["block_count"], facts_b["block_count"])
    record("swept", facts_a.get("swept_reported"), facts_b.get("swept_reported"))
    record("final_layer_count", facts_a["final_layer_count"], facts_b["final_layer_count"])
    record("surviving_by_prefix", facts_a["surviving_by_prefix"], facts_b["surviving_by_prefix"])
    record("final_by_prefix", facts_a["final_by_prefix"], facts_b["final_by_prefix"])
    record(
        "surviving_signature_multiset",
        _signature_multiset(facts_a["surviving_pre_squash_layer_ids"]),
        _signature_multiset(facts_b["surviving_pre_squash_layer_ids"]),
    )
    record(
        "space_layer_dirs",
        {"before": facts_a["before_layer_dirs"], "after": facts_a["after_layer_dirs"]},
        {"before": facts_b["before_layer_dirs"], "after": facts_b["after_layer_dirs"]},
    )
    record("staging_clean", facts_a.get("staging_after"), facts_b.get("staging_after"))

    # Bonus, not required: exact surviving id-set equality (holds when the two
    # daemons' nonce counters lined up; nonce drift is expected and tolerated).
    exact_equal = (
        sorted(facts_a["surviving_pre_squash_layer_ids"])
        == sorted(facts_b["surviving_pre_squash_layer_ids"])
    )
    checks["exact_surviving_id_set"] = {"equal": exact_equal, "required": False}

    # Guard: the arms must actually have run at different sweep widths, else the
    # "equivalence" is vacuous. Serial control reports 1; parallel reports > 1.
    width_a = facts_a.get("sweep_width_reported")
    width_b = facts_b.get("sweep_width_reported")
    widths_ok = (
        width_a is not None
        and width_b is not None
        and int(width_a) == 1
        and int(width_b) > 1
    )
    checks["arms_differ_in_width"] = {
        "a": width_a, "b": width_b, "serial_vs_parallel": widths_ok, "required": True,
    }

    required_ok = all(
        check.get("equal", True)
        for check in checks.values()
        if check.get("required")
    )
    passed = required_ok and widths_ok
    return {
        "pass": passed,
        "arm_a": {"case_id": facts_a.get("case_id"), "sweep_width": width_a, "params": facts_a.get("params")},
        "arm_b": {"case_id": facts_b.get("case_id"), "sweep_width": width_b, "params": facts_b.get("params")},
        "checks": checks,
    }


def _load(path):
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("arm_a", help="serial W=1 control ab-facts.json")
    ap.add_argument("arm_b", help="parallel arm ab-facts.json")
    ap.add_argument("--out", dest="out")
    args = ap.parse_args()

    diff = compare(_load(args.arm_a), _load(args.arm_b))
    text = json.dumps(diff, indent=2, sort_keys=True)
    print(text)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
        print(f"wrote {args.out}", file=sys.stderr)
    return 0 if diff["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
