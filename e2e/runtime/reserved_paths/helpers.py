"""Reserved `.wh.` namespace live catalog: verdicts, preconditions, fixtures.

Implements the harness half of
docs/obsidian/ephemeral-os/implementation_plan/wh-reserved-namespace/test-case.md:
per-case verdict.json in the catalog §2 schema, the §1.1 environment
preconditions P1–P3 (hard-fail, never skip), the §1.3 teardown contract, and
the shared exec/publish assertion helpers. Cases assert on structured JSON
only — never log scraping.
"""

from __future__ import annotations

import atexit
import datetime as dt
import json
import os
import shutil
import subprocess
import tempfile
import threading
import time
import traceback
from contextlib import contextmanager
from pathlib import Path

from core import cleanup
from core.cli import is_error, manager
from core.config import IMAGE
from runtime.file.correctness.test_correctness_sessionless import (
    _assert_publish_rejection,
    _assert_stack_unchanged,
)
from runtime.file.helpers import (
    assert_error,
    assert_ok,
    exec_command,
    file_read,
    file_write,
    layerstack,
    write_command_stdin,
)
from runtime.workspace_session.helpers import (
    squash_layerstacks,
    create_session,
    destroy_session,
    exec_in,
)

SUITE_DIR = Path(__file__).resolve().parent
RUN_ID = os.environ.get(
    "WH_RESERVED_RUN_ID",
    dt.datetime.now().strftime("whres-%Y%m%d-%H%M%S"),
)
REPORT_ROOT = SUITE_DIR / "test-reports" / RUN_ID

REJECT_CLASS = "protected_path"

CASE_TIERS = {
    "EZ-01": "easy",
    "EZ-02": "easy",
    "EZ-03": "easy",
    "EZ-04": "easy",
    "EZ-05": "easy",
    "EZ-06": "easy",
    "MED-01": "medium",
    "MED-02": "medium",
    "MED-03": "medium",
    "MED-04": "medium",
    "MED-05": "medium",
    "MED-06": "medium",
    "CX-01": "complex",
    "CX-02": "complex",
    "CX-03": "complex",
    "CX-04": "complex",
}

SUITE_NOTES = [
    "Red-first waiver: the fix (D1-D3) was committed before this catalog first "
    "ran, so the pre-fix failing verdicts for EZ-01/EZ-03/EZ-04 could not be "
    "recorded; their data-safety assertions encode the pre-fix failure modes.",
    "EZ-04 pinned mapping (spec Open Question 1): sessionless file_write at a "
    "reserved path faults operation_failed carrying the ProtectedPath "
    "rejection; sessionless file_edit is read-modify-write and faults "
    "not_found before publish because a reserved path can never resolve in "
    "the merged view.",
    "MED-05: one-shot terminal responses carry publish_reject_class only (no "
    "path field), so the path-naming half is pinned via the sessionless "
    "file_write error message, which names the literal user path.",
    "Defect found by MED-06 (fixed in this change): the publish path encoded "
    "OpaqueDir as a bare literal .wh..wh..opq file with no kernel overlay "
    "opaque xattr, so live session mounts (raw layer lowerdirs) listed the "
    "marker and resurfaced opaque-masked lower content until a squash "
    "re-encoded it. write_layer_changes now writes the same kernel-native "
    "dual encoding squash flatten always produced (whiteout-encoded marker + "
    "overlay opaque xattr); every existing layer stays valid.",
    "Catalog correction (CX-03 isolation): the original 'fd count stable "
    "+/-16' sentinel contradicts the daemon's documented command-session "
    "retention — every completed session retains its pty fd until LRU "
    "eviction at the engine cap (MAX_ACTIVE_COMMANDS), so any 17+-command "
    "workload would trip it regardless of reserved names. The sentinel now "
    "bounds fd growth by command sessions started (+16 margin), which still "
    "catches real leaks; test-case.md was amended to match.",
    "Residual artifact (pre-existing, shared with every squash-produced "
    "layer): kernel overlayfs does not filter whiteout dirents from an "
    "opaque lowerdir directory, so a raw in-session `ls` of an opaque dir "
    "still lists the whiteout-encoded `.wh..wh..opq` char-device name. The "
    "daemon's merged read surface hides it and no lower data is exposed; "
    "MED-06/CX-02 pin the merged reads plus session data correctness and "
    "record this artifact.",
]

