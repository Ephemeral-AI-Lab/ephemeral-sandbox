#!/usr/bin/env python3
"""E2/A — OCC batch-window sweep.

See README.md. Threshold: ≥50ms p99 reduction in commit_queue_wait_s vs the
0.002s default on N=8 disjoint workload. Realism gate: baseline p99 ≥ 20ms.
"""

from __future__ import annotations

import argparse
import shutil
import sys
import tempfile
import threading
import time
from concurrent.futures import Future
from dataclasses import dataclass
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT / "backend" / "src"))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import harness  # noqa: E402

from sandbox.layer_stack.stack import LayerStack  # noqa: E402
from sandbox.occ.changeset import (  # noqa: E402
    ChangeSource,
    PreparedChangeset,
    PreparedPathGroup,
    RouteDecision,
    WriteChange,
    WritePayload,
)
from sandbox.occ.commit_queue import CommitQueue  # noqa: E402
from sandbox.occ.commit_transaction import CommitTransaction  # noqa: E402
from sandbox.shared.timing_keys import TimingKey  # noqa: E402


# Production targets per plan §1.
PROD_P99_S = 0.0475  # 47.5ms globally; tail max 332-804ms on hottest scenarios
REALISM_THRESHOLD_S = 0.020  # half of prod p99
PROMOTION_REDUCTION_S = 0.050  # 50ms threshold from plan §6 E2/A
DEFAULT_BATCH_WINDOW = 0.002


def make_prepared_changeset(*, path: str, content: bytes) -> PreparedChangeset:
    """Build a disjoint-path DIRECT changeset for one submitter call."""
    change = WriteChange(
        path=path,
        source=ChangeSource.API_WRITE,
        payload=WritePayload(content=content),
        base_hash=None,
    )
    group = PreparedPathGroup(
        path=path,
        route=RouteDecision.DIRECT,
        changes=(change,),
    )
    return PreparedChangeset(
        snapshot=None,
        path_groups=(group,),
        atomic=False,
        timings={},
        changeset_id=f"e2a-{path}",
    )


def run_workload(
    *,
    queue: CommitQueue,
    num_submitters: int,
    commits_per_submitter: int,
    submitter_prefix: str,
) -> list[float]:
    """Launch N submitters that race to submit M commits each.

    Each commit gets a unique path so batches are disjoint and can be combined.
    Returns the list of commit_queue_wait_s values from all completed commits.
    """
    wait_times: list[float] = []
    lock = threading.Lock()
    barrier = threading.Barrier(num_submitters)
    submitter_futures: list[list[Future]] = [[] for _ in range(num_submitters)]

    def submitter(idx: int) -> None:
        # Synchronize all submitters so they all hit `submit` at ≈ same time —
        # this is what produces queue-depth pressure.
        barrier.wait()
        for commit_idx in range(commits_per_submitter):
            path = f"{submitter_prefix}/s{idx:02d}/c{commit_idx:04d}.txt"
            content = f"submitter={idx} commit={commit_idx}".encode("utf-8")
            prepared = make_prepared_changeset(path=path, content=content)
            fut = queue.submit(prepared)
            submitter_futures[idx].append(fut)

    threads = [
        threading.Thread(target=submitter, args=(i,), name=f"sub-{i:02d}")
        for i in range(num_submitters)
    ]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    # Collect wait times
    for futures in submitter_futures:
        for fut in futures:
            result = fut.result()
            wait_s = result.timings.get(TimingKey.COMMIT_QUEUE_WAIT, 0.0)
            wait_times.append(float(wait_s))

    return wait_times


def bench_one_window(
    *,
    storage_root: Path,
    batch_window_s: float,
    num_submitters: int,
    commits_per_submitter: int,
    iters: int,
    warmup: int,
) -> harness.Stats:
    """Run the workload `iters+warmup` times with a CommitQueue at the given window."""
    layer_stack = LayerStack(storage_root / f"ls_{batch_window_s:g}")
    transaction = CommitTransaction(
        snapshot_reader=layer_stack,
        staging=layer_stack,
        publisher=layer_stack,
    )
    queue = CommitQueue(transaction, batch_window_s=batch_window_s)
    queue.start()
    samples: list[float] = []
    try:
        for run_idx in range(warmup + iters):
            prefix = f"win{batch_window_s:g}/run{run_idx:04d}"
            wait_times = run_workload(
                queue=queue,
                num_submitters=num_submitters,
                commits_per_submitter=commits_per_submitter,
                submitter_prefix=prefix,
            )
            if run_idx >= warmup:
                samples.extend(wait_times)
        return harness.Stats.from_samples(samples)
    finally:
        queue.close()
        layer_stack.close()


