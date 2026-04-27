"""CLI entrypoint for running one SWE-EVO instance through TaskCenter.

Example:
    PYTHONPATH=backend/src uv run python -m benchmarks.sweevo \
      --instance-id dask__dask_2023.3.2_2023.4.0 -v
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import sys

from benchmarks.sweevo.dataset import load_sweevo_dataset, summarize_sweevo_instance
from benchmarks.sweevo.models import (
    _DEFAULT_DATASET_SOURCE,
    _DEFAULT_TARGET_BULLETS,
    _REPO_DIR,
)


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m benchmarks.sweevo",
        description="Run a SWE-EVO benchmark instance through TaskCenter.",
    )
    parser.add_argument("--source", default=_DEFAULT_DATASET_SOURCE)
    parser.add_argument("--instance-id", default=None, help="Exact instance_id to run")
    parser.add_argument("--size", default="medium", choices=["small", "medium", "large", "any"])
    parser.add_argument("--target-bullets", type=int, default=_DEFAULT_TARGET_BULLETS)
    parser.add_argument("--list", action="store_true", help="List available instances and exit")
    parser.add_argument("--repo-dir", default=_REPO_DIR)
    parser.add_argument("--snapshot-name", default="")
    parser.add_argument("--sandbox-name", default="")
    snapshot_group = parser.add_mutually_exclusive_group()
    snapshot_group.add_argument("--register-snapshot", dest="register_snapshot", action="store_true")
    snapshot_group.add_argument(
        "--no-register-snapshot",
        dest="register_snapshot",
        action="store_false",
    )
    parser.set_defaults(register_snapshot=True)
    parser.add_argument("--cpu", type=int, default=2)
    parser.add_argument("--disk", type=int, default=10)
    parser.add_argument("--no-evaluate", action="store_true", help="Skip F2P/P2P grading")
    parser.add_argument("--no-stream", action="store_true", help="Print JSON only after completion")
    parser.add_argument("--no-color", action="store_true")
    parser.add_argument("-v", "--verbose", action="store_true")
    return parser


def _cmd_list(source: str) -> int:
    instances = load_sweevo_dataset(source)
    for inst in instances:
        summary = summarize_sweevo_instance(inst)
        print(
            f"{summary['instance_id']}\t"
            f"size={summary['size']}\t"
            f"bullets={summary['bullet_count']}\t"
            f"repo={summary['repo']}"
        )
    print(f"\nTotal: {len(instances)} instances", file=sys.stderr)
    return 0


async def _cmd_run(args: argparse.Namespace) -> int:
    from benchmarks.sweevo.task_center_runner import run_sweevo_with_task_center
    from message.event_printer import MultiAgentEventPrinter

    printer = None
    if not args.no_stream:
        printer = MultiAgentEventPrinter(color=not args.no_color, timestamps=True)
        print("=" * 72, flush=True)
        print(
            f"  SWE-EVO TaskCenter run  instance="
            f"{args.instance_id or f'<auto size={args.size}>'}",
            flush=True,
        )
        print("=" * 72, flush=True)

    result = await run_sweevo_with_task_center(
        printer=printer,
        source=args.source,
        instance_id=args.instance_id,
        size=args.size,
        target_bullets=args.target_bullets,
        snapshot_name=args.snapshot_name,
        sandbox_name=args.sandbox_name,
        register_snapshot=args.register_snapshot,
        cpu=args.cpu,
        disk=args.disk,
        repo_dir=args.repo_dir,
        evaluate=not args.no_evaluate,
    )

    if args.no_stream:
        print(json.dumps(result, indent=2, default=str))
    else:
        grading = result.get("grading") or {}
        print("=" * 72, flush=True)
        print(
            f"  status={result.get('task_center_status')}  "
            f"tasks={result.get('task_count', 0)}  "
            f"events={result.get('agent_events', 0)}  "
            f"duration_s={float(result.get('duration_s') or 0):.1f}",
            flush=True,
        )
        if grading:
            print(
                f"  grading: resolved={grading.get('resolved')}  "
                f"f2p={grading.get('fail_to_pass_passed', 0)}/"
                f"{grading.get('fail_to_pass_total', 0)}  "
                f"p2p_broken={grading.get('pass_to_pass_broken', 0)}/"
                f"{grading.get('pass_to_pass_total', 0)}  "
                f"fix_rate={float(grading.get('fix_rate', 0.0)):.2f}",
                flush=True,
            )
        print("=" * 72, flush=True)

    task_center_ok = result.get("task_center_status") == "done"
    grading = result.get("grading")
    grading_ok = grading is None or bool(grading.get("resolved"))
    return 0 if task_center_ok and grading_ok else 1


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    if args.list:
        return _cmd_list(args.source)
    try:
        return asyncio.run(_cmd_run(args))
    except KeyboardInterrupt:
        print("\nInterrupted.", flush=True)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