_summary_lock = threading.Lock()


def now_iso():
    return dt.datetime.now().astimezone().isoformat(timespec="seconds")


def sh(script: str) -> str:
    return "set -eu\ncd /workspace\n" + script


class CaseRecorder:
    """Writes the catalog §2 verdict schema for one case."""

    def __init__(self, case_id: str):
        self.case_id = case_id
        self.tier = CASE_TIERS[case_id]
        self.dir = REPORT_ROOT / case_id
        self.axes = {
            "correctness": {"pass": False},
            "data_safety": {"pass": False},
            "isolation": {"pass": True, "status": "n/a"},
        }
        self.teardown = {
            "pass": False,
            "lease_registry_empty": False,
            "stack_unchanged": False,
        }
        self.defects: list[dict] = []
        self.notes: list[str] = []
        self.timers: dict = {}
        self.expected_stack = None
        self.sandbox_id = None
        self.started = time.monotonic()
        self._verdict_written = False

    def __enter__(self):
        self.dir.mkdir(parents=True, exist_ok=True)
        self.record("case.json", {"case_id": self.case_id, "run_id": RUN_ID, "tier": self.tier})
        return self

    def __exit__(self, exc_type, exc, tb):
        if exc is not None:
            self.defects.append(
                {
                    "case_id": self.case_id,
                    "defect": f"{exc_type.__name__}: {exc}",
                    "traceback": "".join(traceback.format_exception(exc_type, exc, tb))[-4000:],
                }
            )
        self.write_verdict()
        return False

    def record(self, name: str, payload):
        path = self.dir / name
        if isinstance(payload, str):
            path.write_text(payload, encoding="utf-8")
        else:
            path.write_text(
                json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
            )
        return payload

    def cmd(self, command: str):
        with (self.dir / "cmd.log").open("a", encoding="utf-8") as handle:
            handle.write(command.rstrip("\n") + "\n")

    def axis(self, name: str, passed: bool, **metrics):
        entry = {"pass": bool(passed)}
        entry.update(metrics)
        self.axes[name] = entry

    def note(self, message: str):
        self.notes.append(message)

    def timer(self, name: str, value_ms: float):
        self.timers[name] = round(float(value_ms), 3)

    def expect_stack(self, stack):
        """Remember the last *accepted* stack state for the teardown contract."""
        self.expected_stack = {
            "manifest_version": stack.get("manifest_version"),
            "root_hash": stack.get("root_hash"),
        }
        return stack

    def write_verdict(self):
        if self._verdict_written:
            return
        self._verdict_written = True
        self.timer("wall_ms", (time.monotonic() - self.started) * 1000.0)
        status = "pass" if (
            all(axis.get("pass") for axis in self.axes.values())
            and self.teardown.get("pass")
            and not self.defects
        ) else "fail"
        self.record(
            "verdict.json",
            {
                "case_id": self.case_id,
                "run_id": RUN_ID,
                "status": status,
                "axes": self.axes,
                "teardown": self.teardown,
                "defects": self.defects,
                "tier": self.tier,
                "timers": self.timers,
                "notes": self.notes,
                "generated_at": now_iso(),
            },
        )
        finalize_summary()


