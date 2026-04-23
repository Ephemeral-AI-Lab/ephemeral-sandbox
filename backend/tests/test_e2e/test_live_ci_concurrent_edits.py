"""Live Daytona concurrent edit tests for CI process-audit edge cases.

Complements ``test_live_ci_rename_perf.py`` and
``test_live_daytona_tool_occ_calls.py`` by stressing the audit arbiter and
subprocess-backed Jedi/LSP paths under true racing conditions:

* Non-overlapping concurrent edits across all 4 public write paths
  (``daytona_write_file``, ``daytona_edit_file``, ``daytona_rename_symbol``,
  ``daytona_shell``) on disjoint files — every write must land and
  the process audit must record all edit types.
* Overlapping concurrent edits pairing *different* tool types
  (edit×edit, edit×shell, shell×shell) — final files must remain
  coherent, even though unconditional process writes are last-writer-wins
  rather than reservation conflicts.
All concurrency knobs come from environment variables; none are
hardcoded inside assertions.

Run with::

    uv run pytest backend/tests/test_e2e/test_live_ci_concurrent_edits.py \\
        -m live -v -s
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import json
import os
import re
import shlex
import statistics
import threading
import time
import uuid
from collections import Counter
from pathlib import Path
from typing import Any, Callable

import pytest
from dotenv import load_dotenv

from code_intelligence.routing.service import CodeIntelligenceService
from tools.daytona_toolkit.rename_tool import daytona_rename_symbol
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.shell_tool import daytona_shell
from tools.daytona_toolkit.edit_tool import daytona_edit_file
from tools.daytona_toolkit.tools import daytona_write_file

from tests.test_e2e.test_live_ci_rename_perf import (
    HAS_DAYTONA,
    LiveRenameEnv,
    TraceLog,
    _AsyncSandboxWrapper,
    _TracingSandboxWrapper,
    _percentile,
    _write_perf_project,
)

_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")

pytestmark = [pytest.mark.e2e, pytest.mark.live]


def _env_int(name: str, default: int, *, minimum: int = 1) -> int:
    raw = os.environ.get(name, "").strip()
    if not raw:
        return default
    try:
        value = int(raw)
    except ValueError:
        return default
    return max(minimum, value)


CONCURRENCY = _env_int("CI_LIVE_CONCURRENCY", 12, minimum=4)
OVERLAP_PAIRS = _env_int("CI_LIVE_OVERLAP_PAIRS", 6, minimum=2)
NONOVERLAP_SLOTS = _env_int(
    "CI_LIVE_NONOVERLAP_SLOTS", max(CONCURRENCY, 8), minimum=4
)


@pytest.fixture(scope="module")
def live_edits_env() -> LiveRenameEnv:
    """Single sandbox shared by every test in this module."""
    if not HAS_DAYTONA:
        pytest.skip("Daytona credentials not configured")

    from sandbox.testing import (
        create_test_sandbox,
        delete_test_sandbox,
        get_sandbox_service,
    )

    info = create_test_sandbox(name="ci-lsp-concurrent-live")
    sandbox_id = info["id"]
    try:
        sandbox_svc = get_sandbox_service()
        raw_sandbox = sandbox_svc.get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        trace = TraceLog()
        traced = _TracingSandboxWrapper(raw_sandbox, trace)
        env = LiveRenameEnv(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            traced_sandbox=traced,
            async_sandbox=_AsyncSandboxWrapper(traced),
            trace=trace,
            home=home,
            root_dir=f"{home}/ci_lsp_concurrent_{uuid.uuid4().hex[:8]}",
        )
        env.exec_checked(f"mkdir -p {shlex.quote(env.root_dir)}")
        yield env
    finally:
        delete_test_sandbox(sandbox_id)


def _init_git(env: LiveRenameEnv, root: str) -> None:
    env.exec_checked(
        " && ".join(
            [
                f"git -C {shlex.quote(root)} init -q",
                f"git -C {shlex.quote(root)} config user.email live-ci@example.invalid",
                f"git -C {shlex.quote(root)} config user.name live-ci",
                f"git -C {shlex.quote(root)} add .",
                f"git -C {shlex.quote(root)} commit -q -m init",
            ]
        ),
        timeout=180,
    )


def _build_service(
    env: LiveRenameEnv, root: str, suffix: str
) -> tuple[CodeIntelligenceService, ToolExecutionContext]:
    svc = CodeIntelligenceService(
        sandbox_id=f"{env.sandbox_id}-{suffix}",
        workspace_root=root,
        sandbox=env.traced_sandbox,
    )
    ctx = ToolExecutionContext(
        cwd=Path(root),
        metadata={
            "daytona_sandbox": env.async_sandbox,
            "daytona_cwd": root,
            "repo_root": root,
            "ci_service": svc,
            "arbiter": svc.arbiter,
            "agent_run_id": f"ci-lsp-{suffix}-{uuid.uuid4().hex[:8]}",
            "agent_name": "developer",
            "team_run_id": f"ci-lsp-{suffix}-team-{uuid.uuid4().hex[:8]}",
            "work_item_id": f"work-{suffix}",
            "work_item_started_at": time.time(),
        },
    )
    assert svc.ensure_initialized(wait=True) is True
    assert svc.lsp_client.ensure_ready(
        install_missing=True, languages=("python",)
    )["python"] is True
    assert svc.symbol_index.ensure_built(wait=True, timeout=60.0) is True
    return svc, ctx


def _print_block(label: str, payload: dict[str, Any]) -> None:
    print(
        f"[{label}] " + json.dumps(payload, sort_keys=True, default=str),
        flush=True,
    )


def _summarize_ops(op_results: list[dict[str, Any]]) -> dict[str, Any]:
    durations = [float(item["duration_ms"]) for item in op_results]
    per_op = Counter(item.get("op", "?") for item in op_results)
    outcomes = Counter(item.get("outcome", "?") for item in op_results)
    return {
        "count": len(op_results),
        "p50_ms": _percentile(durations, 50),
        "p95_ms": _percentile(durations, 95),
        "max_ms": round(max(durations), 3) if durations else 0.0,
        "mean_ms": round(statistics.mean(durations), 3) if durations else 0.0,
        "per_op": dict(sorted(per_op.items())),
        "outcomes": dict(sorted(outcomes.items())),
    }


def _parallelism_summary(
    op_results: list[dict[str, Any]],
    *,
    wall_duration_ms: float,
) -> dict[str, Any]:
    summed_ms = round(sum(float(item["duration_ms"]) for item in op_results), 3)
    ratio = round(summed_ms / wall_duration_ms, 3) if wall_duration_ms > 0 else 0.0
    return {
        "wall_duration_ms": wall_duration_ms,
        "summed_operation_ms": summed_ms,
        "parallelism_ratio": ratio,
        "interpretation": (
            "parallel" if ratio >= 3.0 else "possibly_serialized"
        ),
    }


def _barrier_run(
    workers: list[Callable[[], dict[str, Any]]],
) -> list[dict[str, Any]]:
    """Run ``workers`` in parallel threads, all crossing a Barrier first."""
    barrier = threading.Barrier(len(workers))

    def _wrapped(fn: Callable[[], dict[str, Any]]) -> dict[str, Any]:
        barrier.wait()
        return fn()

    with concurrent.futures.ThreadPoolExecutor(max_workers=len(workers)) as pool:
        return list(pool.map(_wrapped, workers))


# ---------------------------------------------------------------------------
# Test A: non-overlapping concurrent edits across all write paths
# ---------------------------------------------------------------------------


def test_concurrent_nonoverlap_edits_across_tools(
    live_edits_env: LiveRenameEnv,
) -> None:
    """All 4 write paths run concurrently on disjoint targets and audit cleanly."""
    env = live_edits_env
    root = f"{env.root_dir}/nonoverlap_{uuid.uuid4().hex[:6]}"
    _write_perf_project(env, root)
    generated_dir = f"{root}/pkg/generated"
    env.exec_checked(f"mkdir -p {shlex.quote(generated_dir)}")

    for index in range(NONOVERLAP_SLOTS):
        if index % 4 == 1:
            env.write_file(f"{generated_dir}/edited_{index}.py", f"seed_{index} = 0\n")
        elif index % 4 == 2:
            env.write_file(
                f"{generated_dir}/rename_{index}.py",
                "\n".join(
                    [
                        f"def sym_{index}(x):",
                        f"    return x + {index}",
                        "",
                        f"def caller_{index}(x):",
                        f"    return sym_{index}(x)",
                        "",
                    ]
                ),
            )

    _init_git(env, root)
    svc, ctx = _build_service(env, root, "nonoverlap")
    arbiter_before = svc.arbiter.metrics.conflicts_detected
    trace_mark = env.trace.mark()

    try:
        def _write_worker(index: int) -> Callable[[], dict[str, Any]]:
            target = f"{generated_dir}/written_{index}.py"

            def _run() -> dict[str, Any]:
                started = time.perf_counter()
                result = asyncio.run(
                    daytona_write_file.execute(
                        daytona_write_file.input_model(
                            file_path=target,
                            content=f"value_{index} = {index}\n",
                        ),
                        ctx,
                    )
                )
                return {
                    "index": index,
                    "op": "write",
                    "outcome": "ok" if not result.is_error else "error",
                    "metadata": dict(result.metadata or {}),
                    "duration_ms": round(
                        (time.perf_counter() - started) * 1000, 3
                    ),
                    "output": result.output,
                }

            return _run

        def _edit_worker(index: int) -> Callable[[], dict[str, Any]]:
            target = f"{generated_dir}/edited_{index}.py"

            def _run() -> dict[str, Any]:
                started = time.perf_counter()
                result = asyncio.run(
                    daytona_edit_file.execute(
                        daytona_edit_file.input_model(
                            file_path=target,
                            old_text=f"seed_{index} = 0",
                            new_text=f"seed_{index} = {index}",
                            description=f"nonoverlap edit #{index}",
                        ),
                        ctx,
                    )
                )
                return {
                    "index": index,
                    "op": "edit",
                    "outcome": "ok" if not result.is_error else "error",
                    "metadata": dict(result.metadata or {}),
                    "duration_ms": round(
                        (time.perf_counter() - started) * 1000, 3
                    ),
                    "output": result.output,
                }

            return _run

        def _rename_worker(index: int) -> Callable[[], dict[str, Any]]:
            def _run() -> dict[str, Any]:
                started = time.perf_counter()
                result = asyncio.run(
                    daytona_rename_symbol.execute(
                        daytona_rename_symbol.input_model(
                            symbol=f"sym_{index}",
                            file_hint=f"rename_{index}.py",
                            new_name=f"sym_{index}_renamed",
                        ),
                        ctx,
                    )
                )
                return {
                    "index": index,
                    "op": "rename",
                    "outcome": "ok" if not result.is_error else "error",
                    "metadata": dict(result.metadata or {}),
                    "duration_ms": round(
                        (time.perf_counter() - started) * 1000, 3
                    ),
                    "output": result.output,
                }

            return _run

        def _shell_worker(index: int) -> Callable[[], dict[str, Any]]:
            rel_path = f"pkg/generated/shell_{index}.py"

            def _run() -> dict[str, Any]:
                started = time.perf_counter()
                code = (
                    f'write({rel_path!r}, "shell_value_{index} = {index}\\n")'
                )
                result = asyncio.run(
                    daytona_shell.execute(
                        daytona_shell.input_model(
                            mode="python",
                            code=code,
                            timeout=180,
                        ),
                        ctx,
                    )
                )
                return {
                    "index": index,
                    "op": "shell",
                    "outcome": "ok" if not result.is_error else "error",
                    "metadata": dict(result.metadata or {}),
                    "duration_ms": round(
                        (time.perf_counter() - started) * 1000, 3
                    ),
                    "output": result.output,
                }

            return _run

        builders: list[Callable[[int], Callable[[], dict[str, Any]]]] = [
            _write_worker,
            _edit_worker,
            _rename_worker,
            _shell_worker,
        ]
        workers: list[Callable[[], dict[str, Any]]] = []
        for index in range(NONOVERLAP_SLOTS):
            builder = builders[index % len(builders)]
            workers.append(builder(index))

        started_at = time.perf_counter()
        telemetry_before = svc.lsp_client.telemetry
        tree_before = svc.tree_cache.stats
        results = _barrier_run(workers)
        duration_ms = round((time.perf_counter() - started_at) * 1000, 3)
        telemetry_after = svc.lsp_client.telemetry
        tree_after = svc.tree_cache.stats

        errors = [item for item in results if item["outcome"] != "ok"]
        assert not errors, json.dumps(errors, sort_keys=True, default=str)

        edits = svc.arbiter.recent_edits(seconds=600)
        counts = Counter(
            str(getattr(item, "edit_type", "") or "") for item in edits
        )
        expected_types = {"write", "edit", "rename", "shell"}
        assert expected_types.issubset(counts), dict(counts)

        conflicts_delta = (
            svc.arbiter.metrics.conflicts_detected - arbiter_before
        )
        assert conflicts_delta == 0, (
            f"Unexpected conflicts: {conflicts_delta} (counts={dict(counts)})"
        )

        payload = {
            "label": "A.nonoverlap_mixed_write_paths",
            "concurrency": len(workers),
            "duration_ms": duration_ms,
            "ops": _summarize_ops(results),
            "parallelism": _parallelism_summary(
                results,
                wall_duration_ms=duration_ms,
            ),
            "arbiter_conflicts_delta": conflicts_delta,
            "arbiter_total_edits": svc.arbiter.metrics.total_edits,
            "lsp_script_runs_delta": (
                telemetry_after.script_runs - telemetry_before.script_runs
            ),
            "tree_cache_hits_delta": tree_after["hits"] - tree_before["hits"],
            "tree_cache_misses_delta": (
                tree_after["misses"] - tree_before["misses"]
            ),
            "tree_cache_size": tree_after["size"],
            "symbol_index_generation": svc.symbol_index.generation,
            "status": svc.status(),
            "trace_summary": env.trace.summary_since(trace_mark),
        }
        _print_block("ci-lsp-concurrent-nonoverlap", payload)
    finally:
        svc.dispose()


# ---------------------------------------------------------------------------
# Test B: overlapping concurrent edits preserve file integrity
# ---------------------------------------------------------------------------


def test_concurrent_overlap_edits_preserve_process_integrity(
    live_edits_env: LiveRenameEnv,
) -> None:
    """Racing process writes on one region leave one coherent final value."""
    env = live_edits_env
    root = f"{env.root_dir}/overlap_{uuid.uuid4().hex[:6]}"
    env.exec_checked(f"mkdir -p {shlex.quote(root)}/pkg")
    env.write_file(f"{root}/pkg/__init__.py", "")
    for pair_index in range(OVERLAP_PAIRS):
        env.write_file(
            f"{root}/pkg/shared_{pair_index}.py",
            f"shared_marker_{pair_index} = 0\n",
        )
    _init_git(env, root)

    svc, ctx = _build_service(env, root, "overlap")
    arbiter_before = svc.arbiter.metrics.conflicts_detected
    trace_mark = env.trace.mark()

    try:
        pair_results: list[dict[str, Any]] = []

        for pair_index in range(OVERLAP_PAIRS):
            target = f"{root}/pkg/shared_{pair_index}.py"
            marker = f"shared_marker_{pair_index}"

            def _edit_variant(
                pair: int, attempt: int, seed: str, path: str
            ) -> Callable[[], dict[str, Any]]:
                def _run() -> dict[str, Any]:
                    started = time.perf_counter()
                    result = asyncio.run(
                        daytona_edit_file.execute(
                            daytona_edit_file.input_model(
                                file_path=path,
                                old_text=f"{seed} = 0",
                                new_text=f"{seed} = {pair}{attempt}",
                                description=(
                                    f"overlap pair {pair} attempt {attempt}"
                                ),
                            ),
                            ctx,
                        )
                    )
                    return {
                        "pair": pair,
                        "attempt": attempt,
                        "op": "edit",
                        "is_error": result.is_error,
                        "metadata": dict(result.metadata or {}),
                        "duration_ms": round(
                            (time.perf_counter() - started) * 1000, 3
                        ),
                        "output": result.output,
                    }

                return _run

            def _shell_variant(
                pair: int, attempt: int, seed: str, path: str
            ) -> Callable[[], dict[str, Any]]:
                rel_path = f"pkg/shared_{pair}.py"

                def _run() -> dict[str, Any]:
                    started = time.perf_counter()
                    new_content = f"{seed} = {pair}{attempt}\n"
                    code = f"write({rel_path!r}, {new_content!r})"
                    result = asyncio.run(
                        daytona_shell.execute(
                            daytona_shell.input_model(
                                mode="python",
                                code=code,
                                timeout=180,
                            ),
                            ctx,
                        )
                    )
                    return {
                        "pair": pair,
                        "attempt": attempt,
                        "op": "shell",
                        "is_error": result.is_error,
                        "metadata": dict(result.metadata or {}),
                        "duration_ms": round(
                            (time.perf_counter() - started) * 1000, 3
                        ),
                        "output": result.output,
                    }

                return _run

            # The unified process operation path audits mutations after the
            # command finishes. It does not pre-reserve a file-level edit token,
            # so unconditional process writes are last-writer-wins. This matrix
            # verifies final file coherence and audit accounting, not legacy
            # prepare/commit conflict rejection.
            combos = [
                (_edit_variant, _edit_variant),
                (_edit_variant, _shell_variant),
                (_shell_variant, _shell_variant),
            ]
            left_builder, right_builder = combos[pair_index % len(combos)]
            workers = [
                left_builder(pair_index, 0, marker, target),
                right_builder(pair_index, 1, marker, target),
            ]
            outcomes = _barrier_run(workers)
            pair_results.append(
                {
                    "pair_index": pair_index,
                    "left_op": outcomes[0]["op"],
                    "right_op": outcomes[1]["op"],
                    "outcomes": outcomes,
                }
            )

        successes_per_pair: list[int] = []
        errors_per_pair: list[int] = []
        error_reasons: Counter[str] = Counter()
        for pair in pair_results:
            wins = [item for item in pair["outcomes"] if not item["is_error"]]
            losses = [item for item in pair["outcomes"] if item["is_error"]]
            successes_per_pair.append(len(wins))
            errors_per_pair.append(len(losses))
            for loss in losses:
                reason = str(loss["metadata"].get("conflict_reason", "")) or (
                    "conflict" if loss["metadata"].get("conflict") else str(loss["output"])
                )
                error_reasons[reason[:120]] += 1

        conflicts_delta = (
            svc.arbiter.metrics.conflicts_detected - arbiter_before
        )
        final_values_by_pair: dict[int, list[str]] = {}
        for pair_index in range(OVERLAP_PAIRS):
            target = f"{root}/pkg/shared_{pair_index}.py"
            text = env.read_file(target)
            marker = f"shared_marker_{pair_index}"
            final_values_by_pair[pair_index] = re.findall(
                rf"{re.escape(marker)} = (\d+)",
                text,
            )

        payload = {
            "label": "B.overlap_process_write_integrity",
            "pairs": OVERLAP_PAIRS,
            "successes_histogram": dict(Counter(successes_per_pair)),
            "errors_histogram": dict(Counter(errors_per_pair)),
            "error_reasons": dict(error_reasons),
            "final_values_by_pair": final_values_by_pair,
            "arbiter_conflicts_delta": conflicts_delta,
            "arbiter_total_edits": svc.arbiter.metrics.total_edits,
            "pair_results": pair_results,
            "trace_summary": env.trace.summary_since(trace_mark),
        }
        _print_block("ci-lsp-concurrent-overlap", payload)

        assert all(count >= 1 for count in successes_per_pair), (
            f"Every racing pair should leave at least one successful write: {payload}"
        )
        assert all(len(values) == 1 for values in final_values_by_pair.values()), (
            f"Torn or missing final state: {final_values_by_pair}"
        )
        assert svc.arbiter.metrics.total_edits >= OVERLAP_PAIRS
    finally:
        svc.dispose()




@pytest.fixture(autouse=True, scope="module")
def _emit_trace_summary(live_edits_env: LiveRenameEnv):
    yield
    live_edits_env.trace.print_all()
    live_edits_env.trace.print_summary()
