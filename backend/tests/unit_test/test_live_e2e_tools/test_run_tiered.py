"""Unit tests for the progressive-tier runner — covers PRD T-C1, T-C2."""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

import pytest

from tests.live_e2e_test._tools import run_tiered
from tests.live_e2e_test._tools.run_tiered import (
    CascadeState,
    RunSummary,
    SubprocessOutcome,
    TierConfig,
    TierOutcome,
    execute_tier,
    load_tier_configs,
    run,
    write_summary,
)
from tests.live_e2e_test._tools.tier0_health import Tier0Result


# --------------------------------------------------------------------------
# TOML loading
# --------------------------------------------------------------------------


_VALID_TOML = """
[[tier]]
id = 0
name = "preflight"
wall_budget_s = 30
kind = "tier0_health"
cascade = "abort_all"

[[tier]]
id = 1
name = "smoke"
wall_budget_s = 60
kind = "pytest"
pytest_args = ["foo.py"]
cascade = "abort_ge"
cascade_target = 2

[[tier]]
id = 2
name = "k_scaling"
wall_budget_s = 120
kind = "pytest"
pytest_args = ["bar.py"]
cascade = "warn"
"""


def test_load_tier_configs_valid(tmp_path):
    cfg = tmp_path / "tiers.toml"
    cfg.write_text(_VALID_TOML)
    tiers = load_tier_configs(cfg)
    assert len(tiers) == 3
    assert tiers[0].kind == "tier0_health"
    assert tiers[1].cascade == "abort_ge"
    assert tiers[1].cascade_target == 2
    assert tiers[2].cascade == "warn"
    assert tiers[2].cascade_target is None


def test_load_tier_configs_rejects_unknown_cascade(tmp_path):
    cfg = tmp_path / "tiers.toml"
    cfg.write_text(
        '[[tier]]\nid = 0\nname = "x"\nwall_budget_s = 1\n'
        'kind = "pytest"\npytest_args = []\ncascade = "BOGUS"\n'
    )
    with pytest.raises(ValueError, match="invalid cascade"):
        load_tier_configs(cfg)


def test_load_tier_configs_rejects_abort_ge_without_target(tmp_path):
    cfg = tmp_path / "tiers.toml"
    cfg.write_text(
        '[[tier]]\nid = 0\nname = "x"\nwall_budget_s = 1\n'
        'kind = "pytest"\npytest_args = []\ncascade = "abort_ge"\n'
    )
    with pytest.raises(ValueError, match="cascade_target"):
        load_tier_configs(cfg)


def test_load_real_tiers_toml_parses():
    """The shipped tiers.toml must remain valid as the runner evolves."""
    here = Path(run_tiered.__file__).resolve().parent
    tiers = load_tier_configs(here / "tiers.toml")
    ids = [t.id for t in tiers]
    assert ids == [0, 1, 2, 3, 4, 5, 6]
    assert tiers[0].kind == "tier0_health"
    assert tiers[-1].cascade == "warn"


# --------------------------------------------------------------------------
# Cascade state machine
# --------------------------------------------------------------------------


def _tier(
    tid: int,
    *,
    cascade: run_tiered.CascadeKind = "warn",
    cascade_target: int | None = None,
    kind: run_tiered.TierKind = "pytest",
) -> TierConfig:
    return TierConfig(
        id=tid,
        name=f"tier{tid}",
        wall_budget_s=10.0,
        kind=kind,
        cascade=cascade,
        pytest_args=[],
        cascade_target=cascade_target,
    )


def test_cascade_abort_all_skips_everything_after_failure():
    state = CascadeState()
    state.record(_tier(0, cascade="abort_all", kind="tier0_health"), "failed")
    assert state.should_skip(_tier(1)) is True
    assert state.should_skip(_tier(6)) is True


def test_cascade_abort_ge_skips_only_at_or_above_threshold():
    state = CascadeState()
    state.record(_tier(1, cascade="abort_ge", cascade_target=2), "failed")
    assert state.should_skip(_tier(1)) is False
    assert state.should_skip(_tier(2)) is True
    assert state.should_skip(_tier(5)) is True
    assert state.should_skip(_tier(6)) is True