@contextmanager
def wh_case(tmp_path, rec: CaseRecorder, files=None, dirs=()):
    """One sandbox per case, seeded host-side; §1.3 teardown contract on exit."""
    root = Path(tmp_path) / f"{rec.case_id.lower()}-workspace"
    root.mkdir(parents=True, exist_ok=True)
    for directory in dirs:
        (root / directory).mkdir(parents=True, exist_ok=True)
    for name, content in (files or {}).items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    created = manager(
        "create_sandbox",
        "--image",
        IMAGE,
        "--workspace-bind-root",
        str(root),
        timeout=600,
    )
    assert not is_error(created), created
    sandbox_id = created["id"]
    rec.sandbox_id = sandbox_id
    cleanup.track(sandbox_id)
    rec.record("create_sandbox.json", created)
    rec.expect_stack(assert_ok(layerstack(sandbox_id)))
    body_failed = False
    try:
        yield sandbox_id
    except BaseException:
        body_failed = True
        raise
    finally:
        try:
            stack = layerstack(sandbox_id)
            rec.record("layerstack.json", stack)
            lease_ok = stack.get("active_lease_count") == 0
            expected = rec.expected_stack or {}
            stack_ok = (
                stack.get("manifest_version") == expected.get("manifest_version")
                and stack.get("root_hash") == expected.get("root_hash")
            )
            rec.teardown = {
                "pass": lease_ok and stack_ok,
                "lease_registry_empty": lease_ok,
                "stack_unchanged": stack_ok,
            }
        except Exception as exc:
            rec.teardown = {
                "pass": False,
                "lease_registry_empty": False,
                "stack_unchanged": False,
                "error": str(exc),
            }
        finally:
            destroyed = manager("destroy_sandbox", "--sandbox-id", sandbox_id, timeout=240)
            cleanup.untrack(sandbox_id)
            rec.record("destroy_sandbox.json", destroyed)
        if not body_failed:
            assert rec.teardown["pass"], {"teardown": rec.teardown}
            assert not is_error(destroyed), destroyed


def exec_terminal(sandbox_id, rec, command, name, *, timeout_ms=300_000, timeout=360):
    rec.cmd(command)
    result = exec_command(
        sandbox_id,
        sh(command),
        yield_time_ms=60_000,
        timeout_ms=timeout_ms,
        timeout=timeout,
    )
    rec.record(f"{name}.json", result)
    assert_ok(result)
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    return result


def exec_publish_ok(sandbox_id, rec, command, name):
    result = exec_terminal(sandbox_id, rec, command, name)
    assert result.get("publish_rejected") is not True, result
    return result


def exec_publish_reject(sandbox_id, rec, command, name, *, reject_class=REJECT_CLASS):
    result = exec_terminal(sandbox_id, rec, command, name)
    assert result.get("publish_rejected") is True, result
    assert result.get("publish_reject_class") == reject_class, result
    return result


def exec_read(sandbox_id, rec, command, name):
    """A read-only fresh exec: empty capture, publish skipped, never rejected."""
    return exec_publish_ok(sandbox_id, rec, command, name)


def start_gated(sandbox_id, rec, command, name):
    gated = sh("read go\n" + command)
    rec.cmd(gated)
    result = exec_command(
        sandbox_id,
        gated,
        yield_time_ms=0,
        timeout_ms=600_000,
        timeout=120,
    )
    rec.record(f"{name}.json", result)
    assert_ok(result)
    assert result["status"] == "running", result
    assert result["command_session_id"], result
    assert result["workspace_session_id"], result
    return result


def release_gated(sandbox_id, rec, started, name, *, expect_reject=None):
    result = write_command_stdin(
        sandbox_id,
        started["command_session_id"],
        "go\n",
        yield_time_ms=120_000,
        timeout=720,
    )
    rec.record(f"{name}.json", result)
    assert_ok(result)
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    if expect_reject is None:
        assert result.get("publish_rejected") is not True, result
    else:
        assert result.get("publish_rejected") is True, result
        assert result.get("publish_reject_class") == expect_reject, result
    return result


def assert_read_equals(sandbox_id, rec, path, content, name=None):
    """Byte-equality via file_read: the line window plus the raw byte count.

    file_read returns the joined line window without the file's final newline;
    together with total_bytes this pins the exact file bytes.
    """
    result = file_read(sandbox_id, path)
    if name:
        rec.record(f"{name}.json", result)
    assert_ok(result)
    window = content[:-1] if content.endswith("\n") else content
    assert result["content"] == window, {"expected": window, "result": result}
    assert result["total_bytes"] == len(content.encode("utf-8")), {
        "expected_bytes": len(content.encode("utf-8")),
        "result": result,
    }
    return result


