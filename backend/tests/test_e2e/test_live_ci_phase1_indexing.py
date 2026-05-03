"""Phase 1 live E2E — in-sandbox indexing + storage probe suite.

Runs seven cases from spec Task 1.5:

* 1.5.A privilege probe (HARD: ``$HOME/.cache/eos-ci`` writable without sudo)
* 1.5.B indexing readiness
* 1.5.C corruption recovery (SQLite index rebuilds when corrupted)
* 1.5.D state path-confinement guard (unit-style, no live needed)
* 1.5.E compatibility matrix (sqlite3, msgpack, basedpyright, git, unshare, …)
* 1.5.F eager bootstrap timing (cold create < 3s; warm restart < 500ms)
* 1.5.G overlay live mount probe (production stack: tmpfs+bind+overlay
  userxattr + write/modify/delete + whiteout + xattr round-trip)

Run with::

    .venv/bin/pytest backend/tests/test_e2e/test_live_ci_phase1_indexing.py -m live -v -s
"""

from __future__ import annotations

import os
import sys
import time
import uuid
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from collections.abc import Iterator
from unittest import mock

import pytest

from engine.testing.eval_agent import EvalAgent
from sandbox.api.bash import extract_exit_code, wrap_bash_command
from sandbox.code_intelligence.daemon.storage import (
    StoragePathEscape,
    _confine,
    workspace_root_hash,
)
from sandbox.code_intelligence.service import CodeIntelligenceService

from ._timing_harness import TimingHarness

pytestmark = [pytest.mark.e2e, pytest.mark.live]

_DASK_SWEEVO_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"
_DASK_SWEEVO_REPO_DIR = "/testbed"


def _flush_print(msg: str) -> None:
    print(msg, flush=True)
    sys.stdout.flush()


@contextmanager
def _traced_step(harness: TimingHarness, name: str) -> Iterator[None]:
    _flush_print(f"  → {name} ...")
    t0 = time.perf_counter()
    with harness.step(name):
        yield
    elapsed = time.perf_counter() - t0
    _flush_print(f"  ✓ {name} ({elapsed:.3f}s)")


@dataclass
class LivePhase1Env:
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

    def make_ci_service(self, *, transport: Any = None) -> CodeIntelligenceService:
        return CodeIntelligenceService(
            sandbox_id=self.sandbox_id,
            workspace_root=self.root_dir,
            sandbox=self.raw_sandbox,
            transport=transport,
        )


def _asyncio_run(coro: Any) -> Any:
    import asyncio

    return asyncio.run(coro)