def test_cascade_warn_does_not_skip():
    state = CascadeState()
    state.record(_tier(2, cascade="warn"), "failed")
    assert state.should_skip(_tier(3)) is False
    assert state.should_skip(_tier(6)) is False


def test_cascade_passed_does_not_propagate():
    state = CascadeState()
    state.record(_tier(0, cascade="abort_all"), "passed")
    assert state.should_skip(_tier(1)) is False


def test_cascade_threshold_keeps_lowest():
    """If tier 1 (target=2) fails AND tier 4 (target=5) fails, tiers ≥2 skipped."""
    state = CascadeState()
    state.record(_tier(1, cascade="abort_ge", cascade_target=2), "failed")
    state.record(_tier(4, cascade="abort_ge", cascade_target=5), "failed")
    assert state.skip_threshold == 2
    assert state.should_skip(_tier(3)) is True


# --------------------------------------------------------------------------
# Budget timeout — fake popen
# --------------------------------------------------------------------------


@dataclass
class _FakeProc:
    pid: int = 4242
    returncode: int | None = 0
    will_timeout_first_communicate: bool = False
    will_timeout_grace_communicate: bool = False
    timeouts_remaining: int = 0
    _stdout: bytes = b"hello"
    _stderr: bytes = b""

    def communicate(self, timeout: float | None = None):  # noqa: ARG002
        if self.timeouts_remaining > 0:
            self.timeouts_remaining -= 1
            raise subprocess.TimeoutExpired(cmd="x", timeout=timeout or 0)
        if self.returncode is None:
            self.returncode = 0
        return self._stdout, self._stderr


def test_run_with_budget_clean_exit_returns_passed():
    proc = _FakeProc(returncode=0)
    outcome = run_tiered.run_with_budget(
        ["pytest"],
        env={},
        wall_budget_s=10.0,
        popen_factory=lambda *a, **kw: proc,
        clock=iter([0.0, 0.5]).__next__,
    )
    assert outcome.timed_out is False
    assert outcome.returncode == 0
    assert outcome.elapsed_s == pytest.approx(0.5)


def test_run_with_budget_logs_midflight_stdout_tail(tmp_path):
    messages: list[str] = []
    outcome = run_tiered.run_with_budget(
        [
            sys.executable,
            "-c",
            "import time; print('progress-marker', flush=True); time.sleep(0.25)",
        ],
        env=os.environ.copy(),
        wall_budget_s=2.0,
        cwd=tmp_path,
        progress_logger=messages.append,
        progress_interval_s=0.05,
    )
    assert outcome.returncode == 0
    assert outcome.timed_out is False
    assert any("pytest running" in message for message in messages)
    assert any("progress-marker" in message for message in messages)


def test_run_with_budget_timeout_signals_then_kills(monkeypatch):
    """First communicate() raises TimeoutExpired (wall budget), then SIGINT
    delivered, second communicate() also raises (grace), then SIGKILL."""
    proc = _FakeProc(returncode=-9, timeouts_remaining=2)
    signals_sent: list[int] = []

    def _fake_terminate(pid: int, sig: int) -> None:
        signals_sent.append(sig)
        # When SIGKILL fires, simulate the kernel reaping the child so the
        # next communicate() returns cleanly.
        if sig == signal.SIGKILL:
            proc.timeouts_remaining = 0

    monkeypatch.setattr(run_tiered, "_terminate_group", _fake_terminate)

    outcome = run_tiered.run_with_budget(
        ["pytest"],
        env={},
        wall_budget_s=0.001,
        grace_s=0.001,
        popen_factory=lambda *a, **kw: proc,
        clock=iter([0.0, 0.05, 0.1, 0.2]).__next__,
    )
    assert outcome.timed_out is True
    assert signal.SIGINT in signals_sent
    assert signal.SIGKILL in signals_sent
    assert signals_sent.index(signal.SIGINT) < signals_sent.index(signal.SIGKILL)


# --------------------------------------------------------------------------
# execute_tier — tier 0 + pytest
# --------------------------------------------------------------------------


