#!/usr/bin/env python3
"""A/B + PERF-WIDTH driver for the squash remount-sweep benchmark.

Runs the SAME deterministic bench scenario across sweep widths. The width is the
in-container daemon's ``runtime.layerstack.remount_sweep_width`` config key,
uploaded with the daemon YAML on every create_sandbox: this driver regenerates
``config/bench.yml`` per width and restarts the gateway (NOT
``--rebuild-binary`` — the daemon is packaged once up front), then runs K repeats
and harvests spans (``SQUASH_HARVEST_OBS=1``).

Outputs per width: pooled overlap / sweep-wall / per-migrated distribution
(via analyze_spans.aggregate over harvested observability.ndjson). For a two-width
run it also emits ``ab-diff.json`` (CORR-AB-EQUIV) comparing the serial control
(first width, must be 1) against the parallel arm (second width).

Examples:
  # CORR-AB-EQUIV on the controlled case, W=1 vs cores:
  ab_driver.py --case AB-EQUIV --label ab --widths 1,CORES --repeats 1
  # PERF-WIDTH tuning curve at N=200 all-migrate:
  ab_driver.py --case PERF-WIDTH --label width --widths 1,2,4,8,16 --repeats 3
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

import analyze_spans

SCRIPT_DIR = Path(__file__).resolve().parent
EXPT_DIR = SCRIPT_DIR.parent
REPO_ROOT = Path(
    subprocess.run(
        ["git", "-C", str(SCRIPT_DIR), "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
)
SUITE_REPORTS = REPO_ROOT / "cli-operation-e2e-live-test/manager/management/squash/test-reports"
BENCH_TEMPLATE = REPO_ROOT / "config/bench.yml"
GATEWAY = REPO_ROOT / "bin/start-sandbox-docker-gateway"
SANDBOX_CLI = REPO_ROOT / "bin/sandbox-cli"


def log(message):
    print(f"[ab_driver] {message}", flush=True)


def resolve_cores(image):
    proc = subprocess.run(
        ["docker", "run", "--rm", image, "nproc"],
        capture_output=True, text=True, timeout=120,
    )
    cores = int(proc.stdout.strip())
    log(f"resolved CORES={cores} (in-container nproc, image={image})")
    return cores


def parse_widths(spec, cores):
    widths = []
    for token in spec.split(","):
        token = token.strip()
        if not token:
            continue
        widths.append(cores if token.upper() == "CORES" else int(token))
    return widths


def write_bench_config(width, generated_dir):
    generated_dir.mkdir(parents=True, exist_ok=True)
    generated = generated_dir / f"bench-W{width}.yml"
    template = BENCH_TEMPLATE.read_text(encoding="utf-8")
    rendered = template.replace("__SWEEP_WIDTH__", str(width)).replace(
        "__DAEMON_CONFIG_PATH__", str(generated)
    )
    generated.write_text(rendered, encoding="utf-8")
    return generated


def restart_gateway(config_path):
    env = dict(os.environ)
    env["SANDBOX_GATEWAY_CONFIG_YAML"] = str(config_path)
    env["PATH"] = f"{REPO_ROOT / 'bin'}:{env.get('PATH', '')}"
    proc = subprocess.run(
        [str(GATEWAY)], cwd=str(REPO_ROOT), env=env,
        capture_output=True, text=True, timeout=600,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"gateway restart failed: {proc.stdout}\n{proc.stderr}")
    for _ in range(60):
        probe = subprocess.run(
            [str(SANDBOX_CLI), "manager", "list_sandboxes"],
            cwd=str(REPO_ROOT), env=env, capture_output=True, text=True, timeout=30,
        )
        if probe.returncode == 0:
            return
        time.sleep(0.5)
    raise RuntimeError("gateway did not become ready after restart")


def run_repeat(case, run_id, config_path, overrides, pytest_bin):
    env = dict(os.environ)
    env["PATH"] = f"{REPO_ROOT / 'bin'}:{env.get('PATH', '')}"
    env["SANDBOX_GATEWAY_CONFIG_YAML"] = str(config_path)
    env["SQUASH_HARVEST_OBS"] = "1"
    env["SQUASH_RUN_ID"] = run_id
    env.update(overrides)
    node = f"cli-operation-e2e-live-test/manager/management/squash/test_squash_bench.py::test_squash_bench_catalog[{case}]"
    log(f"pytest {run_id} :: {case}")
    proc = subprocess.run(
        [pytest_bin, node, "-s", "-q"],
        cwd=str(REPO_ROOT), env=env, timeout=7200,
    )
    return proc.returncode


def read_json(path):
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, json.JSONDecodeError):
        return None


def analyze_run(report_dir):
    ndjson = sorted(report_dir.glob("observability.ndjson*"))
    invocations = analyze_spans.analyze(analyze_spans.load_spans([str(p) for p in ndjson])) if ndjson else []
    return invocations


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--case", required=True)
    ap.add_argument("--label", required=True)
    ap.add_argument("--widths", required=True, help="comma list; token CORES resolves to in-container nproc")
    ap.add_argument("--repeats", type=int, default=1)
    ap.add_argument("--sessions", type=int)
    ap.add_argument("--ratio", type=float)
    ap.add_argument("--blocks", type=int)
    ap.add_argument("--image", default="ubuntu:24.04")
    ap.add_argument("--pytest", default="/opt/homebrew/bin/pytest")
    ap.add_argument("--outdir", default=str(EXPT_DIR / "wtuning"))
    ap.add_argument("--no-restart", action="store_true", help="assume gateway already at a fixed width")
    args = ap.parse_args()

    cores = resolve_cores(args.image)
    widths = parse_widths(args.widths, cores)
    generated_dir = Path(args.outdir) / "generated"
    outdir = Path(args.outdir)
    outdir.mkdir(parents=True, exist_ok=True)

    overrides = {}
    if args.sessions is not None:
        overrides["SQUASH_AB_SESSIONS"] = str(args.sessions)
    if args.ratio is not None:
        overrides["SQUASH_MIGRATE_RATIO"] = str(args.ratio)
    if args.blocks is not None:
        overrides["SQUASH_BLOCK_COUNT"] = str(args.blocks)
    overrides["SQUASH_BENCH_REPEATS"] = str(args.repeats)

    summary = {"case": args.case, "label": args.label, "cores": cores, "widths": {}, "overrides": overrides}
    arm_facts = {}
    for width in widths:
        config_path = write_bench_config(width, generated_dir)
        if not args.no_restart:
            log(f"restarting gateway at width={width} config={config_path}")
            restart_gateway(config_path)
        per_width_invocations = []
        repeats_info = []
        for repeat in range(1, args.repeats + 1):
            run_id = f"{args.label}-W{width}-r{repeat}"
            rc = run_repeat(args.case, run_id, config_path, overrides, args.pytest)
            report_dir = SUITE_REPORTS / run_id / args.case
            facts = read_json(report_dir / "ab-facts.json")
            verdict = read_json(report_dir / "verdict.json")
            invocations = analyze_run(report_dir)
            per_width_invocations.extend(invocations)
            t_squash = (verdict or {}).get("timers", {}).get("T_squash", {}).get("ms")
            repeats_info.append({
                "run_id": run_id,
                "returncode": rc,
                "status": (verdict or {}).get("status"),
                "report_dir": str(report_dir),
                "sweep_width_reported": (facts or {}).get("sweep_width_reported"),
                "migrated": (facts or {}).get("migrated"),
                "block_count": (facts or {}).get("block_count"),
                "T_squash_ms": t_squash,
            })
            if facts is not None and width not in arm_facts:
                arm_facts[width] = report_dir / "ab-facts.json"
        summary["widths"][str(width)] = {
            "repeats": repeats_info,
            "aggregate": analyze_spans.aggregate(per_width_invocations),
        }
        agg = summary["widths"][str(width)]["aggregate"]
        log(f"width={width}: overlap p50={agg.get('overlap_factor', {}).get('p50')} "
            f"sweep_wall p50={agg.get('sweep_wall_ms', {}).get('p50')}ms "
            f"migrated p95={agg.get('migrated_dur_ms', {}).get('p95_ms')}ms "
            f"width_reported={agg.get('sweep_width_reported')}")

    (outdir / f"{args.label}-wtune.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    log(f"wrote {outdir / (args.label + '-wtune.json')}")

    if len(widths) == 2 and all(w in arm_facts for w in widths):
        import ab_compare
        facts_a = read_json(arm_facts[widths[0]])
        facts_b = read_json(arm_facts[widths[1]])
        diff = ab_compare.compare(facts_a, facts_b)
        diff_path = outdir / f"{args.label}-ab-diff.json"
        diff_path.write_text(json.dumps(diff, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        log(f"wrote {diff_path}  CORR-AB-EQUIV pass={diff['pass']}")
        return 0 if diff["pass"] else 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