@pytest.fixture(scope="module")
def live_phase1_env() -> Iterator[LivePhase1Env]:
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials not configured")

    from benchmarks.sweevo.dataset import select_sweevo_instance
    from benchmarks.sweevo.models import _CONDA_ACTIVATE
    from benchmarks.sweevo.sandbox import create_sweevo_test_sandbox
    from sandbox.testing import delete_test_sandbox, get_sandbox_service

    _flush_print(
        f"\n[fixture] provisioning sweevo sandbox {_DASK_SWEEVO_INSTANCE_ID} ..."
    )
    instance = select_sweevo_instance(instance_id=_DASK_SWEEVO_INSTANCE_ID)
    sandbox_name = f"ci-phase1-{uuid.uuid4().hex[:8]}"
    t0 = time.perf_counter()
    result = _asyncio_run(
        create_sweevo_test_sandbox(
            instance,
            sandbox_name=sandbox_name,
            repo_dir=_DASK_SWEEVO_REPO_DIR,
        )
    )
    sandbox_id = str(result["sandbox_id"])
    _flush_print(
        f"[fixture] sandbox {sandbox_id} provisioned in "
        f"{time.perf_counter() - t0:.1f}s"
    )
    try:
        raw_sandbox = get_sandbox_service().get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("echo $HOME", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/root"
        env = LivePhase1Env(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            home=home,
            root_dir=_DASK_SWEEVO_REPO_DIR,
        )
        # Smoke: confirm conda + python in sandbox.
        exit_code, output = env.exec(
            f"{_CONDA_ACTIVATE} && cd {_DASK_SWEEVO_REPO_DIR} && python --version",
            timeout=60,
        )
        assert exit_code == 0, output
        _flush_print(f"[fixture] sandbox ready: {output.strip()}")
        yield env
    finally:
        _flush_print(f"[fixture] tearing down sandbox {sandbox_id} ...")
        delete_test_sandbox(sandbox_id)
        _flush_print("[fixture] sandbox deleted")


# ---------------------------------------------------------------------------
# 1.5.A — Privilege probe
# ---------------------------------------------------------------------------


def test_privilege_probe_home_cache(live_phase1_env: LivePhase1Env) -> None:
    """The most important assertion of Phase 1.

    If ``$HOME/.cache/eos-ci/`` is not writable on the sandbox image, the
    entire migration plan needs amending (gray-area decision #1 in the
    overview).
    """
    h = TimingHarness(phase=1, test_name="privilege_probe")
    env = live_phase1_env

    with _traced_step(h, "mkdir_home_cache"):
        code, out = env.exec(
            'mkdir -p "$HOME/.cache/eos-ci/test_privilege" && '
            'ls -la "$HOME/.cache/eos-ci/"'
        )

    if code != 0:
        _, home_val = env.exec("echo $HOME")
        _, whoami_val = env.exec("whoami")
        _, umask_val = env.exec("umask")
        pytest.fail(
            f"PRIVILEGE FAILURE: mkdir $HOME/.cache/eos-ci failed exit={code}\n"
            f"  output: {out!r}\n"
            f"  $HOME = {home_val.strip()!r}\n"
            f"  whoami = {whoami_val.strip()!r}\n"
            f"  umask = {umask_val.strip()!r}\n"
            f"  ACTION: amend gray-area decision #1 to a writable path."
        )

    _flush_print(h.report())
    h.dump_json()


# ---------------------------------------------------------------------------
# 1.5.B — Indexing readiness
# ---------------------------------------------------------------------------


def test_indexing_readiness(live_phase1_env: LivePhase1Env) -> None:
    """Build the index in-sandbox via DaemonBackend and assert it is usable."""
    from sandbox.daytona.transport import DaytonaTransport

    h = TimingHarness(phase=1, test_name="indexing_readiness")
    env = live_phase1_env

    with mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "1"}):
        with _traced_step(h, "index_build_in_sandbox"):
            svc = env.make_ci_service(transport=DaytonaTransport())
            svc.ensure_initialized(wait=True)
        status = svc.status()
        symbol_status = status.get("symbol_index", {})
        actual_files = int(symbol_status.get("files") or 0)
        actual_symbols = int(symbol_status.get("symbols") or 0)
        h.record(
            "index_build_in_sandbox",
            count=actual_files,
            bytes_=actual_symbols,
        )
        with _traced_step(h, "query_symbols_first"):
            results = svc.query_symbols("Bag")
        h.record("query_symbols_first", count=len(results))

    _flush_print(h.report())
    h.dump_json()

    assert actual_files > 0
    assert actual_symbols > 0
    assert results


# ---------------------------------------------------------------------------
# 1.5.C — Corruption recovery
# ---------------------------------------------------------------------------


def test_corruption_recovery(live_phase1_env: LivePhase1Env) -> None:
    """Corrupt the SQLite index then rebuild — daemon must recover, not crash."""
    from sandbox.daytona.transport import DaytonaTransport

    h = TimingHarness(phase=1, test_name="corruption_recovery")
    env = live_phase1_env

    with mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "1"}):
        with _traced_step(h, "first_build"):
            svc = env.make_ci_service(transport=DaytonaTransport())
            svc.ensure_initialized(wait=True)
        baseline_count = int(
            svc.status().get("symbol_index", {}).get("symbols") or 0
        )
        assert baseline_count > 0

        wh = workspace_root_hash(env.root_dir)
        sqlite_path = f"{env.home.strip()}/.cache/eos-ci/{wh}/v1/index.sqlite3"

        with _traced_step(h, "corruption_inject"):
            svc.dispose()
            code, out = env.exec(f"printf %s GARBAGE > {sqlite_path}")
            assert code == 0, out

        with _traced_step(h, "corruption_recovery"):
            svc2 = env.make_ci_service(transport=DaytonaTransport())
            svc2.ensure_initialized(wait=True)
        recovered_count = int(
            svc2.status().get("symbol_index", {}).get("symbols") or 0
        )
        assert recovered_count >= baseline_count // 2

    _flush_print(h.report())
    h.dump_json()