def test_execute_tier_tier0_health_passed(tmp_path):
    tier = _tier(0, cascade="abort_all", kind="tier0_health")

    def _fake_probe(_url: str) -> Tier0Result:
        return Tier0Result(passed=True, api_health="ok", elapsed_s=0.1)

    outcome = execute_tier(
        tier,
        run_id="testrun",
        project_root=tmp_path,
        results_dir=tmp_path,
        tier0_probe=_fake_probe,
        clock=iter([0.0, 0.5]).__next__,
    )
    assert outcome.status == "passed"
    assert outcome.failed_cells == 0


def test_execute_tier_tier0_health_failed(tmp_path):
    tier = _tier(0, cascade="abort_all", kind="tier0_health")

    def _fake_probe(_url: str) -> Tier0Result:
        return Tier0Result(
            passed=False,
            api_health="ok",
            stuck_rows=["sb-1"],
            docker_available=True,
            elapsed_s=0.1,
            notes="tier0_manual_recovery_required",
        )

    outcome = execute_tier(
        tier,
        run_id="testrun",
        project_root=tmp_path,
        results_dir=tmp_path,
        tier0_probe=_fake_probe,
        clock=iter([0.0, 0.5]).__next__,
    )
    assert outcome.status == "failed"
    assert outcome.failed_cells == 1
    assert "tier0_manual_recovery_required" in outcome.notes


def test_execute_tier_pytest_passed(tmp_path):
    tier = _tier(1, cascade="abort_ge", cascade_target=2)

    def _fake_runner(argv, **kw):
        # Drop a fake artifact with no failures so failed_cells = 0.
        artifact = tmp_path / f"phase00-smoke-{kw['env']['EOS_TIER_RUN_ID']}.jsonl"
        artifact.write_text(
            json.dumps({"schema": "phase00.smoke.v1", "passed": True}) + "\n"
        )
        return SubprocessOutcome(
            returncode=0,
            timed_out=False,
            stdout_tail="",
            stderr_tail="",
            elapsed_s=2.5,
        )

    outcome = execute_tier(
        tier,
        run_id="rid1",
        project_root=tmp_path,
        results_dir=tmp_path,
        subprocess_runner=_fake_runner,
    )
    assert outcome.status == "passed"
    assert outcome.failed_cells == 0


def test_execute_tier_pytest_failed_counts_artifact_failures(tmp_path):
    tier = _tier(3, cascade="warn")
    artifact = tmp_path / "phase07-size-matrix-rid2.jsonl"
    artifact.write_text(
        "\n".join(
            [
                json.dumps({"passed": True, "cell_id": "a"}),
                json.dumps({"passed": False, "cell_id": "b"}),
                json.dumps({"passed": False, "cell_id": "c"}),
            ]
        )
    )

    def _fake_runner(argv, **kw):
        return SubprocessOutcome(
            returncode=1,
            timed_out=False,
            stdout_tail="",
            stderr_tail="2 failed",
            elapsed_s=10.0,
        )

    outcome = execute_tier(
        tier,
        run_id="rid2",
        project_root=tmp_path,
        results_dir=tmp_path,
        subprocess_runner=_fake_runner,
    )
    assert outcome.status == "failed"
    assert outcome.failed_cells == 2


def test_execute_tier_pytest_counts_only_current_tier_artifacts(tmp_path):
    tier = _tier(6, cascade="none")
    (tmp_path / "phase09-size-x-concurrency-rid6.jsonl").write_text(
        json.dumps({"passed": False, "cell_id": "tier4-failure"}) + "\n",
        encoding="utf-8",
    )
    (tmp_path / "phase09-adversarial-rid6.jsonl").write_text(
        json.dumps({"passed": True, "cell_id": "tier6-pass"}) + "\n",
        encoding="utf-8",
    )

    def _fake_runner(argv, **kw):
        return SubprocessOutcome(
            returncode=0,
            timed_out=False,
            stdout_tail="",
            stderr_tail="",
            elapsed_s=3.0,
        )

    outcome = execute_tier(
        tier,
        run_id="rid6",
        project_root=tmp_path,
        results_dir=tmp_path,
        subprocess_runner=_fake_runner,
    )
    assert outcome.status == "passed"
    assert outcome.failed_cells == 0


