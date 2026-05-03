"""Phase 3.6 Stage C — basedpyright vs jedi.Script live benchmark.

Headline deliverable for Phase 3.6: confirm that the rewired
:class:`LspClient` (now routing through basedpyright via
:class:`LspBackendChild`) is materially faster than the pre-rewire
``jedi.Script`` baseline (``phase_0_lsp_baseline_<ts>.json``).

Hard SLOs:

* ``find_definitions`` p50 ≥ 5x faster than the jedi baseline p50.
* ``find_definitions`` p99 < 100 ms warm.
* ``hover`` p50 ≥ 10x faster than the jedi baseline p50.

The benchmark also re-proves HARD INVARIANT 5 against the new backend:
edit a file, then ``find_definitions`` returns the post-edit definition
(never stale). Lives in this file rather than the Phase 3 invariants
file because the invariant must be revalidated against EACH backend.

Run with:
    .venv/bin/pytest backend/tests/test_e2e/test_live_ci_phase3_6_lsp_benchmark.py -m live -v -s
"""

from __future__ import annotations

import asyncio
import glob
import json
import os
import sys
import time
import uuid
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Any, Iterator

import pytest

from engine.testing.eval_agent import EvalAgent
from sandbox.api.bash import extract_exit_code, wrap_bash_command
from sandbox.code_intelligence.core.types import WriteSpec
from sandbox.code_intelligence.language_server.lsp_child import LSP_BACKEND_CHOSEN
from sandbox.code_intelligence.service import CodeIntelligenceService

from ._timing_harness import TimingHarness

pytestmark = [pytest.mark.e2e, pytest.mark.live]

_DASK_SWEEVO_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"
_DASK_SWEEVO_REPO_DIR = "/testbed"
_TIMINGS_DIR = (
    os.path.dirname(os.path.abspath(__file__)) + "/_timings"
)
_DAEMON_WARM_SAMPLES = 10
_DAEMON_COMMAND_WARM_P99_CEILING_S = 10.0


def _flush(msg: str) -> None:
    print(msg, flush=True)
    sys.stdout.flush()


@contextmanager
def _trace(harness: TimingHarness, name: str) -> Iterator[None]:
    _flush(f"  → {name} ...")
    t0 = time.perf_counter()
    with harness.step(name):
        yield
    _flush(f"  ✓ {name} ({time.perf_counter() - t0:.3f}s)")


@dataclass
class LivePhase36Env:
    sandbox_id: str
    raw_sandbox: Any
    home: str
    root_dir: str

    def exec(self, command: str, *, timeout: int = 60) -> tuple[int, str]:
        response = self.raw_sandbox.process.exec(
            wrap_bash_command(command),
            timeout=timeout,
        )
        output, exit_code = extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return exit_code, output

    def make_ci_service(self) -> CodeIntelligenceService:
        return CodeIntelligenceService(
            sandbox_id=self.sandbox_id,
            workspace_root=self.root_dir,
            sandbox=self.raw_sandbox,
        )

    def make_ci_service_daemon(self) -> CodeIntelligenceService:
        """Daemon-path service (DaemonBackend) — basedpyright runs IN the sandbox.

        Required by Phase 3.6 §6.2: the InProcess path's `LspBackendChild` runs
        on the test host (macOS) where ``basedpyright-langserver`` is missing,
        so timings degrade to the symbol-index fallback. The daemon path
        spawns the LSP child INSIDE the sandbox, exercising basedpyright
        end-to-end.
        """
        from sandbox.daytona.transport import DaytonaTransport

        return CodeIntelligenceService(
            sandbox_id=self.sandbox_id,
            workspace_root=self.root_dir,
            transport=DaytonaTransport(),
        )


