"""CLI entrypoint for the SWE-EVO benchmark.

Examples:
    # List available instances
    python -m benchmarks.sweevo --list

    # Run a specific instance end-to-end (provision sandbox + F2P/P2P grading)
    python -m benchmarks.sweevo --instance-id iterative__dvc_1.0.0a1_1.0.0a2

    # Auto-pick a medium-sized instance near target bullet count
    python -m benchmarks.sweevo --size medium --target-bullets 10
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import re
import sys
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterator

from benchmarks.sweevo.dataset import load_sweevo_dataset, summarize_sweevo_instance
from benchmarks.sweevo.models import (
    _DEFAULT_DATASET_SOURCE,
    _DEFAULT_TARGET_BULLETS,
    _REPO_DIR,
)
from benchmarks.sweevo.team_runner import _SWEEVO_TEAM_NAME

# MultiAgentEventPrinter and run_sweevo_with_agent are imported lazily inside
# _cmd_run so that ``--help`` / ``--list`` still work in minimal envs without
# the full providers dependency tree.

_PROJECT_ROOT = Path(__file__).resolve().parents[4]
_ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
_TEAM_RUN_SUFFIX_RE = re.compile(r"[^A-Za-z0-9_.-]+")


class _AnsiStrippingTee:
    """Mirror writes to the terminal and a plain-text run log."""

    def __init__(self, primary: Any, mirror: Any) -> None:
        self._primary = primary
        self._mirror = mirror
        self._primary_broken = False
        self.encoding = getattr(primary, "encoding", "utf-8")
        self.errors = getattr(primary, "errors", "strict")

    def write(self, data: str) -> int:
        if self._primary_broken:
            written = len(data)
        else:
            try:
                written = self._primary.write(data)
            except BrokenPipeError:
                self._primary_broken = True
                written = len(data)
        try:
            self._mirror.write(_ANSI_ESCAPE_RE.sub("", data))
        except ValueError:
            # Interrupt-driven shutdown can close the log file before late
            # asyncio/aiohttp cleanup messages drain through logging.
            pass
        return written

    def writelines(self, lines: list[str]) -> None:
        for line in lines:
            self.write(line)

    def flush(self) -> None:
        if not self._primary_broken:
            try:
                self._primary.flush()
            except BrokenPipeError:
                self._primary_broken = True
        try:
            self._mirror.flush()
        except ValueError:
            pass

    def isatty(self) -> bool:
        return bool(getattr(self._primary, "isatty", lambda: False)())

    def __getattr__(self, name: str) -> Any:
        return getattr(self._primary, name)


def _utc_run_time() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%d-%H-%M")


def _benchmark_dir(team_run_id: str) -> Path:
    return _PROJECT_ROOT / ".ephemeralos" / "team-runs" / team_run_id / "benchmark"


def _build_run_log_path(team_run_id: str, time: str) -> Path:
    return _benchmark_dir(team_run_id) / f"{time}_run.log"


def _build_code_intelligence_log_path(team_run_id: str, time: str) -> Path:
    return _benchmark_dir(team_run_id) / f"{time}_run.code-intelligence.log"


def _build_structured_log_path(team_run_id: str, time: str) -> Path:
    return _benchmark_dir(team_run_id) / f"{time}_run.events.jsonl"


def _team_run_suffix(team_name: str) -> str:
    raw = str(team_name or "").strip() or _SWEEVO_TEAM_NAME
    return _TEAM_RUN_SUFFIX_RE.sub("_", raw).strip("._-") or _SWEEVO_TEAM_NAME


def _append_jsonl(path: Path | None, event: dict[str, Any]) -> None:
    if path is None:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(event, default=str) + "\n")


def _build_file_handler(path: Path, *, level: int) -> logging.FileHandler:
    handler = logging.FileHandler(path, encoding="utf-8")
    handler.setLevel(level)
    handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s"))
    return handler


@contextmanager
def _capture_run_output(team_run_id: str, time: str) -> Iterator[Path]:
    log_path = _build_run_log_path(team_run_id, time)
    log_path.parent.mkdir(parents=True, exist_ok=True)

    original_stdout = sys.stdout
    original_stderr = sys.stderr

    with log_path.open("w", encoding="utf-8", buffering=1) as log_file:
        sys.stdout = _AnsiStrippingTee(original_stdout, log_file)
        sys.stderr = _AnsiStrippingTee(original_stderr, log_file)
        try:
            yield log_path
        finally:
            try:
                sys.stdout.flush()
                sys.stderr.flush()
            finally:
                sys.stdout = original_stdout
                sys.stderr = original_stderr


@contextmanager
def _capture_code_intelligence_logs(
    team_run_id: str,
    time: str,
    *,
    verbose: bool,
) -> Iterator[Path]:
    log_path = _build_code_intelligence_log_path(team_run_id, time)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_level = logging.DEBUG if verbose else logging.INFO
    handler = _build_file_handler(log_path, level=log_level)

    managed_loggers = [
        logging.getLogger("code_intelligence"),
        logging.getLogger("server.routers.code_intelligence"),
    ]
    original_levels = {logger.name: logger.level for logger in managed_loggers}
    original_propagates = {logger.name: logger.propagate for logger in managed_loggers}

    for logger in managed_loggers:
        logger.addHandler(handler)
        logger.setLevel(log_level)
        logger.propagate = False

    try:
        yield log_path
    finally:
        for logger in managed_loggers:
            logger.removeHandler(handler)
            logger.setLevel(original_levels[logger.name])
            logger.propagate = original_propagates[logger.name]
        handler.close()


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="python -m benchmarks.sweevo",
        description="Run the SWE-EVO benchmark on a selected instance.",
    )
    p.add_argument("--source", default=_DEFAULT_DATASET_SOURCE, help="HF dataset id or .parquet path")
    p.add_argument("--instance-id", default=None, help="Exact instance_id to run")
    p.add_argument("--size", default="medium", choices=["small", "medium", "large", "any"])
    p.add_argument("--target-bullets", type=int, default=_DEFAULT_TARGET_BULLETS)
    p.add_argument("--list", action="store_true", help="List available instances and exit")
    p.add_argument("--repo-dir", default=_REPO_DIR)
    p.add_argument("--snapshot-name", default="")
    p.add_argument("--sandbox-name", default="")
    p.add_argument(
        "--team",
        "--team-name",
        dest="team_name",
        default=_SWEEVO_TEAM_NAME,
        help=(
            "Team definition name to run for fresh benchmark runs "
            f"(default: {_SWEEVO_TEAM_NAME})"
        ),
    )
    snapshot_group = p.add_mutually_exclusive_group()
    snapshot_group.add_argument(
        "--register-snapshot",
        dest="register_snapshot",
        action="store_true",
        help="Register a Daytona snapshot from the SWE-EVO image before sandbox creation.",
    )
    snapshot_group.add_argument(
        "--no-register-snapshot",
        dest="register_snapshot",
        action="store_false",
        help="Create the sandbox directly from the SWE-EVO image instead of registering a snapshot.",
    )
    p.set_defaults(register_snapshot=True)
    p.add_argument("--cpu", type=int, default=2)
    p.add_argument("--disk", type=int, default=10)
    p.add_argument("--no-stream", action="store_true", help="Disable live line streaming")
    p.add_argument("--no-color", action="store_true")
    p.add_argument("-v", "--verbose", action="store_true")
    return p


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


def _collect_health_issues(result: dict[str, Any]) -> list[str]:
    team_status = str(result.get("team_status") or "unknown")
    health_issues: list[str] = []
    if team_status != "succeeded":
        health_issues.append(f"team_status={team_status}")

    grading = result.get("grading") or {}
    if grading:
        f2p_passed = int(grading.get("fail_to_pass_passed") or 0)
        f2p_total = int(grading.get("fail_to_pass_total") or 0)
        p2p_broken = int(grading.get("pass_to_pass_broken") or 0)
        p2p_total = int(grading.get("pass_to_pass_total") or 0)
        if f2p_total > 0 and f2p_passed < f2p_total:
            health_issues.append(f"f2p={f2p_passed}/{f2p_total}")
        if p2p_total > 0 and p2p_broken > 0:
            health_issues.append(f"p2p_broken={p2p_broken}/{p2p_total}")

    return health_issues


async def _cmd_run(args: argparse.Namespace, *, team_run_id: str) -> int:
    from message.event_printer import MultiAgentEventPrinter
    from benchmarks.sweevo.runner import run_sweevo_with_agent

    run_log_path = Path(getattr(args, "run_log_path", "")) if getattr(args, "run_log_path", None) else None
    structured_log_path = (
        Path(getattr(args, "structured_log_path", ""))
        if getattr(args, "structured_log_path", None)
        else None
    )
    ci_log_path = (
        Path(getattr(args, "code_intelligence_log_path", ""))
        if getattr(args, "code_intelligence_log_path", None)
        else None
    )
    use_color = not args.no_color
    quiet = args.no_stream
    printer = MultiAgentEventPrinter(
        color=use_color and not quiet,
        truncate=None,
        timestamps=True,
        sink=(lambda _line: None) if quiet else None,
    )

    if not quiet:
        header = "=" * 72
        print(header, flush=True)
        print(
            f"  SWE-EVO run  instance={args.instance_id or f'<auto size={args.size}>'} "
            f"team={args.team_name}",
            flush=True,
        )
        print(header, flush=True)

    _append_jsonl(
        structured_log_path,
        {
            "ts": datetime.now(timezone.utc).isoformat(),
            "event": "run_start",
            "instance_id": args.instance_id,
            "size": args.size,
            "target_bullets": args.target_bullets,
            "team_name": args.team_name,
            "run_log_path": str(run_log_path) if run_log_path is not None else None,
            "structured_log_path": str(structured_log_path) if structured_log_path is not None else None,
            "code_intelligence_log_path": str(ci_log_path) if ci_log_path is not None else None,
        },
    )

    try:
        result = await run_sweevo_with_agent(
            printer=printer,
            team_name=args.team_name,
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
            team_run_id=team_run_id,
            structured_log_path=(
                str(structured_log_path) if structured_log_path is not None else None
            ),
        )
    except Exception as exc:
        _append_jsonl(
            structured_log_path,
            {
                "ts": datetime.now(timezone.utc).isoformat(),
                "event": "run_error",
                "instance_id": args.instance_id,
                "error": str(exc),
            },
        )
        raise

    grading = result.get("grading", {})
    team = result.get("team", {})
    team_status = str(result.get("team_status") or "unknown")
    health_issues = _collect_health_issues(result)
    stream_summary = printer.summary()
    result["health_ok"] = not health_issues
    result["health_issues"] = health_issues
    result["stream_summary"] = stream_summary
    _append_jsonl(
        structured_log_path,
        {
            "ts": datetime.now(timezone.utc).isoformat(),
            "event": "run_finish",
            "instance_id": result.get("instance", {}).get("instance_id", args.instance_id),
            "team_run_id": result.get("team_run_id"),
            "team_status": team_status,
            "health_ok": not health_issues,
            "health_issues": health_issues,
            "grading": grading,
            "team": {
                "work_items": team.get("work_items"),
                "agent_runs": team.get("agent_runs"),
                "replans_used": team.get("replans_used"),
                "usage": team.get("usage"),
                "usage_by_model": team.get("usage_by_model"),
                "agent_run_log_dir": team.get("agent_run_log_dir"),
            },
        },
    )

    if not quiet:
        print("=" * 72, flush=True)
        print(
            f"  agent_events={result.get('agent_events', 0)}  "
            f"team_status={team_status}",
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
        if team:
            usage = team.get("usage") or {}
            usage_by_model = team.get("usage_by_model") or []
            agent_counts = team.get("agent_counts") or {}
            budgets = team.get("budgets") or {}
            print(
                f"  team: work_items={team.get('work_items', result.get('team_work_items', 0))}  "
                f"max_depth={team.get('max_depth_reached', 0)}  "
                f"agent_runs={team.get('agent_runs', 0)}",
                flush=True,
            )
            print(
                f"  run_ids: team_run_id={result.get('team_run_id') or '-'}  "
                f"session_id={team.get('session_id') or '-'}",
                flush=True,
            )
            if team.get("agent_run_log_dir"):
                print(
                    f"  agent_run_logs: {team.get('agent_run_log_dir')}",
                    flush=True,
                )
            print(
                f"  stream: agents={stream_summary['totals']['agents']}  "
                f"tool_calls={stream_summary['totals']['tool_calls']}  "
                f"subagents={stream_summary['totals']['subagents_spawned']}",
                flush=True,
            )
            if usage:
                print(
                    f"  tokens: prompt={usage.get('prompt_tokens', 0)}  "
                    f"completion={usage.get('completion_tokens', 0)}  "
                    f"total={usage.get('total_tokens', 0)}  "
                    f"run_rows={usage.get('run_count', usage.get('call_count', 0))}",
                    flush=True,
                )
            if budgets:
                print(
                    f"  budgets: plan_size={budgets.get('max_plan_size', 0)}  "
                    f"depth={budgets.get('max_depth', 0)}  "
                    f"max_tasks={budgets.get('max_tasks', 0)}",
                    flush=True,
                )
            if agent_counts:
                rendered_counts = " ".join(
                    f"{agent}={count}" for agent, count in sorted(agent_counts.items())
                )
                print(f"  agent_counts: {rendered_counts}", flush=True)
            if usage_by_model:
                rendered_models = " ".join(
                    (
                        f"{entry.get('model_id', '?')}"
                        f"(total={entry.get('total_tokens', 0)},run_rows={entry.get('run_count', entry.get('call_count', 0))})"
                    )
                    for entry in usage_by_model
                )
                print(f"  models: {rendered_models}", flush=True)
        if health_issues:
            print(f"  unhealthy={' ; '.join(health_issues)}", flush=True)
        print("=" * 72, flush=True)
    else:
        # sandbox objects may not be JSON-serializable; coerce via str fallback.
        print(json.dumps(result, indent=2, default=str))

    return 0 if not health_issues else 1


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    if args.list:
        logging.basicConfig(
            level=logging.DEBUG if args.verbose else logging.INFO,
            handlers=[logging.NullHandler()],
            force=True,
        )
        return _cmd_list(args.source)
    time = _utc_run_time()
    team_run_id = f"{time}_{_team_run_suffix(args.team_name)}"
    with _capture_run_output(team_run_id, time) as log_path:
        with _capture_code_intelligence_logs(team_run_id, time, verbose=args.verbose) as ci_log_path:
            root_handler = _build_file_handler(log_path, level=logging.DEBUG if args.verbose else logging.INFO)
            logging.basicConfig(
                level=root_handler.level,
                handlers=[root_handler],
                force=True,
            )
            args.run_log_path = str(log_path)
            args.structured_log_path = str(_build_structured_log_path(team_run_id, time))
            args.code_intelligence_log_path = str(ci_log_path)
            try:
                return asyncio.run(_cmd_run(args, team_run_id=team_run_id))
            except KeyboardInterrupt:
                try:
                    from sandbox.lifecycle import shutdown_cached_client

                    shutdown_cached_client()
                except Exception:
                    logging.getLogger(__name__).debug(
                        "Interrupted run cleanup failed",
                        exc_info=True,
                    )
                print("\nInterrupted.", flush=True)
                return 130


if __name__ == "__main__":
    raise SystemExit(main())