def test_execute_tier_pytest_aborted_budget(tmp_path):
    tier = _tier(2, cascade="warn")

    def _fake_runner(argv, **kw):
        return SubprocessOutcome(
            returncode=-9,
            timed_out=True,
            stdout_tail="",
            stderr_tail="killed",
            elapsed_s=120.0,
        )

    outcome = execute_tier(
        tier,
        run_id="ridA",
        project_root=tmp_path,
        results_dir=tmp_path,
        subprocess_runner=_fake_runner,
    )
    assert outcome.status == "aborted_budget"
    assert "wall_budget" in outcome.notes


def test_artifact_progress_note_summarizes_latest_row(tmp_path):
    artifact = tmp_path / "phase06-k1000-spot-check-rid.jsonl"
    artifact.write_text(
        "\n".join(
            [
                json.dumps(
                    {
                        "schema": "phase06.large_capture_scaling.v2",
                        "cell_id": "tracked-k1000",
                        "passed": True,
                    }
                ),
                json.dumps(
                    {
                        "schema": "phase06.k1000_spot_check.summary.v1",
                        "failed_cells": 0,
                    }
                ),
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    note = run_tiered._artifact_progress_note(tmp_path, "rid")
    assert "phase06-k1000-spot-check-rid.jsonl:rows=2" in note
    assert "failed_cells=0" in note


def test_artifact_progress_note_filters_to_current_tier(tmp_path):
    (tmp_path / "phase09-size-x-concurrency-rid.jsonl").write_text(
        json.dumps({"schema": "phase09.size_x_concurrency.v1", "passed": False})
        + "\n",
        encoding="utf-8",
    )
    (tmp_path / "phase09-adversarial-rid.jsonl").write_text(
        json.dumps({"schema": "phase09.live_e2e.v1", "passed": True}) + "\n",
        encoding="utf-8",
    )
    note = run_tiered._artifact_progress_note(tmp_path, "rid", tier_id=6)
    assert "phase09-adversarial-rid.jsonl" in note
    assert "phase09-size-x-concurrency-rid.jsonl" not in note


# --------------------------------------------------------------------------
# Full run — cascade integration
# --------------------------------------------------------------------------


def _build_pipeline_tiers() -> list[TierConfig]:
    """Mirror the production tiers.toml cascade topology (plan §3)."""
    return [
        _tier(0, cascade="abort_all", kind="tier0_health"),
        _tier(1, cascade="abort_ge", cascade_target=2),
        _tier(2, cascade="warn"),
        _tier(3, cascade="warn"),
        _tier(4, cascade="abort_eq", cascade_target=5),
        _tier(5, cascade="abort_eq", cascade_target=6),
        _tier(6, cascade="none"),
    ]


def _ok_probe(_url: str) -> Tier0Result:
    return Tier0Result(passed=True, api_health="ok", elapsed_s=0.1)


def _fail_probe(_url: str) -> Tier0Result:
    return Tier0Result(
        passed=False,
        api_health="ok",
        stuck_rows=["sb-stuck"],
        docker_available=True,
        elapsed_s=0.1,
        notes="tier0_manual_recovery_required",
    )


def _all_pass_runner(argv, **kw):
    return SubprocessOutcome(
        returncode=0, timed_out=False, stdout_tail="", stderr_tail="", elapsed_s=1.0
    )


def _failing_tier_runner(failing_id: int):
    def _runner(argv, **kw):
        tier_id = int(kw["env"]["EOS_TIER_ID"])
        if tier_id == failing_id:
            return SubprocessOutcome(
                returncode=1,
                timed_out=False,
                stdout_tail="",
                stderr_tail="1 failed",
                elapsed_s=2.0,
            )
        return _all_pass_runner(argv, **kw)
    return _runner


def test_run_full_pipeline_all_pass(tmp_path):
    summary = run(
        _build_pipeline_tiers(),
        project_root=tmp_path,
        results_dir=tmp_path,
        run_id="full_pass",
        tier0_probe=_ok_probe,
        subprocess_runner=_all_pass_runner,
        clock=iter([float(i) for i in range(0, 100)]).__next__,
    )
    statuses = [o.status for o in summary.outcomes]
    assert statuses == ["passed"] * 7
    assert summary.exit_code == 0
    assert summary.summary_path.exists()


def test_run_tier0_failure_aborts_everything(tmp_path):
    summary = run(
        _build_pipeline_tiers(),
        project_root=tmp_path,
        results_dir=tmp_path,
        run_id="t0fail",
        tier0_probe=_fail_probe,
        subprocess_runner=_all_pass_runner,
        clock=iter([float(i) for i in range(0, 100)]).__next__,
    )
    assert summary.outcomes[0].status == "failed"
    assert all(o.status == "skipped_cascade" for o in summary.outcomes[1:])
    assert summary.exit_code == 1


def test_run_tier1_failure_aborts_2_through_4_keeps_5_6(tmp_path):
    """Plan §3: tier 1 cascade=abort_ge target=2 → skip tiers ≥2."""
    summary = run(
        _build_pipeline_tiers(),
        project_root=tmp_path,
        results_dir=tmp_path,
        run_id="t1fail",
        tier0_probe=_ok_probe,
        subprocess_runner=_failing_tier_runner(failing_id=1),
        clock=iter([float(i) for i in range(0, 100)]).__next__,
    )
    statuses = [o.status for o in summary.outcomes]
    assert statuses[0] == "passed"
    assert statuses[1] == "failed"
    assert all(s == "skipped_cascade" for s in statuses[2:])
    assert summary.exit_code == 1


def test_run_tier4_failure_aborts_5_keeps_6(tmp_path):
    """Plan §3: tier 4 cascade=abort_eq target=5 → tier 6 still runs."""
    summary = run(
        _build_pipeline_tiers(),
        project_root=tmp_path,
        results_dir=tmp_path,
        run_id="t4fail",
        tier0_probe=_ok_probe,
        subprocess_runner=_failing_tier_runner(failing_id=4),
        clock=iter([float(i) for i in range(0, 100)]).__next__,
    )
    statuses = [o.status for o in summary.outcomes]
    assert statuses[:4] == ["passed"] * 4
    assert statuses[4] == "failed"
    assert statuses[5] == "skipped_cascade"
    assert statuses[6] == "passed"  # tier 6 still runs
    assert summary.exit_code == 1


def test_run_warn_tier_failure_does_not_cascade(tmp_path):
    """Plan §3: tier 3 cascade=warn → tier 4..6 still run."""
    summary = run(
        _build_pipeline_tiers(),
        project_root=tmp_path,
        results_dir=tmp_path,
        run_id="t3warn",
        tier0_probe=_ok_probe,
        subprocess_runner=_failing_tier_runner(failing_id=3),
        clock=iter([float(i) for i in range(0, 100)]).__next__,
    )
    statuses = [o.status for o in summary.outcomes]
    assert statuses[3] == "failed"
    assert all(s == "passed" for s in statuses[4:])


# --------------------------------------------------------------------------
# Aggregator
# --------------------------------------------------------------------------


def test_write_summary_emits_one_row_per_tier(tmp_path):
    outcomes = [
        TierOutcome(tier_id=0, name="t0", status="passed", elapsed_s=0.5),
        TierOutcome(tier_id=1, name="t1", status="failed", elapsed_s=2.0,
                    failed_cells=2, notes="boom"),
        TierOutcome(tier_id=2, name="t2", status="skipped_cascade", elapsed_s=0.0),
    ]
    path = tmp_path / "summary.jsonl"
    write_summary(outcomes, path, run_id="abc")
    rows = [json.loads(line) for line in path.read_text().splitlines() if line]
    assert len(rows) == 3
    assert rows[0]["tier"] == 0
    assert rows[0]["status"] == "passed"
    assert rows[1]["failed_cells"] == 2
    assert rows[1]["notes"] == "boom"
    assert rows[2]["status"] == "skipped_cascade"
    assert all(r["schema"] == "progressive_test.tier_summary.v1" for r in rows)
    assert all(r["run_id"] == "abc" for r in rows)


def test_run_summary_exit_code_treats_aborted_budget_as_failure(tmp_path):
    summary = RunSummary(
        run_id="r",
        outcomes=[
            TierOutcome(0, "t0", "passed", 0.5),
            TierOutcome(1, "t1", "aborted_budget", 60.0),
        ],
        summary_path=tmp_path / "s.jsonl",
    )
    assert summary.exit_code == 1