def assert_read_not_found(sandbox_id, rec, path, name=None):
    result = file_read(sandbox_id, path)
    if name:
        rec.record(f"{name}.json", result)
    assert_error(result, "not_found")
    return result


def assert_no_wh_visible(sandbox_id, rec, name):
    """No `.wh.*` name is visible anywhere in the merged workspace."""
    result = exec_read(
        sandbox_id, rec, "find /workspace -name '.wh.*' -print", name
    )
    assert result["output"] == "", result
    return result


def assert_manifest_version(sandbox_id, rec, expected, name):
    stack = assert_ok(layerstack(sandbox_id))
    rec.record(f"{name}.json", stack)
    assert stack["manifest_version"] == expected, {
        "expected_manifest_version": expected,
        "layerstack": stack,
    }
    return stack


def docker_sh(sandbox_id, script, *, timeout=60):
    proc = subprocess.run(
        ["docker", "exec", sandbox_id, "sh", "-c", script],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    return proc


def daemon_fd_count(sandbox_id):
    script = (
        "pid=1; "
        "for p in /proc/[0-9]*/comm; do "
        'if [ "$(cat $p 2>/dev/null)" = "sandbox-daemon" ]; then '
        'pid=${p#/proc/}; pid=${pid%/comm}; break; fi; done; '
        "ls /proc/$pid/fd 2>/dev/null | wc -l"
    )
    proc = docker_sh(sandbox_id, script)
    assert proc.returncode == 0, proc.stderr or proc.stdout
    return int(proc.stdout.strip())


_preconditions = {"ran": False, "error": None}


def assert_preconditions_once():
    """§1.1 P1–P3, once per pytest session. Hard-fail, never skip."""
    if _preconditions["ran"]:
        if _preconditions["error"] is not None:
            raise AssertionError(
                f"suite preconditions failed earlier: {_preconditions['error']}"
            )
        return
    _preconditions["ran"] = True
    try:
        _run_preconditions()
    except BaseException as exc:
        _preconditions["error"] = f"{type(exc).__name__}: {exc}"
        raise


def _run_preconditions():
    report_dir = REPORT_ROOT / "PRECONDITIONS"
    report_dir.mkdir(parents=True, exist_ok=True)
    results = {"run_id": RUN_ID, "generated_at": now_iso()}

    def flush():
        (report_dir / "preconditions.json").write_text(
            json.dumps(results, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )

    workspace = tempfile.mkdtemp(prefix="whres-preconditions-")
    created = manager(
        "create_sandbox",
        "--image",
        IMAGE,
        "--workspace-bind-root",
        workspace,
        timeout=600,
    )
    assert not is_error(created), created
    sandbox_id = created["id"]
    cleanup.track(sandbox_id)
    try:
        proc = docker_sh(
            sandbox_id,
            "findmnt -no FSTYPE /eos/layer-stack 2>/dev/null || "
            "awk '$2==\"/eos/layer-stack\"{print $3}' /proc/mounts",
        )
        fstype = proc.stdout.strip()
        results["P1"] = {"fstype": fstype, "stderr": proc.stderr}
        flush()
        assert fstype == "ext4", f"P1: /eos/layer-stack fstype={fstype!r}, expected ext4"

        session = create_session(sandbox_id)["workspace_session_id"]
        try:
            smoke = exec_in(
                sandbox_id,
                session,
                "touch /workspace/.wh.smoke && ls -a /workspace",
                yield_time_ms=60_000,
            )
            results["P2"] = {"exec": smoke}
            flush()
            assert not is_error(smoke), smoke
            assert smoke["status"] == "ok" and smoke["exit_code"] == 0, smoke
            assert ".wh.smoke" in smoke["output"].split(), smoke
        finally:
            destroyed = destroy_session(sandbox_id, session, grace_s=1)
            assert not is_error(destroyed), destroyed
        discarded = file_read(sandbox_id, ".wh.smoke")
        results["P2"]["post_destroy_read"] = discarded
        flush()
        assert_error(discarded, "not_found")

        seeded = file_write(sandbox_id, "probe.txt", "probe\n")
        assert not is_error(seeded), seeded
        removal = exec_command(
            sandbox_id,
            sh("rm /workspace/probe.txt"),
            yield_time_ms=60_000,
            timeout_ms=300_000,
            timeout=360,
        )
        results["P3"] = {"exec": removal}
        flush()
        assert not is_error(removal), removal
        assert removal["status"] == "ok" and removal["exit_code"] == 0, removal
        assert removal.get("publish_rejected") is not True, removal
        gone = file_read(sandbox_id, "probe.txt")
        results["P3"]["post_delete_read"] = gone
        flush()
        assert_error(gone, "not_found")

        results["status"] = "pass"
        flush()
    finally:
        manager("destroy_sandbox", "--sandbox-id", sandbox_id, timeout=240)
        cleanup.untrack(sandbox_id)
        shutil.rmtree(workspace, ignore_errors=True)


def finalize_summary(exitstatus=None):
    with _summary_lock:
        REPORT_ROOT.mkdir(parents=True, exist_ok=True)
        verdicts = []
        for path in sorted(REPORT_ROOT.glob("*/verdict.json")):
            try:
                verdicts.append(json.loads(path.read_text(encoding="utf-8")))
            except (OSError, json.JSONDecodeError):
                continue
        counts = {}
        for verdict in verdicts:
            status = verdict.get("status", "fail")
            counts[status] = counts.get(status, 0) + 1
        preconditions = "not_run"
        precondition_path = REPORT_ROOT / "PRECONDITIONS" / "preconditions.json"
        if precondition_path.exists():
            try:
                payload = json.loads(precondition_path.read_text(encoding="utf-8"))
                preconditions = payload.get("status", "fail")
            except (OSError, json.JSONDecodeError):
                preconditions = "fail"
        lines = [
            "# Reserved `.wh.` Namespace Verdict Summary",
            "",
            f"- Run id: `{RUN_ID}`",
            f"- Generated: `{now_iso()}`",
            f"- Exit status: `{exitstatus}`",
            f"- Preconditions (P1-P3): `{preconditions}`",
            f"- Cases with verdicts: `{len(verdicts)}` of 16",
            f"- Counts: `{json.dumps(counts, sort_keys=True)}`",
            "",
            "| Case | Tier | Status | Correctness | Data-safety | Isolation | Teardown |",
            "| --- | --- | --- | --- | --- | --- | --- |",
        ]
        for verdict in verdicts:
            axes = verdict.get("axes", {})

            def cell(axis):
                if axis.get("status") == "n/a":
                    return "n/a"
                return "pass" if axis.get("pass") else "fail"

            lines.append(
                "| `{}` | `{}` | `{}` | `{}` | `{}` | `{}` | `{}` |".format(
                    verdict.get("case_id"),
                    verdict.get("tier"),
                    verdict.get("status"),
                    cell(axes.get("correctness", {})),
                    cell(axes.get("data_safety", {})),
                    cell(axes.get("isolation", {})),
                    "pass" if verdict.get("teardown", {}).get("pass") else "fail",
                )
            )
        lines += ["", "## Notes", ""]
        lines += [f"- {note}" for note in SUITE_NOTES]
        lines.append("")
        (REPORT_ROOT / "SUMMARY.md").write_text("\n".join(lines), encoding="utf-8")
        return REPORT_ROOT / "SUMMARY.md"


atexit.register(finalize_summary)

__all__ = [
    "CaseRecorder",
    "REJECT_CLASS",
    "REPORT_ROOT",
    "RUN_ID",
    "assert_error",
    "assert_manifest_version",
    "assert_no_wh_visible",
    "assert_ok",
    "assert_preconditions_once",
    "assert_read_equals",
    "assert_read_not_found",
    "_assert_publish_rejection",
    "_assert_stack_unchanged",
    "squash_layerstacks",
    "create_session",
    "daemon_fd_count",
    "destroy_session",
    "exec_in",
    "exec_publish_ok",
    "exec_publish_reject",
    "exec_read",
    "exec_terminal",
    "file_read",
    "file_write",
    "finalize_summary",
    "layerstack",
    "release_gated",
    "sh",
    "start_gated",
    "wh_case",
]