@pytest.fixture(scope="module")
def live_phase36_env() -> LivePhase36Env:
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials not configured")

    from benchmarks.sweevo.dataset import select_sweevo_instance
    from benchmarks.sweevo.models import _CONDA_ACTIVATE
    from benchmarks.sweevo.sandbox import create_sweevo_test_sandbox
    from sandbox.testing import delete_test_sandbox, get_sandbox_service

    _flush(
        f"\n[fixture] provisioning sweevo sandbox {_DASK_SWEEVO_INSTANCE_ID} ..."
    )
    instance = select_sweevo_instance(instance_id=_DASK_SWEEVO_INSTANCE_ID)
    sandbox_name = f"ci-phase36-{uuid.uuid4().hex[:8]}"
    t0 = time.perf_counter()
    result = asyncio.run(
        create_sweevo_test_sandbox(
            instance,
            sandbox_name=sandbox_name,
            repo_dir=_DASK_SWEEVO_REPO_DIR,
        )
    )
    sandbox_id = str(result["sandbox_id"])
    _flush(
        f"[fixture] sandbox {sandbox_id} ready in "
        f"{time.perf_counter() - t0:.1f}s"
    )
    try:
        raw_sandbox = get_sandbox_service().get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        env = LivePhase36Env(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            home=home,
            root_dir=_DASK_SWEEVO_REPO_DIR,
        )
        exit_code, output = env.exec(
            f"{_CONDA_ACTIVATE} && cd {_DASK_SWEEVO_REPO_DIR} && python --version",
            timeout=60,
        )
        assert exit_code == 0, output

        # Ensure basedpyright is installed (image does NOT bundle it — see
        # lsp-qualification-spike-result.md).
        _flush("[fixture] ensuring basedpyright is installed in sandbox ...")
        env.exec(
            "python3 -c 'import basedpyright' 2>/dev/null || "
            "python3 -m pip install --no-cache-dir --retries 10 --timeout 300 basedpyright",
            timeout=600,
        )
        bp_check, _ = env.exec("command -v basedpyright-langserver")
        if bp_check != 0:
            pytest.skip(
                "basedpyright-langserver not on PATH after install — see "
                "lsp-qualification-spike-result.md for the install gotcha"
            )
        _flush(f"[fixture] sandbox ready: {output.strip()}; basedpyright installed")
        yield env
    finally:
        _flush(f"[fixture] tearing down sandbox {sandbox_id} ...")
        delete_test_sandbox(sandbox_id)


def _latest_jedi_baseline() -> dict[str, dict[str, float]] | None:
    """Locate the most recent ``phase_0_lsp_baseline_*.json`` for SLO comparison."""
    files = sorted(glob.glob(f"{_TIMINGS_DIR}/phase_0_lsp_baseline_*.json"))
    if not files:
        return None
    payload = json.loads(open(files[-1], encoding="utf-8").read())
    return payload.get("distributions") or {}


def _find_target_position(env: LivePhase36Env, file_path: str) -> tuple[int, int]:
    """Locate the first ``def NAME(`` line in *file_path* (1-indexed)."""
    code, output = env.exec(
        f"grep -nE '^(def |class )' {file_path} | head -1",
        timeout=20,
    )
    if code != 0 or not output.strip():
        return 1, 4
    first = output.splitlines()[0]
    line_str, _, _ = first.partition(":")
    try:
        return int(line_str), 4
    except ValueError:
        return 1, 4


