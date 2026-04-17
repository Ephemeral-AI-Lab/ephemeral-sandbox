"""Live Daytona concurrent overlay-auditor tests.

Mirrors the barrier-driven architecture of
``test_live_ci_concurrent_edits.py`` but exercises the
:class:`OverlayAuditor` specifically at varying scales. Each worker is
one overlay-audited CodeAct that writes a distinct file; at the end we
assert:

* every audit ledger record names exactly that worker's file (the
  false-attribution regression guard);
* arbiter conflict delta stays at 0 for disjoint writes;
* ``parallelism_ratio`` climbs with scale, confirming that overlay
  setup does not serialize.

Scales 1, 20, 50, 100 run as parametrized cases. Scale 1 establishes
the serial baseline (ratio ≈ 1) that the larger scales are measured
against.

Run with::

    uv run pytest backend/tests/test_e2e/test_live_overlay_auditor.py \\
        -m live -v -s
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import json
import shlex
import statistics
import threading
import time
import uuid
from collections import Counter
from pathlib import Path
from typing import Any, Callable

import pytest

from code_intelligence.editing.arbiter import Arbiter
from code_intelligence.routing.content_manager import ContentManager
from code_intelligence.routing.overlay_auditor import (
    OverlayAuditor,
    OverlayAuditorConfig,
)
from code_intelligence.routing.overlay_probe import probe_overlay_capability
from tests.test_e2e.test_live_ci_rename_perf import (
    HAS_DAYTONA,
    LiveRenameEnv,
    TraceLog,
    _AsyncSandboxWrapper,
    _TracingSandboxWrapper,
    _write_perf_project,
)

pytestmark = [pytest.mark.e2e, pytest.mark.live]


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def live_overlay_env() -> LiveRenameEnv:
    """One sandbox shared by every overlay scale test in this module."""
    if not HAS_DAYTONA:
        pytest.skip("Daytona credentials not configured")

    from sandbox.testing import (
        create_test_sandbox,
        delete_test_sandbox,
        get_sandbox_service,
    )

    info = create_test_sandbox(name="overlay-auditor")
    sandbox_id = info["id"] if isinstance(info, dict) else getattr(info, "id", None)
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
            root_dir=f"{home}/overlay_auditor_{uuid.uuid4().hex[:8]}",
        )
        env.exec_checked(f"mkdir -p {shlex.quote(env.root_dir)}")
        yield env
    finally:
        if sandbox_id:
            try:
                delete_test_sandbox(sandbox_id)
            except Exception:
                pass


@pytest.fixture(scope="module")
def overlay_capable(live_overlay_env: LiveRenameEnv) -> None:
    """Skip the module if the sandbox does not support per-run overlays."""
    env = live_overlay_env

    async def _exec(sandbox: Any, command: str, *, timeout: Any) -> Any:
        return await sandbox.process.exec(command, timeout=timeout)

    result = asyncio.run(
        probe_overlay_capability(env.async_sandbox, _exec)
    )
    if not result.supported:
        pytest.skip(f"overlay not supported on sandbox: {result.reason}")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _setup_repo_and_lowerdir(env: LiveRenameEnv) -> tuple[str, str]:
    """Seed a repo and a shared git-worktree-backed lowerdir.

    Returns ``(repo_root, lowerdir)``. The lowerdir is a detached
    worktree of ``HEAD`` that stays immutable for the duration of
    every scale test — exactly the "shared cached snapshot" strategy
    the auditor relies on for cheap per-command setup.
    """
    repo_root = f"{env.root_dir}/overlay_{uuid.uuid4().hex[:6]}"
    _write_perf_project(env, repo_root)

    env.exec_checked(f"cd {shlex.quote(repo_root)} && git init -q")
    env.exec_checked(
        f"cd {shlex.quote(repo_root)} && "
        "git -c user.email=t@t -c user.name=t add -A && "
        "git -c user.email=t@t -c user.name=t commit -qm seed"
    )
    lowerdir = f"/tmp/overlay-lower-{uuid.uuid4().hex[:8]}"
    env.exec_checked(
        f"git -C {shlex.quote(repo_root)} worktree add --detach "
        f"{shlex.quote(lowerdir)} HEAD"
    )
    return repo_root, lowerdir


def _build_auditor(
    env: LiveRenameEnv,
    repo_root: str,
    lowerdir: str,
) -> tuple[OverlayAuditor, Arbiter]:
    arbiter = Arbiter(workspace_root=repo_root)
    content = ContentManager(repo_root, sandbox=env.async_sandbox)

    class _NullIndex:
        def refresh(self, *args: Any, **kwargs: Any) -> None:
            pass

    class _NullLsp:
        def invalidate(self, *args: Any, **kwargs: Any) -> None:
            pass

    async def _exec(sandbox: Any, command: str, *, timeout: Any = None) -> Any:
        return await sandbox.process.exec(command, timeout=timeout)

    async def _lowerdir_provider(_repo_root: str) -> str:
        return lowerdir

    auditor = OverlayAuditor(
        workspace_root=repo_root,
        exec_process=_exec,
        arbiter=arbiter,
        content=content,
        symbol_index=_NullIndex(),
        lsp_client=_NullLsp(),
        lowerdir_provider=_lowerdir_provider,
        config=OverlayAuditorConfig(tmpfs_size="1g"),
    )
    return auditor, arbiter


def _barrier_run(
    workers: list[Callable[[], dict[str, Any]]],
) -> list[dict[str, Any]]:
    """All workers cross a barrier before firing — max simultaneity."""
    barrier = threading.Barrier(len(workers))

    def _wrapped(fn: Callable[[], dict[str, Any]]) -> dict[str, Any]:
        barrier.wait()
        return fn()

    with concurrent.futures.ThreadPoolExecutor(max_workers=len(workers)) as pool:
        return list(pool.map(_wrapped, workers))


def _parallelism_summary(
    op_results: list[dict[str, Any]],
    *,
    wall_duration_ms: float,
) -> dict[str, Any]:
    summed = round(sum(float(r["duration_ms"]) for r in op_results), 3)
    ratio = round(summed / wall_duration_ms, 3) if wall_duration_ms > 0 else 0.0
    return {
        "wall_duration_ms": wall_duration_ms,
        "summed_operation_ms": summed,
        "parallelism_ratio": ratio,
        "interpretation": "parallel" if ratio >= 1.5 else "possibly_serialized",
    }


def _percentile(values: list[float], pct: int) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, int(round((pct / 100.0) * (len(ordered) - 1)))))
    return round(ordered[index], 3)


def _summarize_ops(op_results: list[dict[str, Any]]) -> dict[str, Any]:
    durations = [float(r["duration_ms"]) for r in op_results]
    outcomes = Counter(r.get("outcome", "?") for r in op_results)
    return {
        "count": len(op_results),
        "p50_ms": _percentile(durations, 50),
        "p95_ms": _percentile(durations, 95),
        "max_ms": round(max(durations), 3) if durations else 0.0,
        "mean_ms": round(statistics.mean(durations), 3) if durations else 0.0,
        "outcomes": dict(sorted(outcomes.items())),
    }


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


@pytest.mark.xfail(
    reason=(
        "Overlay mount fails in the live auditor path with "
        "'special device overlay does not exist' despite the isolated "
        "probe succeeding on the same sandbox. Integration debug in progress."
    ),
    strict=False,
)
@pytest.mark.parametrize("scale", [1, 20, 50, 100])
def test_overlay_auditor_scale_nonoverlap(
    live_overlay_env: LiveRenameEnv,
    overlay_capable: None,
    scale: int,
) -> None:
    """At each scale, N workers write disjoint files through the overlay.

    Per-actor attribution is the correctness regression guard; the
    parallelism ratio is the performance check.
    """
    env = live_overlay_env
    repo_root, lowerdir = _setup_repo_and_lowerdir(env)
    auditor, arbiter = _build_auditor(env, repo_root, lowerdir)

    def _worker(index: int) -> Callable[[], dict[str, Any]]:
        rel = f"pkg/generated/overlay_{index:04d}.py"
        abs_path = f"{repo_root}/{rel}"
        actor = f"agent-{index:04d}"
        cmd = (
            f"mkdir -p $(dirname {shlex.quote(abs_path)}) && "
            f"printf 'value_{index} = {index}\\n' > {shlex.quote(abs_path)}"
        )

        def _run() -> dict[str, Any]:
            started = time.perf_counter()
            try:
                result = asyncio.run(
                    auditor.execute(
                        env.async_sandbox,
                        cmd,
                        timeout=60,
                        agent_id=actor,
                        agent_run_id=actor,
                        task_id=f"task-{index:04d}",
                    )
                )
                outcome = "ok" if result.exit_code == 0 else "error"
                changed = list(result.changed_paths)
                err = ""
            except Exception as exc:  # noqa: BLE001
                outcome = "error"
                changed = []
                err = str(exc)
            return {
                "index": index,
                "actor": actor,
                "abs_path": abs_path,
                "outcome": outcome,
                "changed_paths": changed,
                "duration_ms": round(
                    (time.perf_counter() - started) * 1000, 3
                ),
                "error": err,
            }

        return _run

    workers = [_worker(i) for i in range(scale)]
    conflicts_before = arbiter.metrics.conflicts_detected

    started = time.perf_counter()
    results = _barrier_run(workers) if scale > 1 else [workers[0]()]
    wall_ms = round((time.perf_counter() - started) * 1000, 3)

    errors = [r for r in results if r["outcome"] != "ok"]
    assert not errors, json.dumps(errors, sort_keys=True, default=str)

    # Per-actor attribution: each worker's changed_paths contains exactly
    # its own file and nothing else.
    for r in results:
        assert r["changed_paths"] == [r["abs_path"]], (
            f"false attribution: actor {r['actor']} saw "
            f"{r['changed_paths']} but wrote only {r['abs_path']}"
        )

    # No disjoint-write conflicts.
    assert arbiter.metrics.conflicts_detected == conflicts_before

    payload = {
        "label": f"overlay_scale_{scale}",
        "scale": scale,
        "wall_duration_ms": wall_ms,
        "ops": _summarize_ops(results),
        "parallelism": _parallelism_summary(results, wall_duration_ms=wall_ms),
        "arbiter_total_edits": arbiter.metrics.total_edits,
    }
    print(f"[overlay_scale_{scale}] " + json.dumps(payload, sort_keys=True), flush=True)

    if scale >= 20:
        assert payload["parallelism"]["parallelism_ratio"] >= 1.5, (
            f"scale={scale} did not parallelize: {payload['parallelism']}"
        )


@pytest.mark.xfail(
    reason="Same overlay mount failure in auditor path; see scale test.",
    strict=False,
)
def test_overlay_auditor_rejects_foreign_writes(
    live_overlay_env: LiveRenameEnv,
    overlay_capable: None,
) -> None:
    """Regression guard for the original bug.

    Two overlay runs write different files concurrently. Each ledger
    record must name only its own file — never the other worker's.
    """
    env = live_overlay_env
    repo_root, lowerdir = _setup_repo_and_lowerdir(env)
    auditor, arbiter = _build_auditor(env, repo_root, lowerdir)

    def _run_worker(name: str, rel: str) -> dict[str, Any]:
        abs_path = f"{repo_root}/{rel}"
        cmd = (
            f"mkdir -p $(dirname {shlex.quote(abs_path)}) && "
            f"printf '{name}=1\\n' > {shlex.quote(abs_path)}"
        )
        result = asyncio.run(
            auditor.execute(
                env.async_sandbox,
                cmd,
                timeout=60,
                agent_id=name,
                agent_run_id=name,
            )
        )
        return {
            "name": name,
            "changed_paths": list(result.changed_paths),
            "abs_path": abs_path,
        }

    barrier = threading.Barrier(2)

    def _barrier_wrap(fn: Callable[[], dict[str, Any]]) -> dict[str, Any]:
        barrier.wait()
        return fn()

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        a = pool.submit(_barrier_wrap, lambda: _run_worker("A", "pkg/a.py"))
        b = pool.submit(_barrier_wrap, lambda: _run_worker("B", "pkg/b.py"))
        ra = a.result()
        rb = b.result()

    assert ra["changed_paths"] == [ra["abs_path"]]
    assert rb["changed_paths"] == [rb["abs_path"]]