def main() -> int:
    parser = argparse.ArgumentParser(description="E2/A OCC batch-window sweep")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--n-submitters", type=int, default=8)
    parser.add_argument("--commits-per-submitter", type=int, default=10)
    parser.add_argument("--iters", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument(
        "--windows",
        nargs="+",
        type=float,
        default=[0.0, 1e-5, 1e-4, 5e-4, 1e-3, 2e-3, 5e-3, 1e-2],
        help="batch_window_s values to sweep (default in plan)",
    )
    args = parser.parse_args()

    print(
        f"E2/A bench — N={args.n_submitters}, "
        f"commits/sub={args.commits_per_submitter}, "
        f"iters={args.iters}, warmup={args.warmup}, "
        f"windows={args.windows}"
    )

    tmpdir = Path(tempfile.mkdtemp(prefix="e2a_"))
    print(f"workdir: {tmpdir}")
    try:
        per_window: dict[float, harness.Stats] = {}
        for w in args.windows:
            print(f"\n--- batch_window_s = {w:g}s ---")
            t0 = time.perf_counter()
            stats = bench_one_window(
                storage_root=tmpdir,
                batch_window_s=w,
                num_submitters=args.n_submitters,
                commits_per_submitter=args.commits_per_submitter,
                iters=args.iters,
                warmup=args.warmup,
            )
            elapsed = time.perf_counter() - t0
            per_window[w] = stats
            print(
                f"  ran in {elapsed:.1f}s. "
                f"queue_wait: n={stats.n}, median={stats.median * 1e3:.3f}ms, "
                f"p95={stats.p95 * 1e3:.3f}ms, p99={stats.p99 * 1e3:.3f}ms, "
                f"max={stats.max_ * 1e3:.3f}ms"
            )

        # Pick baseline = 0.002 (current default)
        if DEFAULT_BATCH_WINDOW not in per_window:
            print(f"FATAL: baseline window {DEFAULT_BATCH_WINDOW} not in sweep")
            return 3
        baseline = per_window[DEFAULT_BATCH_WINDOW]

        # Best alternative window by p99
        non_baseline = {
            w: s for w, s in per_window.items() if w != DEFAULT_BATCH_WINDOW
        }
        best_window, best_stats = min(
            non_baseline.items(), key=lambda kv: kv[1].p99
        )

        reduction = baseline.p99 - best_stats.p99
        threshold_met = reduction >= PROMOTION_REDUCTION_S
        realism_passed = baseline.p99 >= REALISM_THRESHOLD_S

        write_report(
            args.output,
            args=args,
            per_window=per_window,
            baseline=baseline,
            best_window=best_window,
            best_stats=best_stats,
            reduction=reduction,
            threshold_met=threshold_met,
            realism_passed=realism_passed,
        )
        print(f"\nReport written to: {args.output}")
        print()
        print(
            f"  baseline (w={DEFAULT_BATCH_WINDOW}): p99={baseline.p99 * 1e3:.3f}ms"
        )
        print(
            f"  best treatment (w={best_window:g}): p99={best_stats.p99 * 1e3:.3f}ms"
        )
        print(
            f"  reduction: {reduction * 1e3:+.3f}ms "
            f"(target: ≥{PROMOTION_REDUCTION_S * 1e3:.0f}ms) "
            f"{'PASS' if threshold_met else 'FAIL'}"
        )
        print(
            f"  realism gate: baseline p99 = {baseline.p99 * 1e3:.3f}ms "
            f"(target: ≥{REALISM_THRESHOLD_S * 1e3:.0f}ms) "
            f"{'PASS' if realism_passed else 'FAIL'}"
        )
        if not realism_passed:
            print("  VERDICT: INCONCLUSIVE")
            return 2
        print(f"  VERDICT: {'PROMOTED' if threshold_met else 'KILLED'}")
        return 0 if threshold_met else 1
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


def write_report(
    path: Path,
    *,
    args: argparse.Namespace,
    per_window: dict[float, harness.Stats],
    baseline: harness.Stats,
    best_window: float,
    best_stats: harness.Stats,
    reduction: float,
    threshold_met: bool,
    realism_passed: bool,
) -> None:
    lines: list[str] = []
    lines.append(
        harness.yaml_front_matter(
            {
                "experiment": "E2A-batch-window-sweep",
                "n_submitters": args.n_submitters,
                "commits_per_submitter": args.commits_per_submitter,
                "iters_per_window": args.iters,
                "warmup_per_window": args.warmup,
                "windows": str(args.windows),
                "production_p99_ms": PROD_P99_S * 1e3,
            }
        )
    )
    lines.append("# E2/A — OCC batch-window sweep report\n")
    lines.append(
        "**Plan:** docs/plans/sandbox_perf_experiments_PLAN.md §6 E2/A.  \n"
        "See README.md in this directory for design and threshold rationale.\n"
    )
    lines.append("\n## Verdict\n")
    if not realism_passed:
        lines.append(
            f"**VERDICT: INCONCLUSIVE** — baseline p99 = **{baseline.p99 * 1e3:.3f}ms** "
            f"(realism gate requires ≥{REALISM_THRESHOLD_S * 1e3:.0f}ms; "
            f"production p99 = {PROD_P99_S * 1e3:.1f}ms). The workstation cannot "
            "reproduce the production queue-wait tail — workstation SSDs do not "
            "exhibit the filesystem-stall behaviour that drives the prod tail. "
            "The microbench cannot prove or falsify the production hypothesis.\n\n"
            "Recommendation per plan §6: do not ship a batch-window change based "
            "on this run alone. Either rerun inside the production provider container "
            "(where the publisher experiences real disk pressure) or accept that "
            "batch_window_s tuning is not load-bearing for the prod tail.\n"
        )
    elif threshold_met:
        lines.append(
            f"**VERDICT: PROMOTED** — batch_window_s = {best_window:g}s reduces "
            f"`commit_queue_wait_s` p99 by **{reduction * 1e3:.3f}ms** vs the "
            f"{DEFAULT_BATCH_WINDOW:g}s default (target ≥{PROMOTION_REDUCTION_S * 1e3:.0f}ms). "
            f"Single-line change with zero correctness risk per plan §6. "
            "Ship via env override; integration PR is one default constant change.\n"
        )
    else:
        lines.append(
            f"**VERDICT: KILLED** — best alternative window ({best_window:g}s) "
            f"reduces p99 by only **{reduction * 1e3:.3f}ms** vs baseline "
            f"({DEFAULT_BATCH_WINDOW:g}s). Threshold of ≥{PROMOTION_REDUCTION_S * 1e3:.0f}ms "
            "not met. Per plan §6, no E2/A integration ships from this run. "
            "Either E2/B (pipelined publisher) is required, or the 332-804ms tails "
            "are filesystem-stall artifacts not addressable from inside the batcher.\n"
        )

    lines.append("\n## Sweep results\n")
    lines.append("| batch_window_s | n | median ms | p95 ms | p99 ms | max ms | Δp99 vs default ms |")
    lines.append("|---:|---:|---:|---:|---:|---:|---:|")
    for w in sorted(per_window):
        s = per_window[w]
        delta = (s.p99 - baseline.p99) * 1e3
        sign = "+" if delta >= 0 else ""
        marker = "  ← **default**" if w == DEFAULT_BATCH_WINDOW else ""
        lines.append(
            f"| {w:g} | {s.n} | {s.median * 1e3:.3f} | {s.p95 * 1e3:.3f} | "
            f"{s.p99 * 1e3:.3f} | {s.max_ * 1e3:.3f} | {sign}{delta:.3f}{marker} |"
        )

    lines.append("\n## Ceiling reasoning\n")
    lines.append(
        f"Per `commit_queue.py:132-167`, the batch window adds at most one "
        f"`time.sleep(batch_window_s)` per batch — i.e. ≤ batch_window_s overhead **per batch**, "
        f"not per item. Even if every commit produced its own batch, "
        f"the ceiling on what tuning can save from this 2ms window is **~2ms per batch**. "
        f"Production p99 = {PROD_P99_S * 1e3:.1f}ms; tail max up to 804ms. "
        "Therefore the batch window cannot be the load-bearing cause of the tail. "
        "If this bench shows a small-but-real reduction, ship-or-skip on whether the "
        "marginal gain justifies the operational change. If this bench shows no "
        "reduction, the prod tail is sourced elsewhere (publisher latency, filesystem "
        "stalls), and E2/B or different optimization paths are required.\n"
    )

    lines.append("\n## Methodology\n")
    lines.append(
        f"- Setup: fresh `LayerStack` on local disk; `CommitTransaction` wraps it; "
        f"one `CommitQueue` per swept batch_window_s value (drained + closed between conditions).\n"
        f"- Workload per iteration: N={args.n_submitters} threads synchronize on a "
        f"`threading.Barrier`, then each thread submits "
        f"{args.commits_per_submitter} `PreparedChangeset` items targeting disjoint "
        f"paths (DIRECT route, single WriteChange per group).\n"
        f"- Per-window: {args.warmup} warmup + {args.iters} timed iters.\n"
        f"- Sample = per-commit `occ.serial.queue_wait_s` (TimingKey.COMMIT_QUEUE_WAIT). "
        f"Stats are over the full sample set across iters: "
        f"{args.iters} × {args.n_submitters} × {args.commits_per_submitter} = "
        f"{args.iters * args.n_submitters * args.commits_per_submitter} samples per window.\n"
        f"- Paths are unique per (iter, thread, commit) tuple so batches stay disjoint "
        f"and the batcher's path-collision predicate never falsely defers an item.\n"
    )

    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    sys.exit(main())