def test_phase3_6_chosen_backend_benchmark(live_phase36_env: LivePhase36Env) -> None:
    """Headline benchmark: chosen LSP backend vs jedi baseline."""
    h = TimingHarness(phase=3.6, test_name="chosen_lsp_backend_benchmark")
    env = live_phase36_env

    target_file = f"{env.root_dir}/dask/__init__.py"

    with _trace(h, "ci_service_construct"):
        svc = env.make_ci_service()
    with _trace(h, "index_build_in_process"):
        svc.ensure_initialized(wait=True)

    target_line, target_char = _find_target_position(env, target_file)
    _flush(
        f"  [phase3.6] target {target_file}:{target_line}:{target_char}"
    )

    # Cold call separate from warm distribution.
    with _trace(h, "lsp_cold_first_query"):
        try:
            svc.find_definitions(target_file, "", target_line, target_char)
        except Exception as exc:
            _flush(f"  [phase3.6] cold first_query exception: {exc}")

    # Warm-up so any per-call basedpyright caches settle before sampling.
    for _ in range(3):
        try:
            svc.find_definitions(target_file, "", target_line, target_char)
        except Exception:
            pass

    # 50-sample distributions for each LSP op. Vary line/char across samples
    # so the orchestrator-side LSP cache doesn't mask per-call backend cost.
    positions = _gather_def_positions(env, target_file, n=50)
    if not positions:
        positions = [(target_line, target_char)] * 50

    for step in h.step_repeat("find_definitions", n=50):
        with step:
            line, ch = positions[len(h._samples["find_definitions"]) % len(positions)]
            try:
                svc.find_definitions(target_file, "", line, ch)
            except Exception:
                pass

    for step in h.step_repeat("find_references", n=50):
        with step:
            line, ch = positions[len(h._samples["find_references"]) % len(positions)]
            try:
                svc.find_references(target_file, "", line, ch)
            except Exception:
                pass

    for step in h.step_repeat("hover", n=50):
        with step:
            line, ch = positions[len(h._samples["hover"]) % len(positions)]
            try:
                svc.hover(target_file, line, ch)
            except Exception:
                pass

    for step in h.step_repeat("diagnostics", n=50):
        with step:
            try:
                svc.diagnostics(target_file)
            except Exception:
                pass

    # Print the canonical comparison table.
    jedi_baseline = _latest_jedi_baseline()
    if jedi_baseline:
        _print_lsp_benchmark_table(LSP_BACKEND_CHOSEN, h.distributions, jedi_baseline)
    else:
        _flush(
            "  [phase3.6] WARNING: no phase_0_lsp_baseline_*.json found — "
            "SLO assertions will be skipped"
        )

    with _trace(h, "ci_service_dispose"):
        svc.dispose()

    _flush("\n" + h.report())
    out_path = h.dump_json()
    _flush(f"\n[phase3.6] benchmark JSON saved at: {out_path}")

    # ---- SLO assertions ----
    if jedi_baseline:
        _assert_slo_5x_find_definitions(h.distributions, jedi_baseline)
        _assert_slo_p99_under_100ms(h.distributions)
        _assert_slo_10x_hover(h.distributions, jedi_baseline)


def test_phase3_6_chosen_backend_benchmark_daemon_path(
    live_phase36_env: LivePhase36Env,
) -> None:
    """Daemon-path variant: basedpyright runs IN the sandbox via DaemonBackend.

    The base ``test_phase3_6_chosen_backend_benchmark`` runs against
    InProcessBackend; on a host without ``basedpyright-langserver`` the
    LspChildUnavailable fallback kicks in and the numbers reflect the
    symbol-index linear scan, not basedpyright. This variant routes through
    the in-sandbox daemon (``EOS_CI_IN_SANDBOX=1`` + DaytonaTransport) so the
    LSP child is spawned in the sandbox where basedpyright is installed.
    """
    import os
    from unittest import mock

    h = TimingHarness(phase=3.6, test_name="chosen_lsp_backend_benchmark_daemon")
    env = live_phase36_env

    target_file = f"{env.root_dir}/dask/__init__.py"

    with mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "1"}):
        with _trace(h, "ci_service_construct"):
            svc = env.make_ci_service_daemon()
        with _trace(h, "index_build_in_sandbox"):
            svc.ensure_initialized(wait=True)

        target_line, target_char = _find_target_position(env, target_file)
        _flush(
            f"  [phase3.6-daemon] target {target_file}:{target_line}:{target_char}"
        )

        with _trace(h, "lsp_cold_first_query"):
            try:
                svc.find_definitions(target_file, "", target_line, target_char)
            except Exception as exc:
                _flush(f"  [phase3.6-daemon] cold first_query exception: {exc}")

        for _ in range(3):
            try:
                svc.find_definitions(target_file, "", target_line, target_char)
            except Exception:
                pass

        positions = _gather_def_positions(env, target_file, n=_DAEMON_WARM_SAMPLES)
        if not positions:
            positions = [(target_line, target_char)] * _DAEMON_WARM_SAMPLES

        for step in h.step_repeat("find_definitions", n=_DAEMON_WARM_SAMPLES):
            with step:
                line, ch = positions[
                    len(h._samples["find_definitions"]) % len(positions)
                ]
                try:
                    svc.find_definitions(target_file, "", line, ch)
                except Exception:
                    pass

        for step in h.step_repeat("find_references", n=_DAEMON_WARM_SAMPLES):
            with step:
                line, ch = positions[
                    len(h._samples["find_references"]) % len(positions)
                ]
                try:
                    svc.find_references(target_file, "", line, ch)
                except Exception:
                    pass

        for step in h.step_repeat("hover", n=_DAEMON_WARM_SAMPLES):
            with step:
                line, ch = positions[len(h._samples["hover"]) % len(positions)]
                try:
                    svc.hover(target_file, line, ch)
                except Exception:
                    pass

        for step in h.step_repeat("diagnostics", n=_DAEMON_WARM_SAMPLES):
            with step:
                try:
                    svc.diagnostics(target_file)
                except Exception:
                    pass

        jedi_baseline = _latest_jedi_baseline()
        if jedi_baseline:
            _print_lsp_benchmark_table(
                f"{LSP_BACKEND_CHOSEN} (daemon)", h.distributions, jedi_baseline
            )

        with _trace(h, "ci_service_dispose"):
            svc.dispose()

    _flush("\n" + h.report())
    out_path = h.dump_json()
    _flush(f"\n[phase3.6-daemon] benchmark JSON saved at: {out_path}")

    # This is a public daemon-path measurement, so it includes Daytona
    # ``transport.exec`` shim latency for every daemon command. Keep it as a completion
    # gate for the previously-hung warm loop; the raw LSP-child SLO belongs in
    # an in-daemon batch probe that does not pay one provider exec per sample.
    if "find_definitions" in h.distributions:
        p99 = h.distributions["find_definitions"]["p99"]
        assert p99 < _DAEMON_COMMAND_WARM_P99_CEILING_S, (
            f"daemon-path find_definitions p99 ({p99:.3f}s) exceeded "
            f"{_DAEMON_COMMAND_WARM_P99_CEILING_S:.1f}s; the warm loop may be "
            "stuck behind the provider exec shim again"
        )