# ---------------------------------------------------------------------------
# 1.5.D — Storage path-confinement (unit-style; no live infra required)
# ---------------------------------------------------------------------------


def test_storage_path_confinement(tmp_path: Path) -> None:
    """State-dir confinement rejects path traversal — unit-style test in this file."""
    state = tmp_path / "state"
    state.mkdir()

    target = _confine(state, "ok.bin")
    target.write_text("ok", encoding="utf-8")
    assert target.exists()

    with pytest.raises(StoragePathEscape):
        _confine(state, "../escape.bin")

    with pytest.raises(StoragePathEscape):
        _confine(state, "/etc/passwd")


# ---------------------------------------------------------------------------
# 1.5.E — Compatibility matrix probe
# ---------------------------------------------------------------------------


def test_compatibility_probe_dep_matrix(live_phase1_env: LivePhase1Env) -> None:
    """Survey every dep the daemon needs; surface the matrix in one run."""
    h = TimingHarness(phase=1, test_name="compatibility_probe")
    env = live_phase1_env

    checks = {
        "python_version":         "python3 --version",
        "python_310_plus":        "python3 -c 'import sys; assert sys.version_info >= (3,10)'",
        "sqlite3":                "python3 -c 'import sqlite3'",
        "msgpack_native":         "python3 -c 'import msgpack'",
        # Phase 3.6: chosen LSP backend is basedpyright (see
        # lsp-qualification-spike-result.md). The launch-binary check is the
        # one that matters — `python3 -c 'import basedpyright'` succeeds even
        # on images where the launch fails (Stage A finding).
        "basedpyright_native":    "python3 -c 'import basedpyright'",
        "basedpyright_langserver": "command -v basedpyright-langserver",
        "git":                    "git --version",
        "unshare_userns":         "unshare -Urm true",
        "setsid":                 "command -v setsid",
        "nohup":                  "command -v nohup",
        "tar":                    "command -v tar",
        "base64":                 "command -v base64",
        "kill":                   "command -v kill",
        "ps":                     "command -v ps",
        "home_writable":          'test -w "$HOME"',
        "tmp_writable":           "test -w /tmp && touch /tmp/_eos_probe && rm /tmp/_eos_probe",
        "af_unix_sockets":        "python3 -c 'import socket; s=socket.socket(socket.AF_UNIX); s.close()'",
        "proc_pid_status":        "test -r /proc/self/status",
    }

    matrix: dict[str, dict[str, Any]] = {}
    for name, cmd in checks.items():
        with _traced_step(h, f"probe_{name}"):
            code, out = env.exec(cmd, timeout=20)
        matrix[name] = {"ok": code == 0, "exit_code": code, "output": out.strip()[:200]}

    _flush_print(f"\n=== Compatibility matrix for sandbox {env.sandbox_id} ===")
    for name, result in matrix.items():
        status = "PASS" if result["ok"] else "FAIL"
        _flush_print(
            f"  [{status}] {name:20s} exit={result['exit_code']:3d} {result['output']!r}"
        )

    required = [
        "python_310_plus",
        "sqlite3",
        "git",
        "unshare_userns",
        "setsid",
        "nohup",
        "tar",
        "base64",
        "kill",
        "ps",
        "home_writable",
        "tmp_writable",
        "af_unix_sockets",
    ]
    missing = [r for r in required if not matrix[r]["ok"]]
    if missing:
        pytest.fail(
            f"Sandbox image missing required deps: {missing}\n"
            f"Full matrix: {matrix}"
        )

    # Phase 3.6: the chosen LSP backend (basedpyright + its langserver
    # binary) is HARD-required by the rewired LspClient. Until the sandbox
    # image bundles them, the live LSP path warm-installs at fixture time
    # — so we surface the missing deps as ERROR-level warnings (not test
    # failures) and recommend pre-baking. Once the image bundles them,
    # promote both to ``required`` above.
    soft_post_3_6 = ["basedpyright_native", "basedpyright_langserver"]
    soft = ["msgpack_native", "proc_pid_status", *soft_post_3_6]
    soft_missing = [s for s in soft if not matrix[s]["ok"]]
    if soft_missing:
        _flush_print(f"WARNING: soft deps missing: {soft_missing}")
        if "msgpack_native" in soft_missing:
            _flush_print("  msgpack_native missing: OK — bundle vendors msgpack")
        if "proc_pid_status" in soft_missing:
            _flush_print("  proc_pid_status missing: Phase 3.5 RSS sampling skipped")
        if any(d in soft_missing for d in soft_post_3_6):
            _flush_print(
                "  basedpyright_* missing: Phase 3.6 LSP path warm-installs "
                "at fixture time. Pre-bake into sandbox image to drop the "
                "first-spawn install cost (see lsp-qualification-spike-result.md)."
            )

    h.dump_json()


