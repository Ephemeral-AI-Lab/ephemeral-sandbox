"""SWE-EVO benchmark CLI entry: ``python -m task_center_runner.benchmarks.sweevo``."""

from __future__ import annotations

import argparse
import asyncio
import logging
import sys

from task_center_runner.benchmarks.sweevo.models import (
    _DEFAULT_DATASET_SOURCE,
    _REPO_DIR,
)
from task_center_runner.benchmarks.sweevo.pipeline import run_benchmark_sweevo


def _configure_logging() -> None:
    logging.basicConfig(
        level=logging.ERROR,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    logging.disable(logging.WARNING)


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m task_center_runner.benchmarks.sweevo",
        description="Run one SWE-EVO instance through the benchmark_sweevo lifecycle.",
    )
    parser.add_argument("--source", default=_DEFAULT_DATASET_SOURCE)
    parser.add_argument(
        "--instance-id",
        required=True,
        help="Exact SWE-EVO instance_id to run.",
    )
    parser.add_argument("--repo-dir", default=_REPO_DIR)
    parser.add_argument(
        "--csv-path",
        default=None,
        help=(
            "Override the PR descriptions CSV path "
            "(defaults to SWEEVO_PR_DESCRIPTIONS_CSV env or the bundled CSV)."
        ),
    )
    parser.add_argument(
        "--max-duration-s",
        type=float,
        default=10800.0,
        help="Wall-clock cap for the real-agent task_center run (default 3h).",
    )
    parser.add_argument(
        "--audit-dir",
        default=None,
        help="Override audit base dir (defaults to .sweevo_runs/).",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    _configure_logging()
    try:
        return asyncio.run(run_benchmark_sweevo(args))
    except KeyboardInterrupt:
        print("\nInterrupted.", flush=True, file=sys.stderr)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