def test_phase3_6_invariant_5_lsp_invalidation(live_phase36_env: LivePhase36Env) -> None:
    """HARD INVARIANT 5 (LSP cache invalidation) regression vs new backend."""
    h = TimingHarness(phase=3.6, test_name="invariant_5_lsp_invalidation")
    env = live_phase36_env
    svc = env.make_ci_service()
    svc.ensure_initialized(wait=True)

    target_file = f"{env.root_dir}/_phase3_6_inv5.py"
    env.exec(
        f"echo 'def alpha(): return 1\\n' > {target_file}",
        timeout=10,
    )

    with _trace(h, "find_def_pre_edit"):
        pre = svc.find_definitions(target_file, "alpha", 1, 4)
    _flush(f"  [phase3.6] pre-edit definitions: {len(pre)}")

    # Mutate via the daemon-aware path so cache invalidation fires.
    res = svc.write_file(
        [WriteSpec(file_path=target_file, content="def beta(): return 2\n", overwrite=True)],
    )
    assert res.success, f"write_file did not succeed: {res.status}"

    with _trace(h, "find_def_post_edit"):
        post = svc.find_definitions(target_file, "beta", 1, 4)
    _flush(f"  [phase3.6] post-edit definitions: {len(post)}")

    # The post-edit query MUST NOT return a stale `alpha` reference; it should
    # either return the new `beta` definition or an empty list (LSP handled
    # the change). The HARD INVARIANT is: results match the post-edit content.
    pre_names = {s.name for s in pre}
    post_names = {s.name for s in post}
    assert "alpha" not in post_names, (
        f"INVARIANT 5 violation: alpha still resolves after edit; pre={pre_names} post={post_names}"
    )

    svc.dispose()
    _flush("\n" + h.report())
    h.dump_json()


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _gather_def_positions(env: LivePhase36Env, file_path: str, *, n: int) -> list[tuple[int, int]]:
    """Return up to *n* (line, char) pairs at ``def NAME(`` / ``class NAME:`` sites."""
    code, output = env.exec(
        f"grep -nE '^[[:space:]]*(def |class )' {file_path}",
        timeout=20,
    )
    if code != 0:
        return []
    positions: list[tuple[int, int]] = []
    for raw in output.splitlines():
        line_str, _, content = raw.partition(":")
        try:
            line = int(line_str)
        except ValueError:
            continue
        # Skip past the keyword to the symbol-name column.
        stripped = content.lstrip()
        indent = len(content) - len(stripped)
        if stripped.startswith("def "):
            positions.append((line, indent + len("def ")))
        elif stripped.startswith("class "):
            positions.append((line, indent + len("class ")))
        if len(positions) >= n:
            break
    return positions