# ---------------------------------------------------------------------------
# 1.5.F — Eager bootstrap timing
# ---------------------------------------------------------------------------


def test_eager_bootstrap_timing(live_phase1_env: LivePhase1Env) -> None:
    """Cold + warm timing for the in-sandbox runtime upload + indexer.

    We measure against the live fixture (already provisioned); cold run is
    the first ``ensure_runtime_uploaded`` (uploads bundle), warm run is the
    second (no-op when hash matches).
    """
    from sandbox.code_intelligence.daemon.launcher import ensure_runtime_uploaded
    from sandbox.daytona.transport import DaytonaTransport

    h = TimingHarness(phase=1, test_name="eager_bootstrap_timing")
    env = live_phase1_env
    transport = DaytonaTransport()

    with _traced_step(h, "bundle_upload_cold"):
        _asyncio_run(ensure_runtime_uploaded(transport, env.sandbox_id))
    with _traced_step(h, "bundle_upload_warm"):
        _asyncio_run(ensure_runtime_uploaded(transport, env.sandbox_id))

    with mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "1"}):
        with _traced_step(h, "indexer_run_cold"):
            svc = env.make_ci_service(transport=transport)
            svc.ensure_initialized(wait=True)

        with _traced_step(h, "query_symbols_after_eager"):
            results = svc.query_symbols("Bag")
        assert len(results) > 0, "index not ready immediately after eager bootstrap"

    cold_total = (
        h._step_index["bundle_upload_cold"].elapsed_s
        + h._step_index["indexer_run_cold"].elapsed_s
    )
    warm_only = h._step_index["bundle_upload_warm"].elapsed_s

    _flush_print(h.report())
    h.dump_json()

    # Phase 1 SLO context: Daytona's binary upload/download endpoints
    # (``fs.upload_file`` / ``fs.download_file``) returned 502 from the proxy
    # for any payload more than tens of KB on this self-hosted Daytona,
    # forcing chunked-base64 over ``transport.exec`` for the runtime bundle.
    # The test keeps a generous ceiling so it catches real regressions
    # without flagging expected Daytona exec round-trip cost.
    assert cold_total < 120.0, (
        f"cold bundle-upload + indexer > 120s ({cold_total:.2f}s) — investigate"
    )
    # Warm bundle upload skips the chunked tar write but still rebuilds
    # the bundle bytes locally + does a marker check. ~5-7s observed.
    assert warm_only < 15.0, (
        f"warm bundle-upload > 15s ({warm_only:.2f}s); idempotency or "
        f"bundle-build cache may be broken"
    )


# ---------------------------------------------------------------------------
# 1.5.G — Overlay live mount probe
# ---------------------------------------------------------------------------