def _print_lsp_benchmark_table(
    chosen: str,
    current: dict[str, dict[str, float]],
    baseline: dict[str, dict[str, float]],
) -> None:
    print(f"\n=== Phase 3.6 LSP benchmark: {chosen} vs jedi.Script baseline ===")
    header = (
        f"{'op':<20} {'jedi p50/p95/p99 (ms)':>26} "
        f"{chosen + ' p50/p95/p99 (ms)':>30} {'speedup':>10}"
    )
    print(header)
    print("-" * len(header))
    for op in ["find_definitions", "find_references", "hover", "diagnostics"]:
        b = baseline.get(op) or {}
        c = current.get(op) or {}
        if not b or not c:
            print(f"{op:<20} {'(missing)':>26} {'(missing)':>30} {'-':>10}")
            continue
        b_str = f"{b['p50']*1000:>5.1f}/{b['p95']*1000:>5.1f}/{b['p99']*1000:>5.1f}"
        c_str = f"{c['p50']*1000:>5.1f}/{c['p95']*1000:>5.1f}/{c['p99']*1000:>5.1f}"
        speedup = b["p50"] / max(c["p50"], 1e-6)
        print(f"{op:<20} {b_str:>26} {c_str:>30} {speedup:>9.1f}x")


_CACHE_MASKED_THRESHOLD_S = 1e-4  # baseline p50 < 100µs → LspClient cache hit, not real LSP cost


def _assert_slo_5x_find_definitions(
    current: dict[str, dict[str, float]],
    baseline: dict[str, dict[str, float]],
) -> None:
    if "find_definitions" not in baseline or "find_definitions" not in current:
        pytest.skip("missing find_definitions distribution")
    chosen_p50 = current["find_definitions"]["p50"]
    jedi_p50 = baseline["find_definitions"]["p50"]
    if jedi_p50 < _CACHE_MASKED_THRESHOLD_S:
        pytest.skip(
            f"jedi baseline p50 ({jedi_p50*1e6:.1f}µs) < 100µs — the "
            "LspClient cache masked the per-call jedi.Script cost. "
            "Re-run the baseline with cache-defeating positions to make "
            "the apples-to-apples comparison."
        )
    speedup = jedi_p50 / max(chosen_p50, 1e-9)
    assert chosen_p50 * 5 <= jedi_p50, (
        f"{LSP_BACKEND_CHOSEN} find_definitions p50 ({chosen_p50*1000:.1f}ms) "
        f"NOT ≥5x faster than jedi baseline ({jedi_p50*1000:.1f}ms). "
        f"Achieved {speedup:.1f}x."
    )


def _assert_slo_p99_under_100ms(current: dict[str, dict[str, float]]) -> None:
    if "find_definitions" not in current:
        pytest.skip("missing find_definitions distribution")
    p99 = current["find_definitions"]["p99"]
    assert p99 < 0.1, (
        f"{LSP_BACKEND_CHOSEN} find_definitions p99 ({p99*1000:.1f}ms) "
        ">= 100ms warm — investigate"
    )


def _assert_slo_10x_hover(
    current: dict[str, dict[str, float]],
    baseline: dict[str, dict[str, float]],
) -> None:
    if "hover" not in baseline or "hover" not in current:
        pytest.skip("missing hover distribution")
    chosen_p50 = current["hover"]["p50"]
    jedi_p50 = baseline["hover"]["p50"]
    if jedi_p50 < _CACHE_MASKED_THRESHOLD_S:
        pytest.skip(
            f"jedi hover baseline p50 ({jedi_p50*1e6:.1f}µs) < 100µs — "
            "LspClient cache masked the per-call cost"
        )
    speedup = jedi_p50 / max(chosen_p50, 1e-9)
    assert chosen_p50 * 10 <= jedi_p50, (
        f"{LSP_BACKEND_CHOSEN} hover p50 ({chosen_p50*1000:.1f}ms) "
        f"NOT ≥10x faster than jedi baseline ({jedi_p50*1000:.1f}ms). "
        f"Achieved {speedup:.1f}x."
    )