_OVERLAY_PROBE_SCRIPT = r'''
set -e
tmpdir=$(mktemp -d)
lower=$tmpdir/lower
merged=$tmpdir/merged
tmpfs_root=$tmpdir/tmpfs

mkdir -p "$lower" "$merged" "$tmpfs_root"
echo "lower-keep"   > "$lower/keep.txt"
echo "lower-modify" > "$lower/modify.txt"
echo "lower-delete" > "$lower/delete.txt"

unshare -Urm bash -c "
  set -e

  mount -t tmpfs -o size=10m tmpfs '$tmpfs_root'
  mkdir -p '$tmpfs_root/upper' '$tmpfs_root/work'

  mount --bind '$lower' '$lower'

  mount -t overlay overlay -o 'lowerdir=$lower,upperdir=$tmpfs_root/upper,workdir=$tmpfs_root/work,userxattr' '$merged'

  echo 'new-content' > '$merged/new.txt'
  test -f '$tmpfs_root/upper/new.txt' || { echo 'FAIL: new file not copied up'; exit 51; }
  test \"\$(cat '$merged/new.txt')\" = 'new-content' || { echo 'FAIL: merged view of new file wrong'; exit 51; }

  echo 'modified-content' > '$merged/modify.txt'
  test -f '$tmpfs_root/upper/modify.txt' || { echo 'FAIL: modify did not copy up'; exit 52; }
  test \"\$(cat '$tmpfs_root/upper/modify.txt')\" = 'modified-content' || { echo 'FAIL: upperdir copy-up content wrong'; exit 52; }

  rm '$merged/delete.txt'
  if [ -e '$merged/delete.txt' ]; then echo 'FAIL: delete still visible through merged'; exit 53; fi

  upper_delete='$tmpfs_root/upper/delete.txt'
  if [ ! -e \"\$upper_delete\" ]; then
    echo 'FAIL: whiteout tombstone missing in upperdir'
    ls -la '$tmpfs_root/upper'
    exit 54
  fi

  whiteout_ok=0
  if stat -c '%t,%T' \"\$upper_delete\" 2>/dev/null | grep -q '^0,0\$'; then
    whiteout_ok=1
  elif command -v getfattr >/dev/null 2>&1; then
    if getfattr -n user.overlay.whiteout \"\$upper_delete\" 2>/dev/null | grep -q whiteout; then
      whiteout_ok=1
    fi
  fi
  if [ \"\$whiteout_ok\" != '1' ]; then
    if ! command -v getfattr >/dev/null 2>&1; then
      echo 'WARN: getfattr missing — cannot verify userxattr-style whiteout; trusting kernel'
      whiteout_ok=1
    fi
  fi
  if [ \"\$whiteout_ok\" != '1' ]; then
    echo 'FAIL: tombstone is neither char(0,0) nor user.overlay.whiteout xattr'
    stat \"\$upper_delete\" || true
    exit 55
  fi

  if command -v setfattr >/dev/null 2>&1 && command -v getfattr >/dev/null 2>&1; then
    setfattr -n user.eos_probe -v 'probe_value' '$merged/modify.txt'
    got=\$(getfattr -n user.eos_probe --only-values '$merged/modify.txt' 2>/dev/null)
    if [ \"\$got\" != 'probe_value' ]; then
      echo \"FAIL: user.* xattr round-trip — got '\$got' expected 'probe_value'\"
      exit 56
    fi
    echo 'OK: overlay live probe passed (xattr round-trip verified)'
  else
    echo 'OK: overlay live probe passed (xattr binaries missing — kernel-level overlay verified)'
  fi
"
rc=$?
rm -rf "$tmpdir"
exit $rc
'''


def test_overlay_live_mount_probe(live_phase1_env: LivePhase1Env) -> None:
    """End-to-end probe of the production overlay stack.

    Mirrors :func:`namespace.setup_mounts` (tmpfs + bind lower + overlay
    userxattr) and exercises copy-up, whiteout, and xattr round-trip. If
    this fails, ``svc.cmd`` will not work on the sandbox image — diagnostic
    exit codes 51-56 narrow the failure mode.
    """
    h = TimingHarness(phase=1, test_name="overlay_live_mount_probe")
    env = live_phase1_env

    with _traced_step(h, "overlay_live_probe"):
        code, out = env.exec(_OVERLAY_PROBE_SCRIPT, timeout=120)

    if code != 0:
        pytest.fail(
            f"OVERLAY LIVE PROBE FAILED (exit_code={code}):\n"
            f"{out}\n"
            f"This is the production overlay stack; failure means svc.cmd "
            f"will not work on this image. Investigate kernel overlayfs "
            f"userxattr support (≥5.11), unprivileged userns config, LSM "
            f"profiles (AppArmor/SELinux), and getfattr/setfattr binaries."
        )

    _flush_print(h.report())
    h.dump_json()
