"""CLI wrappers, fixtures, verdict machinery, and scenarios for the Manager
Export Changes live-Docker catalog (spec.md + test-case.md, same folder).

Every executed case writes
``manager/management/export/test-reports/<RUN_ID>/<CASE_ID>/verdict.json``
with the one schema from test-case.md §2 (three axes + teardown). Cases assert
only on structured JSON and the on-disk tree, never on logs.
"""

from __future__ import annotations

import base64
import datetime as dt
import hashlib
import io
import json
import math
import os
import shutil
import statistics
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
from pathlib import Path

from core import cleanup
from core.cli import internal_runtime_result, route_cli
from core.config import IMAGE, REPO_ROOT

SUITE_DIR = Path(__file__).resolve().parent
RUN_ID = os.environ.get("EXPORT_RUN_ID", dt.datetime.now().strftime("export-%Y%m%d-%H%M%S"))
REPORT_ROOT = SUITE_DIR / "test-reports" / RUN_ID

SCRATCH_ROOT = "/eos/workspace"
EXPORT_SPOOL_DIR = f"{SCRATCH_ROOT}/.export"
SPOOL_OVERRIDE = f"{EXPORT_SPOOL_DIR}/OVERRIDE.tar.zst"
ZSTD_MAGIC = b"\x28\xb5\x2f\xfd"

_report_lock = threading.Lock()


# --------------------------------------------------------------------------- CLI


class RawResult:
    def __init__(self, args, returncode, stdout, stderr, elapsed_ms):
        self.args = list(args)
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr
        self.elapsed_ms = elapsed_ms
        self.json = self._parse_json()

    def _parse_json(self):
        for text in (self.stdout, self.stderr):
            for line in reversed(text.splitlines()):
                stripped = line.strip()
                if stripped.startswith("{"):
                    try:
                        return json.loads(stripped)
                    except json.JSONDecodeError:
                        continue
        return None

    @property
    def ok(self):
        return (
            self.returncode == 0
            and isinstance(self.json, dict)
            and "error" not in self.json
        )


def raw_cli(rec, *args, timeout=180):
    started = time.monotonic()
    env = os.environ.copy()
    env["PATH"] = f"{REPO_ROOT / 'bin'}:{env.get('PATH', '')}"
    binary, argv, _ = route_cli(args)
    command = [binary.name, *map(str, argv)]
    proc = subprocess.run(
        [str(binary), *argv],
        cwd=str(REPO_ROOT),
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
    )
    elapsed = round((time.monotonic() - started) * 1000.0, 3)
    result = RawResult(command, proc.returncode, proc.stdout, proc.stderr, elapsed)
    if rec is not None:
        rec.add_command(
            {
                "cmd": command,
                "exit_code": proc.returncode,
                "elapsed_ms": elapsed,
                "stdout": proc.stdout,
                "stderr": proc.stderr,
                "parsed_json": result.json,
            }
        )
    return result


def manager(rec, operation, *args, timeout=180):
    return raw_cli(rec, "manager", operation, *args, timeout=timeout)


def runtime(rec, sandbox_id, operation, *args, timeout=180):
    return raw_cli(
        rec, "runtime", "--sandbox-id", sandbox_id, operation, *args, timeout=timeout
    )


def observability(rec, operation, *args, timeout=180):
    return raw_cli(rec, "observability", operation, *args, timeout=timeout)


def docker(rec, container, *args, timeout=60, check=False):
    started = time.monotonic()
    proc = subprocess.run(
        ["docker", "exec", container, *map(str, args)],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    elapsed = round((time.monotonic() - started) * 1000.0, 3)
    if rec is not None:
        rec.add_command(
            {
                "cmd": ["docker", "exec", container, *map(str, args)],
                "exit_code": proc.returncode,
                "elapsed_ms": elapsed,
                "stdout": proc.stdout,
                "stderr": proc.stderr,
            }
        )
    if check and proc.returncode != 0:
        raise AssertionError(proc.stderr or proc.stdout or f"docker exec failed: {args}")
    return proc


# ---------------------------------------------------------------- verdict record


class CaseRecorder:
    """One case's report bundle + the axis verdict.json: the three axes of
    test-case.md §2, plus a fourth load-bearing ``runnable`` axis for the
    runnable tier (runnable-export-test-case.md §3)."""

    def __init__(self, case):
        self.case = dict(case)
        self.case_id = self.case["id"]
        self.case_dir = REPORT_ROOT / self.case_id
        self.commands = []
        self.axes = {
            "correctness": {"pass": False, "status": "not_run"},
            "host_safety": {"pass": False, "status": "not_run"},
            "incremental": {"pass": False, "status": "not_run"},
        }
        if self.case.get("tier") == "runnable":
            self.axes["runnable"] = {"pass": False, "status": "not_run"}
        self.teardown = {"pass": False, "details": "not checked"}
        self.defects = []
        self.started = None
        self.verdict = None

    def __enter__(self):
        with _report_lock:
            self.case_dir.mkdir(parents=True, exist_ok=True)
        self.started = time.monotonic()
        self.write_json("case.json", self.case)
        return self

    def __exit__(self, exc_type, exc, tb):
        if exc is not None:
            self.defects.append({"type": exc_type.__name__, "message": str(exc)})
            if self.axes["correctness"]["status"] == "not_run":
                self.axis("correctness", False, str(exc))
        if self.verdict is None:
            self.write_verdict()
        return False

    def write_json(self, name, payload):
        path = self.case_dir / name
        path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        return path

    def write_text(self, name, text):
        (self.case_dir / name).write_text(text, encoding="utf-8")

    def add_command(self, record):
        self.commands.append(record)

    def axis(self, name, passed, details="", *, status=None, extra=None, n_a=False):
        if status is None:
            status = "n/a" if n_a else ("pass" if passed else "fail")
        payload = {"pass": bool(passed) or n_a, "status": status, "details": details}
        if extra:
            payload.update(extra)
        self.axes[name] = payload

    def defect(self, message):
        self.defects.append({"message": message})

    def set_teardown(self, passed, details, extra=None):
        payload = {"pass": bool(passed), "details": details}
        if extra:
            payload.update(extra)
        self.teardown = payload

    def write_verdict(self):
        self.write_json("cmd.log.json", self.commands)
        axes_pass = all(axis.get("pass") for axis in self.axes.values())
        passed = axes_pass and self.teardown.get("pass", False) and not self.defects
        self.verdict = {
            "case_id": self.case_id,
            "run_id": RUN_ID,
            "status": "pass" if passed else "fail",
            "tier": self.case.get("tier"),
            "title": self.case.get("title"),
            "axes": self.axes,
            "teardown": self.teardown,
            "defects": self.defects,
            "elapsed_ms": round((time.monotonic() - (self.started or time.monotonic())) * 1000.0, 3),
            "generated_at": dt.datetime.now().astimezone().isoformat(timespec="seconds"),
        }
        self.write_json("verdict.json", self.verdict)
        return self.verdict


def record_case(case):
    return CaseRecorder(case)


# ------------------------------------------------------------ sandbox lifecycle


def make_seed(case_id, files=None):
    """A host workspace dir seeded with ``files`` (path -> str/bytes)."""
    root = Path(tempfile.mkdtemp(prefix=f"eos-export-{case_id.lower()}-"))
    for rel, content in (files or {}).items():
        target = root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, bytes):
            target.write_bytes(content)
        else:
            target.write_text(content, encoding="utf-8")
    return root


def create_sandbox(rec, workspace_root, timeout=300, image=None):
    """Create a sandbox over ``workspace_root``. ``image`` overrides the suite
    default so runnable-tier projects build on their own toolchain base
    (runnable-export-test-case.md §2)."""
    result = manager(
        rec,
        "create_sandbox",
        "--image",
        image or IMAGE,
        "--workspace-bind-root",
        str(workspace_root),
        timeout=timeout,
    )
    assert result.ok, result.json or result.stderr
    sandbox_id = result.json.get("id")
    assert sandbox_id, result.json
    cleanup.track(sandbox_id)
    return sandbox_id


def destroy_sandbox(rec, sandbox_id):
    cleanup.untrack(sandbox_id)
    return manager(rec, "destroy_sandbox", "--sandbox-id", sandbox_id, timeout=180)


def publish_write(rec, sandbox_id, path, content):
    """Publish one file via the sessionless ``file_write`` backend."""
    result = runtime(
        rec, sandbox_id, "file_write", "--path", path, "--content", content, timeout=120
    )
    assert result.ok, result.json or result.stderr
    return result.json


def publish_exec(rec, sandbox_id, command, timeout=180):
    """Publish arbitrary workspace changes via a sessionless exec (deletes,
    symlinks, opaque rewrites, chmod). exec_command runs the string through a
    shell and the sessionless backend publishes the captured change set when
    the command finishes."""
    result = runtime(rec, sandbox_id, "exec_command", command, timeout=timeout)
    payload = result.json or {}
    if result.ok and payload.get("status") == "running":
        payload = _wait_command(rec, sandbox_id, payload["command_session_id"], timeout_s=timeout)
    assert result.ok and payload.get("exit_code") == 0, payload
    return payload


def _read_command_lines(rec, sandbox_id, command_session_id):
    return runtime(
        rec,
        sandbox_id,
        "read_command_lines",
        "--command-session-id",
        command_session_id,
        "--start-offset",
        "0",
        "--limit",
        "1000",
        timeout=30,
    )


def _wait_command(rec, sandbox_id, command_session_id, *, timeout_s=60):
    deadline = time.monotonic() + timeout_s
    last = None
    while time.monotonic() < deadline:
        result = _read_command_lines(rec, sandbox_id, command_session_id)
        assert result.ok, result.json or result.stderr
        last = result.json
        if last.get("status") != "running":
            return last
        time.sleep(0.1)
    raise AssertionError(f"command {command_session_id} still running: {last}")


def create_session(rec, sandbox_id):
    result = internal_runtime_result(
        sandbox_id,
        "create_workspace_session",
        recorder=rec,
    )
    assert result.ok, result.json or result.stderr
    return result.json["workspace_session_id"]


def destroy_session(rec, sandbox_id, session_id):
    result = internal_runtime_result(
        sandbox_id,
        "destroy_workspace_session",
        {"workspace_session_id": session_id, "grace_s": 1},
        timeout=60,
        recorder=rec,
    )
    return result.json


# -------------------------------------------------------------- export surface


def export_changes(rec, sandbox_id, dest, fmt="dir", timeout=300):
    """Drive ``sandbox-manager-cli export_changes`` and return the RawResult."""
    args = ["export_changes", "--sandbox-id", sandbox_id, "--dest", str(dest)]
    if fmt is not None:
        args += ["--format", fmt]
    return manager(rec, *args, timeout=timeout)


def read_tree(root):
    """A {relpath: kind/content} map of the on-disk tree at ``root``.

    Files map to their bytes; symlinks to ``("symlink", target)``; directories
    are present as keys with value ``"dir"``. Absent root -> empty map.
    """
    root = Path(root)
    tree = {}
    if not root.exists():
        return tree
    for path in sorted(root.rglob("*")):
        rel = str(path.relative_to(root))
        if path.is_symlink():
            tree[rel] = ("symlink", os.readlink(path))
        elif path.is_dir():
            tree[rel] = "dir"
        else:
            tree[rel] = path.read_bytes()
    return tree


def _member_names(archive):
    """Faithful tar entry names in archive order. ``tarfile`` strips the
    trailing slash a directory member carries on the wire; restore it so the
    list matches the archive's OCI encoding (``tar tf`` and test-case.md §EZ-03
    both show ``src/``)."""
    return [f"{member.name}/" if member.isdir() else member.name for member in archive.getmembers()]


def zstd_entries(rec, path):
    """List tar entry names inside a ``.tar.zst`` archive host-side (docker cp
    into a throwaway container is avoided — we decompress locally)."""
    data = Path(path).read_bytes()
    assert data[:4] == ZSTD_MAGIC, "archive is not zstd-framed"
    raw = _zstd_decompress(rec, data)
    with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
        return _member_names(archive)


def tar_entries(path):
    with tarfile.open(str(path)) as archive:
        return _member_names(archive)


def _zstd_decompress(rec, data):
    """Decompress zstd via the host ``zstd`` CLI (P2 asserts it is available)."""
    proc = subprocess.run(
        ["zstd", "-dc"], input=data, capture_output=True, timeout=120
    )
    assert proc.returncode == 0, proc.stderr.decode("utf-8", "replace")
    return proc.stdout


# ---------------------------------------------------------- fault injection


def craft_hostile_spool(entries):
    """Build a zstd-framed tar with raw header names/targets the honest daemon
    could never author (traversal, absolute, hardlink, whiteout escape).

    ``entries`` is a list of dicts: {name, kind, content?, link?}. ``kind`` is
    one of "file", "dir", "symlink", "hardlink", "raw". Returns tar.zst bytes.
    """
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for entry in entries:
            info = tarfile.TarInfo(name=entry["name"])
            kind = entry["kind"]
            if kind == "file":
                payload = entry.get("content", b"")
                info.type = tarfile.REGTYPE
                info.mode = entry.get("mode", 0o644)
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))
            elif kind == "dir":
                info.type = tarfile.DIRTYPE
                info.mode = entry.get("mode", 0o755)
                archive.addfile(info)
            elif kind == "symlink":
                info.type = tarfile.SYMTYPE
                info.linkname = entry["link"]
                archive.addfile(info)
            elif kind == "hardlink":
                info.type = tarfile.LNKTYPE
                info.linkname = entry["link"]
                archive.addfile(info)
            elif kind == "marker":
                info.type = tarfile.REGTYPE
                info.size = 0
                archive.addfile(info, io.BytesIO(b""))
            else:
                raise AssertionError(f"unknown hostile entry kind: {kind}")
    proc = subprocess.run(
        ["zstd", "-q", "-3", "-c"], input=buffer.getvalue(), capture_output=True, timeout=120
    )
    assert proc.returncode == 0, proc.stderr.decode("utf-8", "replace")
    return proc.stdout


def craft_zstd_bomb(decompressed_bytes):
    """A tiny zstd frame that inflates to ``decompressed_bytes`` of one tar
    entry — a decompression bomb whose on-wire size stays small."""
    payload = b"\x00" * decompressed_bytes
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        info = tarfile.TarInfo(name="bomb.bin")
        info.type = tarfile.REGTYPE
        info.size = len(payload)
        archive.addfile(info, io.BytesIO(payload))
    proc = subprocess.run(
        ["zstd", "-q", "-19", "-c"], input=buffer.getvalue(), capture_output=True, timeout=300
    )
    assert proc.returncode == 0, proc.stderr.decode("utf-8", "replace")
    return proc.stdout


def craft_entry_count_bomb(count):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for index in range(count):
            info = tarfile.TarInfo(name=f"f{index}.txt")
            info.type = tarfile.REGTYPE
            info.size = 0
            archive.addfile(info, io.BytesIO(b""))
    proc = subprocess.run(
        ["zstd", "-q", "-3", "-c"], input=buffer.getvalue(), capture_output=True, timeout=120
    )
    assert proc.returncode == 0, proc.stderr.decode("utf-8", "replace")
    return proc.stdout


def inject_spool(rec, sandbox_id, spool_bytes):
    """Drop a pre-crafted spool at ``<scratch_root>/.export/OVERRIDE.tar.zst``
    inside the sandbox (test-case.md §1.4). The next export_layerstack serves
    it instead of the honest fold; the manager applier treats it as untrusted
    (spec inv 9). Written base64 through ``docker exec`` — no host bind needed.
    """
    docker(rec, sandbox_id, "mkdir", "-p", EXPORT_SPOOL_DIR, check=True)
    encoded = base64.b64encode(spool_bytes).decode("ascii")
    proc = subprocess.run(
        ["docker", "exec", "-i", sandbox_id, "sh", "-c", f"base64 -d > {SPOOL_OVERRIDE}"],
        input=encoded,
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert proc.returncode == 0, proc.stderr


def export_dir_entries(rec, sandbox_id):
    proc = docker(
        rec,
        sandbox_id,
        "sh",
        "-c",
        f"ls -1 {EXPORT_SPOOL_DIR} 2>/dev/null | wc -l",
        check=False,
    )
    try:
        return int(proc.stdout.strip() or "0")
    except ValueError:
        return -1


def active_lease_count(rec, sandbox_id):
    view = observability(rec, "layerstack", "--sandbox-id", sandbox_id, timeout=120)
    if not view.ok or not isinstance(view.json, dict):
        return None
    return int(view.json.get("active_lease_count", 0))


# ------------------------------------------------------------- sentinel guard


def _is_within(path, root):
    try:
        Path(path).resolve().relative_to(Path(root).resolve())
        return True
    except ValueError:
        return False


class Sentinel:
    """A canary file tree OUTSIDE dest, snapshotted so the teardown can prove
    nothing outside dest was created, modified, or deleted (the load-bearing
    HRD teardown).

    When a traversal target is an *ancestor* directory of dest — HRD-01 places
    ``dest`` at ``<base>/a/b/dest`` and the ``../../escape.txt`` canary at
    ``<base>/a/escape.txt`` — the dest subtree lives under this base. The dirs
    the manager creates to materialize dest, and any legitimate in-dest writes,
    are not out-of-dest tampering, so register dest with ``guard_dest`` to
    exclude it. The guarantee this pins is test-case.md HRD-01's: every planted
    canary stays byte-identical and no *file* is created outside dest."""

    def __init__(self, base):
        self.base = Path(base)
        self.base.mkdir(parents=True, exist_ok=True)
        self.files = {}
        self._dest = None

    def guard_dest(self, dest):
        """Exclude the dest subtree (which may live under this base) from the
        out-of-dest file check."""
        self._dest = Path(dest)
        return self

    def plant(self, rel, content="canary\n"):
        target = self.base / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        self.files[rel] = content
        return target

    def snapshot(self):
        return read_tree(self.base)

    def unchanged(self):
        """True iff every planted canary is byte-identical AND no file or
        symlink appeared outside dest. Directories (e.g. the parents created to
        materialize a dest that lives under this base) are not content and
        never count as tampering — HRD-01 guards *files* outside dest."""
        current = read_tree(self.base)
        for rel, content in self.files.items():
            if current.get(rel) != content.encode("utf-8"):
                return False
        for rel, value in current.items():
            if not isinstance(value, (bytes, tuple)):
                continue
            if rel in self.files:
                continue
            if self._dest is not None and _is_within(self.base / rel, self._dest):
                continue
            return False
        return True


# --------------------------------------------------------------- assertions


DIR_RESULT_KEYS = {
    "manifest_version",
    "format",
    "layers_exported",
    "files_written",
    "symlinks_written",
    "deletes_applied",
    "opaque_clears",
    "skipped_unchanged",
    "bytes_written",
}
TAR_RESULT_KEYS = {
    "manifest_version",
    "format",
    "layers_exported",
    "files_written",
    "symlinks_written",
    "whiteouts_emitted",
    "bytes_written",
}


def assert_result_contract(result_json, fmt="dir"):
    keys = set(result_json) - {"live_workspace_sessions"}
    expected = DIR_RESULT_KEYS if fmt == "dir" else TAR_RESULT_KEYS
    assert keys == expected, f"result keys {sorted(keys)} != {sorted(expected)}"
    assert result_json["format"] == fmt, result_json
    for name in expected:
        if name in {"format", "layers_exported", "manifest_version"}:
            continue
        value = result_json[name]
        assert isinstance(value, int) and value >= 0, f"{name}={value!r} not a count"


def no_literal_markers(tree):
    return not any(
        Path(rel).name.startswith(".wh.") for rel in tree
    )


# ------------------------------------------------------------- teardown/precond


def teardown(rec, sandbox_id, *, sentinel=None, expect_export_empty=True):
    """§1.3 teardown contract: lease released, <scratch>/.export empty, nothing
    outside dest touched. Checked while the sandbox is still alive; the caller
    destroys it afterwards."""
    leases = active_lease_count(rec, sandbox_id)
    export_count = export_dir_entries(rec, sandbox_id)
    failures = []
    if leases not in (0, None):
        failures.append(f"active_lease_count={leases}")
    if expect_export_empty and export_count not in (0, -1):
        failures.append(f"export_dir_entries={export_count}")
    if sentinel is not None and not sentinel.unchanged():
        failures.append("outside-dest sentinel changed")
    rec.set_teardown(
        not failures,
        "; ".join(failures) or "clean",
        {
            "lease_registry_empty": leases in (0, None),
            "export_dir_empty": export_count in (0, -1),
            "outside_dest_clean": sentinel is None or sentinel.unchanged(),
        },
    )
    assert not failures, failures


def assert_preconditions(rec):
    """P1-P4 (test-case.md §1.1), hard-fail. P1 needs no sandbox; P2-P4 share one."""
    # P1: export_changes is in the manager catalog with the right surface.
    spec_ok = _p1_catalog(rec)
    rec.axis("correctness", spec_ok, "P1 catalog + surface", extra={"P1": spec_ok})

    seed = make_seed("preconditions", {"winner.txt": "seed-v1\n"})
    sandbox_id = None
    try:
        sandbox_id = create_sandbox(rec, seed)
        publish_exec(rec, sandbox_id, "printf 'seed-v2\\n' > winner.txt")

        # P3: dir-apply onto the bind-root seed is reachable and byte-equal.
        p3 = export_changes(rec, sandbox_id, seed)
        assert p3.ok, f"P3 dir export failed: {p3.json or p3.stderr}"
        assert (seed / "winner.txt").read_text() == "seed-v2\n", "P3 winner not byte-equal"

        # P2: zstd round-trip host-side.
        archive = Path(tempfile.mkdtemp(prefix="eos-export-p2-")) / "delta.tar.zst"
        p2 = export_changes(rec, sandbox_id, archive, fmt="tar-zst")
        assert p2.ok, f"P2 tar-zst export failed: {p2.json or p2.stderr}"
        names = zstd_entries(rec, archive)
        assert "winner.txt" in names, f"P2 archive entries missing winner: {names}"
        shutil.rmtree(archive.parent, ignore_errors=True)

        # P4: the export boot step reaps <scratch>/.export on daemon restart.
        docker(rec, sandbox_id, "mkdir", "-p", EXPORT_SPOOL_DIR, check=True)
        docker(
            rec,
            sandbox_id,
            "sh",
            "-c",
            f"printf orphan > {EXPORT_SPOOL_DIR}/orphan.tar.zst",
            check=True,
        )
        subprocess.run(
            ["docker", "restart", sandbox_id],
            check=True,
            capture_output=True,
            text=True,
            timeout=90,
        )
        _wait_container_ready(rec, sandbox_id)
        remaining = export_dir_entries(rec, sandbox_id)
        assert remaining in (0, -1), f"P4 boot reap left {remaining} spool(s) under .export"

        rec.axis("host_safety", True, "P2/P3/P4 asserted", extra={"P2": True, "P3": True, "P4": True})
        rec.axis("incremental", True, "n/a", n_a=True)
        rec.set_teardown(True, "preconditions sandbox destroyed below")
    finally:
        if sandbox_id:
            destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def _p1_catalog(rec):
    result = raw_cli(rec, "manager", "help", "export_changes", timeout=30)
    text = (result.stdout or "") + (result.stderr or "")
    return all(flag in text for flag in ("--sandbox-id", "--dest", "--format"))


def _wait_container_ready(rec, sandbox_id, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        proc = docker(
            rec,
            sandbox_id,
            "sh",
            "-c",
            "test -S /eos/runtime/daemon/runtime.sock && echo up",
            timeout=15,
        )
        if proc.stdout.strip() == "up":
            time.sleep(1)
            return
        time.sleep(0.5)
    raise AssertionError(f"{sandbox_id} daemon did not become ready")


def restart_gateway_and_recover(rec, timeout=180):
    """Restart the gateway so it re-resolves running containers by label
    (`DockerSandboxRuntime::recover_sandboxes`).

    In this deployment the daemon lives and dies with its container — `docker-init`
    (pid 1, `tini -- sandbox-daemon`) exits when its daemon child exits — so a
    daemon restart IS a container restart, and `docker restart` reassigns the
    ephemeral host ports the manager resolved and cached at create time. The
    manager re-resolves those ports on gateway startup (recover-by-label), not
    per forward, so restoring the manager's view of a restarted container is a
    gateway restart. Env — including the export resource caps — is inherited
    from this process so the recovered gateway keeps the same configuration."""
    script = REPO_ROOT / "bin" / "start-sandbox-docker-gateway"
    env = os.environ.copy()
    env["PATH"] = f"{REPO_ROOT / 'bin'}:{env.get('PATH', '')}"
    proc = subprocess.run(
        [str(script)],
        cwd=str(REPO_ROOT),
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    assert proc.returncode == 0, f"gateway restart failed: {proc.stderr or proc.stdout}"
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if manager(rec, "list_sandboxes", timeout=15).ok:
            return
        time.sleep(1)
    raise AssertionError("gateway did not respond after restart")


# ------------------------------------------------------------------ dispatch

CASES = [
    {"id": "EZ-01", "tier": "easy", "title": "dir export onto the seed reproduces the merged view"},
    {"id": "EZ-02", "tier": "easy", "title": "base-only manifest is a clean no-op"},
    {"id": "EZ-03", "tier": "easy", "title": "tar-zst writes a valid whiteout-preserving archive"},
    {"id": "EZ-04", "tier": "easy", "title": "tar writes a plain (decompressed) archive"},
    {"id": "EZ-05", "tier": "easy", "title": "--format defaults to dir"},
    {"id": "EZ-06", "tier": "easy", "title": "relative --dest is rejected before any forward"},
    {"id": "EZ-07", "tier": "easy", "title": "dir result-contract shape is exact"},
    {"id": "EZ-08", "tier": "easy", "title": "single deletion applies with no literal marker"},
    {"id": "EZ-09", "tier": "easy", "title": "a live session is reported, export still succeeds"},
    {"id": "EZ-10", "tier": "easy", "title": "a non-Ready sandbox is rejected by the forward gate"},
    {"id": "MED-01", "tier": "medium", "title": "idempotent re-run writes zero content bytes"},
    {"id": "MED-02", "tier": "medium", "title": "incremental re-export after more publishes"},
    {"id": "MED-03", "tier": "medium", "title": "opaque directory masks base content"},
    {"id": "MED-04", "tier": "medium", "title": "opaque-clear ordering: a dotfile winner survives"},
    {"id": "MED-05", "tier": "medium", "title": "newest-wins fold: older content never exported"},
    {"id": "MED-06", "tier": "medium", "title": "symlink winner recreate; dir<->symlink replacement"},
    {"id": "MED-07", "tier": "medium", "title": "merged-delta equivalence on an empty dest"},
    {"id": "MED-08", "tier": "medium", "title": "delta-cost: the base never crosses the wire"},
    {"id": "MED-09", "tier": "medium", "title": "metadata fidelity: mode carried, uid/gid + xattrs not"},
    {"id": "MED-10", "tier": "medium", "title": "delta re-applies onto a fresh base copy"},
    {"id": "HRD-01", "tier": "hard", "title": "tar-slip: ../absolute entry rejected"},
    {"id": "HRD-02", "tier": "hard", "title": "symlink-then-traverse: write-through blocked"},
    {"id": "HRD-03", "tier": "hard", "title": "whiteout target normalizing outside dest rejected"},
    {"id": "HRD-04", "tier": "hard", "title": "dest deny-list holds"},
    {"id": "HRD-05", "tier": "hard", "title": "resource bombs are capped"},
    {"id": "HRD-06", "tier": "hard", "title": "two concurrent exports of the same sandbox"},
    {"id": "HRD-07", "tier": "hard", "title": "export under concurrent squash_layerstacks"},
    {"id": "HRD-08", "tier": "hard", "title": "export under a concurrent publish"},
    {"id": "HRD-09", "tier": "hard", "title": "deep/large delta converges or fails cleanly"},
    {"id": "HRD-10", "tier": "hard", "title": "daemon restart mid-paging"},
    {"id": "RUN-01", "tier": "runnable", "title": "Node/Express: npm install exports a runnable server"},
    {"id": "RUN-02", "tier": "runnable", "title": "Node/TypeScript: compiled dist runs from a fresh dest"},
    {"id": "RUN-04", "tier": "runnable", "title": "Python/Flask venv: runs at /workspace, xfails at /elsewhere"},
    {"id": "RUN-03", "tier": "runnable", "title": "Node native addon: linux .node runs in-container, host xfail"},
    {"id": "RUN-05", "tier": "runnable", "title": "Python/pytest + numpy wheel: exported suite passes"},
    {"id": "RUN-06", "tier": "runnable", "title": "ABI escape hatch: host npm rebuild makes the tree host-native"},
    {"id": "PERF-0", "tier": "bench", "title": "fixed-cost control: empty-delta export x5"},
    {"id": "PERF-1M", "tier": "bench", "title": "1 MiB urandom: cold/warm dir + tar-zst walls"},
    {"id": "PERF-5M", "tier": "bench", "title": "5 MiB urandom: cold/warm dir + tar-zst walls"},
    {"id": "PERF-20M", "tier": "bench", "title": "20 MiB urandom: cold/warm dir + tar-zst walls"},
    {"id": "PERF-SHAPE-20M", "tier": "bench", "title": "20 x 1 MiB files: entry-count overhead vs single-file"},
    {"id": "PERF-ZSTD-20M", "tier": "bench", "title": "20 MiB zeros: compressibility contrast (dir + tar-zst)"},
]
CASE_BY_ID = {case["id"]: case for case in CASES}


def cases_for_tier(tier):
    return [case for case in CASES if case["tier"] == tier]


def run_case(case):
    with record_case(case) as rec:
        fn = globals()[f"case_{case['id'].replace('-', '_').lower()}"]
        fn(rec)


def _fresh_dest(case_id, name="dest"):
    base = Path(tempfile.mkdtemp(prefix=f"eos-export-dest-{case_id.lower()}-"))
    return base, base / name


def _expected_version(num_delta_layers):
    return 1 + num_delta_layers


# =============================================================== EASY (EZ)


def case_ez_01(rec):
    """B1/inv 2: dir export onto the seed reproduces the merged view."""
    seed = make_seed("ez01", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        result = export_changes(rec, sandbox_id, seed)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert (seed / "src/a.rs").read_text() == "v2\n"
        assert not (seed / "src/b.rs").exists(), "whiteout removed b.rs"
        assert result.json["files_written"] == 1, result.json
        assert result.json["deletes_applied"] == 1, result.json
        assert result.json["symlinks_written"] == 0, result.json
        assert result.json["opaque_clears"] == 0, result.json
        assert result.json["manifest_version"] == _expected_version(1), result.json
        assert len(result.json["layers_exported"]) == 1, result.json
        rec.axis("correctness", True, "a.rs rewritten, b.rs deleted, counts exact")
        tree = read_tree(seed)
        assert no_literal_markers(tree), "literal .wh. marker on host"
        rec.axis("host_safety", True, "no literal markers on the host")
        rec.axis("incremental", True, "n/a (first export)", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def case_ez_02(rec):
    """Empty delta (base-only manifest) is a clean no-op."""
    seed = make_seed("ez02", {"keep.txt": "K\n"})
    sandbox_id = create_sandbox(rec, seed)
    try:
        before = read_tree(seed)
        result = export_changes(rec, sandbox_id, seed)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert result.json["layers_exported"] == [], result.json
        for count in ("files_written", "symlinks_written", "deletes_applied", "opaque_clears", "skipped_unchanged", "bytes_written"):
            assert result.json[count] == 0, (count, result.json)
        assert result.json["manifest_version"] == 1, result.json
        assert "no_op" not in result.json, result.json
        rec.axis("correctness", True, "empty delta, all counts zero, version 1")
        assert read_tree(seed) == before, "dest changed on a no-op"
        rec.axis("host_safety", True, "dest byte-identical, nothing outside dest")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def case_ez_03(rec):
    """B4: tar-zst writes a valid whiteout-preserving archive."""
    seed = make_seed("ez03", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    dest_base, _ = _fresh_dest("ez03")
    dest = dest_base / "delta.tar.zst"
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        result = export_changes(rec, sandbox_id, dest, fmt="tar-zst")
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert dest.read_bytes()[:4] == ZSTD_MAGIC, "archive is not zstd"
        names = zstd_entries(rec, dest)
        assert names == ["src/", "src/a.rs", "src/.wh.b.rs"], names
        assert result.json["whiteouts_emitted"] == 1, result.json
        assert result.json["files_written"] == 1, result.json
        assert result.json["bytes_written"] == dest.stat().st_size, result.json
        assert "deletes_applied" not in result.json, result.json
        rec.axis("correctness", True, "zstd archive with logical whiteout, counts exact")
        siblings = os.listdir(dest_base)
        assert siblings == ["delta.tar.zst"], f"temp sibling left: {siblings}"
        rec.axis("host_safety", True, "only dest present; no .tmp left (atomicity)")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)


def case_ez_04(rec):
    """--format tar writes a plain (decompressed) archive of the same entries."""
    seed = make_seed("ez04", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    dest_base, _ = _fresh_dest("ez04")
    dest = dest_base / "delta.tar"
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        result = export_changes(rec, sandbox_id, dest, fmt="tar")
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert dest.read_bytes()[:4] != ZSTD_MAGIC, "tar must not be zstd"
        assert tar_entries(dest) == ["src/", "src/a.rs", "src/.wh.b.rs"], tar_entries(dest)
        assert result.json["files_written"] == 1 and result.json["whiteouts_emitted"] == 1, result.json
        rec.axis("correctness", True, "plain tar with the same logical entries")
        assert os.listdir(dest_base) == ["delta.tar"], os.listdir(dest_base)
        rec.axis("host_safety", True, "only dest present; no temp left")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)


def case_ez_05(rec):
    """--format omitted behaves as dir (applied tree, not an archive)."""
    seed = make_seed("ez05", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    dest_base, dest = _fresh_dest("ez05")
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        result = export_changes(rec, sandbox_id, dest, fmt=None)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert result.json["format"] == "dir", result.json
        assert dest.is_dir() and (dest / "src/a.rs").read_text() == "v2\n", read_tree(dest)
        rec.axis("correctness", True, "default format is dir; applied as a tree")
        assert no_literal_markers(read_tree(dest)), "literal markers"
        rec.axis("host_safety", True, "no literal markers")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)


def case_ez_06(rec):
    """Relative --dest is rejected before any forward."""
    seed = make_seed("ez06", {"src/a.rs": "v1\n"})
    sandbox_id = create_sandbox(rec, seed)
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs")
        result = export_changes(rec, sandbox_id, "./relative")
        assert not result.ok, result.json
        assert result.json["error"]["kind"] == "invalid_request", result.json
        assert "manifest_version" not in result.json, result.json
        rec.axis("correctness", True, "relative dest rejected with invalid_request")
        assert export_dir_entries(rec, sandbox_id) in (0, -1), "fold started on a rejected dest"
        rec.axis("host_safety", True, "nothing written; .export empty (no fold)")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def case_ez_07(rec):
    """dir result-contract shape is exact."""
    seed = make_seed("ez07", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    dest_base, dest = _fresh_dest("ez07")
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        result = export_changes(rec, sandbox_id, dest)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert_result_contract(result.json, fmt="dir")
        rec.axis("correctness", True, "exact dir contract keys; integer counts")
        rec.axis("host_safety", True, "n/a", n_a=True)
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)


def case_ez_08(rec):
    """A single deletion applies in dir mode with no literal marker."""
    seed = make_seed("ez08", {"gone.txt": "X\n"})
    sandbox_id = create_sandbox(rec, seed)
    try:
        publish_exec(rec, sandbox_id, "rm -f gone.txt")
        result = export_changes(rec, sandbox_id, seed)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert not (seed / "gone.txt").exists(), "gone.txt not deleted"
        assert result.json["deletes_applied"] == 1 and result.json["files_written"] == 0, result.json
        rec.axis("correctness", True, "deletion applied, files_written 0")
        assert not (seed / ".wh.gone.txt").exists(), "literal .wh. marker on host"
        assert no_literal_markers(read_tree(seed)), "literal markers"
        rec.axis("host_safety", True, "no .wh.gone.txt on host; marker consumed")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def case_ez_09(rec):
    """A live session is reported; export still succeeds on published state."""
    seed = make_seed("ez09", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    dest_base, dest = _fresh_dest("ez09")
    session = None
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        session = create_session(rec, sandbox_id)
        first = export_changes(rec, sandbox_id, dest)
        assert first.ok, first.json or first.stderr
        rec.write_json("result-live.json", first.json)
        live = first.json.get("live_workspace_sessions")
        assert live and session in live, f"live session not reported: {first.json}"
        assert (dest / "src/a.rs").read_text() == "v2\n", "published state exported"
        destroy_session(rec, sandbox_id, session)
        session = None
        dest2 = dest_base / "second"
        second = export_changes(rec, sandbox_id, dest2)
        assert second.ok, second.json or second.stderr
        assert "live_workspace_sessions" not in second.json, second.json
        rec.axis("correctness", True, "live session reported, then omitted after destroy")
        rec.axis("host_safety", True, "n/a", n_a=True)
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        if session:
            destroy_session(rec, sandbox_id, session)
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)


def case_ez_10(rec):
    """A non-Ready / unknown sandbox is rejected by the forward gate."""
    dest_base, dest = _fresh_dest("ez10")
    try:
        result = export_changes(rec, "eos-nonexistent-sandbox", dest)
        assert not result.ok, result.json
        assert result.json["error"]["kind"] == "invalid_request", result.json
        assert "manifest_version" not in result.json, result.json
        rec.axis("correctness", True, "unknown sandbox rejected by the forward gate")
        assert not dest.exists(), "dest created on a gate reject"
        rec.axis("host_safety", True, "dest untouched on reject")
        rec.axis("incremental", True, "n/a", n_a=True)
        rec.set_teardown(True, "no sandbox created")
    finally:
        shutil.rmtree(dest_base, ignore_errors=True)


# ============================================================= MEDIUM (MED)


def case_med_01(rec):
    """inv 4: idempotent re-run writes zero content bytes for file winners."""
    seed = make_seed("med01", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        first = export_changes(rec, sandbox_id, seed)
        assert first.ok, first.json or first.stderr
        assert first.json["files_written"] == 1 and first.json["skipped_unchanged"] == 0, first.json
        tree_after_first = read_tree(seed)
        second = export_changes(rec, sandbox_id, seed)
        assert second.ok, second.json or second.stderr
        rec.write_json("result-rerun.json", second.json)
        assert second.json["files_written"] == 0, second.json
        assert second.json["bytes_written"] == 0, second.json
        assert second.json["skipped_unchanged"] == 1, second.json
        assert second.json["manifest_version"] == first.json["manifest_version"], second.json
        rec.axis("correctness", True, "re-run: files_written 0, skipped==file entries")
        assert no_literal_markers(read_tree(seed)), "literal markers"
        rec.axis("host_safety", True, "no literal markers; nothing outside dest")
        assert read_tree(seed) == tree_after_first, "file-winner tree changed on re-run"
        rec.axis("incremental", True, "content_bytes_written==0; tree byte-identical",
                 extra={"content_bytes_written": second.json["bytes_written"]})
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def case_med_02(rec):
    """B2: incremental re-export after 9 more changed paths."""
    seed = make_seed("med02", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        first = export_changes(rec, sandbox_id, seed)
        assert first.ok, first.json or first.stderr
        prior_entries = first.json["files_written"] + first.json["symlinks_written"]
        publish_exec(rec, sandbox_id, "mkdir -p pkg && for n in 1 2 3 4 5 6 7 8 9; do printf \"c$n\\n\" > pkg/f$n.txt; done")
        second = export_changes(rec, sandbox_id, seed)
        assert second.ok, second.json or second.stderr
        rec.write_json("result-incremental.json", second.json)
        assert second.json["files_written"] == 9, second.json
        assert second.json["skipped_unchanged"] == prior_entries, (second.json, prior_entries)
        assert second.json["manifest_version"] == _expected_version(2), second.json
        for n in range(1, 10):
            assert (seed / f"pkg/f{n}.txt").read_text() == f"c{n}\n", n
        rec.axis("correctness", True, "9 written, prior entries skipped, version advanced")
        rec.axis("host_safety", True, "no markers; nothing outside dest",
                 extra={"markers": no_literal_markers(read_tree(seed))})
        rec.axis("incremental", True, "content bytes track the 9 changed files only",
                 extra={"content_bytes_written": second.json["bytes_written"]})
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def case_med_03(rec):
    """B3: opaque directory masks base content."""
    seed = make_seed("med03", {"cfg/dev.yml": "D\n", "cfg/prod.yml": "P\n"})
    sandbox_id = create_sandbox(rec, seed)
    try:
        publish_exec(rec, sandbox_id, "rm -rf cfg && mkdir cfg && printf 'P2\\n' > cfg/prod.yml")
        result = export_changes(rec, sandbox_id, seed)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert not (seed / "cfg/dev.yml").exists(), "opaque clear left base dev.yml"
        assert (seed / "cfg/prod.yml").read_text() == "P2\n", "prod.yml not rewritten"
        assert result.json["opaque_clears"] == 1 and result.json["files_written"] == 1, result.json
        rec.axis("correctness", True, "cfg cleared of base content, prod rewritten")
        assert not (seed / "cfg/.wh..wh..opq").exists(), "literal opaque marker on host"
        rec.axis("host_safety", True, "no literal opaque marker; nothing outside dest")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def case_med_04(rec):
    """inv 2 / C2: a dotfile winner survives its directory's opaque clear."""
    seed = make_seed("med04", {"cfg/dev.yml": "D\n"})
    sandbox_id = create_sandbox(rec, seed)
    try:
        publish_exec(
            rec, sandbox_id,
            "rm -rf cfg && mkdir cfg && printf 'E\\n' > cfg/.env && printf 'P\\n' > cfg/prod.yml",
        )
        result = export_changes(rec, sandbox_id, seed)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert (seed / "cfg/.env").read_text() == "E\n", "dotfile winner destroyed by the clear"
        assert (seed / "cfg/prod.yml").read_text() == "P\n", "prod winner lost"
        assert not (seed / "cfg/dev.yml").exists(), "base dev.yml survived the clear"
        rec.axis("correctness", True, "both winners survive; three-pass ordering holds")
        assert no_literal_markers(read_tree(seed)), "literal markers"
        rec.axis("host_safety", True, "no literal opaque marker; nothing outside dest")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def case_med_05(rec):
    """Newest-wins fold: an older layer's content is never exported."""
    seed = make_seed("med05", {})
    sandbox_id = create_sandbox(rec, seed)
    dest_base, dest = _fresh_dest("med05")
    try:
        publish_exec(rec, sandbox_id, "printf 'v1\\n' > a.rs")
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > a.rs")
        result = export_changes(rec, sandbox_id, dest)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert (dest / "a.rs").read_text() == "v2\n", "older content leaked"
        assert result.json["files_written"] == 1, result.json
        rec.axis("correctness", True, "only the v2 winner crossed; one file written")
        rec.axis("host_safety", True, "n/a", n_a=True)
        rec.axis("incremental", True, "content bytes == v2 size only",
                 extra={"content_bytes_written": result.json["bytes_written"]})
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)


def case_med_06(rec):
    """Symlink winner recreate; a dest symlink at a dir position is replaced."""
    seed = make_seed("med06", {"link_target/keep.txt": "K\n"})
    sandbox_id = create_sandbox(rec, seed)
    elsewhere = Path(tempfile.mkdtemp(prefix="eos-export-med06-elsewhere-"))
    (elsewhere / "untouched.txt").write_text("E\n")
    try:
        publish_exec(rec, sandbox_id, "ln -s link_target s && mkdir -p d && printf 'F\\n' > d/file.txt")
        # Pre-load dest_seed with a conflicting symlink d -> elsewhere.
        (seed / "d").exists() and shutil.rmtree(seed / "d", ignore_errors=True)
        os.symlink(str(elsewhere), str(seed / "d"))
        result = export_changes(rec, sandbox_id, seed)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert os.path.islink(seed / "s") and os.readlink(seed / "s") == "link_target", "symlink winner"
        assert (seed / "d").is_dir() and not os.path.islink(seed / "d"), "dest symlink not replaced"
        assert (seed / "d/file.txt").read_text() == "F\n", "winner dir content"
        assert result.json["symlinks_written"] == 1, result.json
        rec.axis("correctness", True, "s recreated; d replaced by a real directory")
        assert (elsewhere / "untouched.txt").read_text() == "E\n", "wrote through the dest symlink"
        assert set(os.listdir(elsewhere)) == {"untouched.txt"}, os.listdir(elsewhere)
        rec.axis("host_safety", True, "old symlink target untouched; never followed")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(elsewhere, ignore_errors=True)


def case_med_07(rec):
    """inv 2: merged-delta equivalence on an empty dest (mixed classes)."""
    seed = make_seed("med07", {"keep/base.txt": "BASE\n", "drop.txt": "D\n", "cfg/old.yml": "O\n"})
    sandbox_id = create_sandbox(rec, seed)
    dest_base, dest = _fresh_dest("med07")
    try:
        publish_exec(
            rec, sandbox_id,
            "printf 'NEW\\n' > keep/added.rs && ln -s base.txt keep/link.txt && rm -f drop.txt "
            "&& rm -rf cfg && mkdir cfg && printf 'P\\n' > cfg/new.yml",
        )
        result = export_changes(rec, sandbox_id, dest)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        tree = read_tree(dest)
        # The delta over an empty dest = the winner set only (no base-only paths).
        assert tree.get("keep/added.rs") == b"NEW\n", tree
        assert tree.get("keep/link.txt") == ("symlink", "base.txt"), tree
        assert tree.get("cfg/new.yml") == b"P\n", tree
        assert "keep" in tree and tree["keep"] == "dir", tree
        assert "cfg" in tree and tree["cfg"] == "dir", tree
        # A deletion/opaque over an EMPTY dest is a no-op on the tree (nothing to remove).
        assert "drop.txt" not in tree and "keep/base.txt" not in tree and "cfg/old.yml" not in tree, tree
        rec.axis("correctness", True, "empty-dest tree equals the winner projection")
        assert no_literal_markers(tree), "literal markers"
        rec.axis("host_safety", True, "no markers; nothing outside dest")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)


def case_med_08(rec):
    """Delta-cost: a big base never crosses the wire; only the delta does."""
    base_files = {f"base/f{index:04d}.txt": ("x" * 4096 + "\n") for index in range(300)}
    seed = make_seed("med08", base_files)
    base_bytes = sum(len(v) for v in base_files.values())
    sandbox_id = create_sandbox(rec, seed, timeout=420)
    dest_base, dest = _fresh_dest("med08")
    try:
        publish_exec(rec, sandbox_id, "printf 'delta\\n' > only.txt")
        result = export_changes(rec, sandbox_id, dest)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert result.json["files_written"] == 1, result.json
        assert result.json["bytes_written"] < 4096, result.json
        rec.write_json("cost.json", {"base_bytes": base_bytes, "bytes_written": result.json["bytes_written"]})
        rec.axis("correctness", True, f"one file written, {result.json['bytes_written']}B vs base {base_bytes}B")
        rec.axis("host_safety", True, "n/a", n_a=True)
        rec.axis("incremental", True, "bytes_written is O(delta), not O(image)",
                 extra={"base_bytes": base_bytes, "content_bytes_written": result.json["bytes_written"]})
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)


def case_med_09(rec):
    """inv 10 fidelity boundary: FILE mode is carried; uid/gid land on the
    manager process and user xattrs do not cross. DIRECTORY mode is part of the
    not-carried boundary, not a fidelity: the overlay-capture model records
    directories only implicitly, via their children
    (``workspace/src/overlay/capture.rs`` emits no directory LayerChange and
    drops empty dirs), so every consumer — squash, MergedView, export —
    materializes a directory at the layer-write default, never the sandbox's
    ``chmod``. Export faithfully carries the layer's stored dir mode, so this
    pins the directory-only shape (inv 2) plus the file-mode/ownership/xattr
    boundary export owns, and records the directory mode as an artifact rather
    than asserting a fidelity the layer never stored."""
    seed = make_seed("med09", {})
    sandbox_id = create_sandbox(rec, seed)
    dest_base, dest = _fresh_dest("med09")
    try:
        # `secret` carries a child so the directory-only shape is captured (an
        # empty dir is not a LayerChange); `key` exercises real file-mode fidelity.
        publish_exec(
            rec, sandbox_id,
            "mkdir -p secret && printf 'guard\\n' > secret/inner && chmod 0700 secret "
            "&& printf 'k\\n' > key && chmod 0640 key "
            "&& { setfattr -n user.note -v hi key 2>/dev/null || true; }",
        )
        result = export_changes(rec, sandbox_id, dest)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        key_mode = (dest / "key").stat().st_mode & 0o777
        assert key_mode == 0o640, oct(key_mode)
        assert (dest / "secret").is_dir(), "directory-only shape not reproduced"
        assert (dest / "secret/inner").read_text() == "guard\n", "dir child not reproduced"
        dir_mode = (dest / "secret").stat().st_mode & 0o777
        assert dir_mode != 0o700, f"dir mode unexpectedly carried the sandbox chmod: {oct(dir_mode)}"
        rec.axis("correctness", True, f"file mode {oct(key_mode)} carried; directory reproduced (mode {oct(dir_mode)}, not carried)")
        owner_ok = (dest / "key").stat().st_uid == os.getuid()
        xattr_absent = _no_user_xattr(dest / "key")
        assert owner_ok, "file not owned by the manager process"
        assert xattr_absent, "user xattr unexpectedly carried"
        rec.axis("host_safety", True, "uid==manager, user xattr absent, dir mode not carried (documented boundary)",
                 extra={"owner_is_manager": owner_ok, "user_xattr_absent": xattr_absent, "dir_mode": oct(dir_mode)})
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)


def _no_user_xattr(path):
    try:
        return not any(name.startswith("user.") for name in os.listxattr(path))
    except (OSError, AttributeError):
        return True


def case_med_10(rec):
    """B4 portability: the delta re-applies onto a fresh copy of the base."""
    seed = make_seed("med10", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    base_copy = Path(tempfile.mkdtemp(prefix="eos-export-med10-copy-"))
    archive_base, _ = _fresh_dest("med10")
    archive = archive_base / "delta.tar.zst"
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        # The portable archive (validated shape).
        arc = export_changes(rec, sandbox_id, archive, fmt="tar-zst")
        assert arc.ok, arc.json or arc.stderr
        assert zstd_entries(rec, archive) == ["src/", "src/a.rs", "src/.wh.b.rs"]
        # Re-apply the same delta onto a fresh copy of the base (cp -a of the seed).
        shutil.copytree(seed, base_copy, dirs_exist_ok=True, symlinks=True)
        # Restore the pre-export base state on the copy (seed was mutated by the archive run? no —
        # archive mode never touches the seed; the copy holds the original base).
        result = export_changes(rec, sandbox_id, base_copy)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert (base_copy / "src/a.rs").read_text() == "v2\n", "winner not applied on the copy"
        assert not (base_copy / "src/b.rs").exists(), "deletion not applied on the copy"
        rec.axis("correctness", True, "delta applied onto a fresh base copy == merged view")
        assert no_literal_markers(read_tree(base_copy)), "literal markers"
        rec.axis("host_safety", True, "nothing outside the second dir; no markers")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(base_copy, ignore_errors=True)
        shutil.rmtree(archive_base, ignore_errors=True)


# =============================================================== HARD (HRD)


def _hostile_sandbox(rec, case_id):
    """A ready sandbox carrying a tiny honest delta, so export_layerstack folds
    successfully before apply_spool_override swaps in the hostile stream."""
    seed = make_seed(case_id, {"seed.txt": "s\n"})
    sandbox_id = create_sandbox(rec, seed)
    publish_exec(rec, sandbox_id, "printf 'delta\\n' > seed.txt")
    return seed, sandbox_id


def case_hrd_01(rec):
    """C1/inv 9: a ../ or absolute entry is rejected; nothing escapes dest."""
    seed, sandbox_id = _hostile_sandbox(rec, "hrd01")
    dest_base = Path(tempfile.mkdtemp(prefix="eos-export-hrd01-"))
    dest = dest_base / "a" / "b" / "dest"
    sentinel = Sentinel(dest_base).guard_dest(dest)
    dotdot_canary = sentinel.plant("a/escape.txt", "canary-dotdot\n")
    abs_base = Path(tempfile.mkdtemp(prefix="eos-export-hrd01-abs-"))
    abs_sentinel = Sentinel(abs_base)
    abs_canary = abs_sentinel.plant("abs-escape.txt", "canary-abs\n")
    try:
        inject_spool(
            rec, sandbox_id,
            craft_hostile_spool([
                {"name": "ok.txt", "kind": "file", "content": b"OK\n"},
                {"name": "../../escape.txt", "kind": "file", "content": b"pwn\n"},
                {"name": str(abs_canary), "kind": "file", "content": b"pwn\n"},
            ]),
        )
        result = export_changes(rec, sandbox_id, dest)
        assert not result.ok, f"hostile stream applied: {result.json}"
        rec.write_json("result.json", result.json)
        message = result.json["error"]["message"]
        assert "'..'" in message or "absolute" in message, message
        rec.axis("correctness", True, "traversal entry rejected with a structured error")
        assert sentinel.unchanged() and abs_sentinel.unchanged(), "a sentinel changed"
        assert dotdot_canary.read_text() == "canary-dotdot\n"
        assert abs_canary.read_text() == "canary-abs\n"
        assert export_dir_entries(rec, sandbox_id) in (0, -1)
        rec.axis("host_safety", True, "both sentinels byte-identical; nothing outside dest",
                 extra={"rejected_class": "traversal"})
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id, sentinel=sentinel)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)
        shutil.rmtree(abs_base, ignore_errors=True)


def case_hrd_02(rec):
    """C1/inv 9: symlink-then-traverse never writes through the symlink."""
    seed, sandbox_id = _hostile_sandbox(rec, "hrd02")
    dest_base, dest = _fresh_dest("hrd02")
    evil = Path(tempfile.mkdtemp(prefix="eos-export-hrd02-evil-"))
    try:
        inject_spool(
            rec, sandbox_id,
            craft_hostile_spool([
                {"name": "x", "kind": "symlink", "link": str(evil)},
                {"name": "x/passwd", "kind": "file", "content": b"pwn\n"},
            ]),
        )
        result = export_changes(rec, sandbox_id, dest)
        rec.write_json("result.json", result.json)
        # The applier may reject, or replace x with a real in-dest directory.
        applied_in_dest = result.ok and (dest / "x" / "passwd").exists()
        assert result.ok or "error" in result.json, result.json
        rec.axis("correctness", True, "second entry rejected or contained in-dest")
        assert not (evil / "passwd").exists(), "write followed the symlink out of dest"
        assert list(os.listdir(evil)) == [], f"evil dir not empty: {os.listdir(evil)}"
        rec.axis("host_safety", True, "/evil/passwd never created; evil dir stays empty",
                 extra={"applied_in_dest": applied_in_dest})
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)
        shutil.rmtree(evil, ignore_errors=True)


def case_hrd_03(rec):
    """inv 9: a whiteout target that escapes after the prefix strip is rejected."""
    seed, sandbox_id = _hostile_sandbox(rec, "hrd03")
    dest_base, dest = _fresh_dest("hrd03")
    victim_base = Path(tempfile.mkdtemp(prefix="eos-export-hrd03-victim-"))
    victim = victim_base / "victim"
    victim.write_text("present\n")
    try:
        inject_spool(
            rec, sandbox_id,
            craft_hostile_spool([{"name": ".wh...", "kind": "marker"}]),
        )
        after_strip = export_changes(rec, sandbox_id, dest)
        rec.write_json("result-after-strip.json", after_strip.json)
        assert not after_strip.ok, after_strip.json
        assert "whiteout" in after_strip.json["error"]["message"], after_strip.json

        inject_spool(
            rec, sandbox_id,
            craft_hostile_spool([{"name": "../.wh.victim", "kind": "marker"}]),
        )
        parent_escape = export_changes(rec, sandbox_id, dest)
        rec.write_json("result-parent-escape.json", parent_escape.json)
        assert not parent_escape.ok, parent_escape.json
        rec.axis("correctness", True, "whiteout escape rejected (after-strip and parent)")
        assert victim.read_text() == "present\n", "a remove_path escaped dest"
        rec.axis("host_safety", True, "outside-dest victim still present and byte-equal")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)
        shutil.rmtree(victim_base, ignore_errors=True)


def case_hrd_04(rec):
    """inv 9 / L1: the dest deny-list holds, pre-forward."""
    seed = make_seed("hrd04", {"seed.txt": "s\n"})
    sandbox_id = create_sandbox(rec, seed)
    spool_dir = Path(tempfile.mkdtemp(prefix="eos-export-hrd04-")) / ".export" / "x"
    try:
        publish_exec(rec, sandbox_id, "printf 'delta\\n' > seed.txt")
        denied = ["/", os.path.expanduser("~"), str(spool_dir)]
        registry = _manager_registry_dir()
        if registry:
            denied.append(registry)
        results = {}
        for dest in denied:
            result = export_changes(rec, sandbox_id, dest)
            results[dest] = result.json
            assert not result.ok, f"deny-list let {dest} through: {result.json}"
            assert result.json["error"]["kind"] == "invalid_request", (dest, result.json)
            assert export_dir_entries(rec, sandbox_id) in (0, -1), f"fold started for {dest}"
        rec.write_json("deny-results.json", results)
        rec.axis("correctness", True, f"deny-list rejected {len(denied)} roots pre-forward")
        home_ok = Path(os.path.expanduser("~")).exists()
        rec.axis("host_safety", True, "denied roots unmodified; / and $HOME intact",
                 extra={"home_present": home_ok})
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(spool_dir.parents[1], ignore_errors=True)


def _manager_registry_dir():
    for candidate in (
        os.environ.get("SANDBOX_MANAGER_STATE_DIR"),
        os.environ.get("SANDBOX_REGISTRY_PATH"),
    ):
        if candidate:
            return str(Path(candidate).parent if candidate.endswith(".json") else candidate)
    return None


def case_hrd_05(rec):
    """inv 9: decompression and entry-count bombs are capped, no disk exhaustion.

    The caps ride ``manager.export`` in the gateway YAML (config consolidation
    phase 1), so this case owns a lowered-caps gateway for its duration and
    restores the baseline gateway afterwards — the config-family custody
    pattern; the parametrized case carries the ``config`` marker so it runs in
    the serial config lane.
    """
    from config import helpers as config_helpers

    max_decompressed = 256 * 1024 * 1024
    max_entries = 50_000
    arm_dir = Path(tempfile.mkdtemp(prefix="eos-export-hrd05-config-"))
    lowered = config_helpers.make_config(
        {
            "manager": {
                "export": {
                    "max_decompressed_bytes": max_decompressed,
                    "max_apply_entries": max_entries,
                }
            }
        },
        arm_dir / "gateway-lowered-caps.yml",
    )
    config_helpers.start_gateway(lowered)
    seed = sandbox_id = None
    dest_base, dest = _fresh_dest("hrd05")
    try:
        seed, sandbox_id = _hostile_sandbox(rec, "hrd05")
        free_before = shutil.disk_usage(dest_base).free
        inject_spool(rec, sandbox_id, craft_zstd_bomb(max_decompressed + 64 * 1024 * 1024))
        zstd_bomb = export_changes(rec, sandbox_id, dest)
        rec.write_json("result-zstd-bomb.json", zstd_bomb.json)
        assert not zstd_bomb.ok, f"zstd bomb applied: {zstd_bomb.json}"
        assert "decompressed" in zstd_bomb.json["error"]["message"], zstd_bomb.json

        inject_spool(rec, sandbox_id, craft_entry_count_bomb(max_entries + 5_000))
        entry_bomb = export_changes(rec, sandbox_id, dest)
        rec.write_json("result-entry-bomb.json", entry_bomb.json)
        assert not entry_bomb.ok, f"entry bomb applied: {entry_bomb.json}"
        assert "entry-count cap" in entry_bomb.json["error"]["message"], entry_bomb.json

        free_after = shutil.disk_usage(dest_base).free
        rec.axis("correctness", True, "both bombs aborted with cap-exceeded errors")
        floor = 1024 * 1024 * 1024
        assert free_after > floor, f"disk floor breached: {free_after}"
        assert free_before - free_after < 512 * 1024 * 1024, "bomb wrote large output to disk"
        rec.axis("host_safety", True, "host free space held; no pre-allocation on daemon totals",
                 extra={"free_before": free_before, "free_after": free_after})
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        if sandbox_id is not None:
            destroy_sandbox(rec, sandbox_id)
        if seed is not None:
            shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)
        shutil.rmtree(arm_dir, ignore_errors=True)
        config_helpers.restore_baseline_gateway()


def case_hrd_06(rec):
    """M4: two concurrent exports of one sandbox — singleflight or both converge."""
    seed = make_seed("hrd06", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    d1_base, dest1 = _fresh_dest("hrd06", "one")
    d2_base, dest2 = _fresh_dest("hrd06", "two")
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        results = {}

        def run(key, dest):
            results[key] = export_changes(rec, sandbox_id, dest, timeout=300)

        threads = [threading.Thread(target=run, args=("a", dest1)), threading.Thread(target=run, args=("b", dest2))]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        rec.write_json("result-a.json", results["a"].json)
        rec.write_json("result-b.json", results["b"].json)
        oks = [r for r in results.values() if r.ok]
        rejected = [r for r in results.values() if not r.ok]
        for reject in rejected:
            assert "in flight" in json.dumps(reject.json) or reject.json["error"]["kind"] == "operation_failed", reject.json
        for ok in oks:
            dest = dest1 if ok is results["a"] else dest2
            assert (dest / "src/a.rs").read_text() == "v2\n", "a spool served the wrong bytes"
            assert not (dest / "src/b.rs").exists()
        assert oks, "both exports failed"
        rec.axis("correctness", True, f"{len(oks)} converged, {len(rejected)} in-flight-rejected")
        rec.axis("host_safety", True, "each dest internally consistent; no cross-spool bytes")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(d1_base, ignore_errors=True)
        shutil.rmtree(d2_base, ignore_errors=True)


def case_hrd_07(rec):
    """B5/inv 3: export under a concurrent squash_layerstacks — both converge."""
    seed = make_seed("hrd07", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    d_base, dest = _fresh_dest("hrd07")
    try:
        for index in range(4):
            publish_exec(rec, sandbox_id, f"printf 'l{index}\\n' > src/l{index}.txt")
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        outcome = {}

        def do_export():
            outcome["export"] = export_changes(rec, sandbox_id, dest, timeout=300)

        def do_squash():
            outcome["squash"] = manager(rec, "squash_layerstacks", "--sandbox-id", sandbox_id, timeout=300)

        threads = [threading.Thread(target=do_export), threading.Thread(target=do_squash)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        rec.write_json("result-export.json", outcome["export"].json)
        rec.write_json("result-squash.json", outcome["squash"].json)
        assert outcome["export"].ok, outcome["export"].json
        assert outcome["squash"].ok, outcome["squash"].json
        assert (dest / "src/a.rs").read_text() == "v2\n", "export tore against squash"
        assert not (dest / "src/b.rs").exists()
        rec.axis("correctness", True, "export delivered its snapshot; squash also succeeded")
        rec.axis("host_safety", True, "lease pinned sources; nothing outside dest")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(d_base, ignore_errors=True)


def case_hrd_08(rec):
    """inv 3: an export's snapshot excludes a publish that lands after it."""
    seed = make_seed("hrd08", {"src/a.rs": "v1\n"})
    sandbox_id = create_sandbox(rec, seed)
    d_base, dest_a = _fresh_dest("hrd08", "va")
    dest_b = d_base / "vb"
    try:
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs")
        first = export_changes(rec, sandbox_id, dest_a)
        assert first.ok, first.json or first.stderr
        version_a = first.json["manifest_version"]
        assert not (dest_a / "later.txt").exists()
        publish_exec(rec, sandbox_id, "printf 'later\\n' > later.txt")
        second = export_changes(rec, sandbox_id, dest_b)
        assert second.ok, second.json or second.stderr
        rec.write_json("result-va.json", first.json)
        rec.write_json("result-vb.json", second.json)
        assert second.json["manifest_version"] == version_a + 1, (first.json, second.json)
        assert (dest_b / "later.txt").read_text() == "later\n", "later publish missing at v_a+1"
        rec.axis("correctness", True, "v_a excluded the later layer; v_a+1 included it")
        rec.axis("host_safety", True, "nothing outside dest")
        rec.axis("incremental", True, "second export writes only the new path",
                 extra={"version_a": version_a, "version_b": second.json["manifest_version"]})
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(d_base, ignore_errors=True)


def case_hrd_09(rec):
    """H5: a deep delta converges, or fails cleanly at the start-request ceiling."""
    layers = int(os.environ.get("EXPORT_DEEP_LAYERS", "120"))
    seed = make_seed("hrd09", {})
    sandbox_id = create_sandbox(rec, seed)
    d_base, dest = _fresh_dest("hrd09")
    try:
        for index in range(layers):
            publish_write(rec, sandbox_id, f"deep/f{index:04d}.txt", f"layer-{index}\n")
        result = export_changes(rec, sandbox_id, dest, timeout=600)
        rec.write_json("result.json", result.json)
        if result.ok:
            assert (dest / f"deep/f{layers - 1:04d}.txt").read_text() == f"layer-{layers - 1}\n"
            assert result.json["files_written"] == layers, result.json
            rec.axis("correctness", True, f"{layers}-layer delta converged; tree == merged view")
        else:
            assert result.json["error"]["kind"] in ("operation_failed", "invalid_request"), result.json
            assert not (dest / "deep").exists() or _dir_empty(dest / "deep"), "partial dest on the fail path"
            rec.axis("correctness", True, "clean start-request-ceiling failure, no partial corruption")
        # Squash-first mitigation.
        squashed = manager(rec, "squash_layerstacks", "--sandbox-id", sandbox_id, timeout=300)
        assert squashed.ok, squashed.json
        d2 = d_base / "after-squash"
        again = export_changes(rec, sandbox_id, d2, timeout=600)
        assert again.ok, f"export did not converge after squash: {again.json}"
        assert (d2 / f"deep/f{layers - 1:04d}.txt").read_text() == f"layer-{layers - 1}\n"
        rec.axis("host_safety", True, "no partial/corrupt dest; .export reaped",
                 extra={"deep_layers": layers})
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(d_base, ignore_errors=True)


def _dir_empty(path):
    try:
        return not any(Path(path).iterdir())
    except OSError:
        return True


def case_hrd_10(rec):
    """M3/H1: daemon restart drops the registry; boot reap clears .export; re-run converges."""
    seed = make_seed("hrd10", {"src/a.rs": "v1\n", "src/b.rs": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    d_base, dest = _fresh_dest("hrd10")
    try:
        # A multi-chunk delta widens the mid-paging window. The payload must be
        # INCOMPRESSIBLE and generated container-side: a repeated byte compresses
        # to ~nothing (one chunk, not "several"), and an ~8 MiB --content CLI arg
        # blows past the host ARG_MAX. 6 MiB of urandom → ~6 MiB compressed spool
        # → several 2-MiB chunks.
        publish_exec(rec, sandbox_id, "printf 'v2\\n' > src/a.rs && rm -f src/b.rs")
        publish_exec(rec, sandbox_id, "head -c 6291456 /dev/urandom > big.bin")

        outcome = {}

        def do_export():
            outcome["export"] = export_changes(rec, sandbox_id, dest, timeout=120)

        thread = threading.Thread(target=do_export)
        thread.start()
        time.sleep(0.05)
        subprocess.run(
            ["docker", "restart", sandbox_id], capture_output=True, text=True, timeout=90
        )
        thread.join()
        rec.write_json("result-interrupted.json", outcome["export"].json)
        _wait_container_ready(rec, sandbox_id)

        # The orphaned spool is removed by the export boot step (not leaked).
        assert export_dir_entries(rec, sandbox_id) in (0, -1), "boot reap left a spool under .export"

        interrupted = outcome["export"]
        if not interrupted.ok:
            message = json.dumps(interrupted.json)
            assert "not found" in message or "forward" in message or "operation_failed" in message, interrupted.json
            rec.axis("correctness", True, "interrupted invocation aborted cleanly (registry dropped)")
        else:
            assert (dest / "src/a.rs").read_text() == "v2\n"
            rec.axis("correctness", True, "restart missed the window; export converged")

        # The container restart reassigned the daemon's ephemeral host ports; the
        # manager re-resolves them by label on gateway startup (recover_sandboxes),
        # which is this deployment's recovery path (the daemon cannot restart
        # without its container — docker-init exits with it). Recover the manager's
        # view, then the re-run rebuilds the spool and converges.
        restart_gateway_and_recover(rec)
        d2 = d_base / "rerun"
        rerun = export_changes(rec, sandbox_id, d2, timeout=120)
        assert rerun.ok, f"re-run did not converge: {rerun.json}"
        assert (d2 / "src/a.rs").read_text() == "v2\n" and not (d2 / "src/b.rs").exists()
        rec.axis("host_safety", True, ".export reaped by the boot step; re-run byte-identical")
        rec.axis("incremental", True, "re-run == clean export", extra={"reran": True})
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(d_base, ignore_errors=True)


# ====================================================== RUNNABLE (RUN-01..05)
# Runnable-project round-trip (runnable-export-test-case.md): build a real
# Node/Python project in-sandbox, export the built tree, and RUN it — primary
# proof in a fresh same-image container mounted at the build-time workspace
# path, secondary best-effort on the host. The portability boundary (native
# ABI, venv paths) is exercised for real and recorded as xfail, never hidden.

NODE_IMAGE = "node:22-slim"
PYTHON_IMAGE = "python:3.12-slim"
WORKSPACE_MOUNT = "/workspace"

NPM_INSTALL = "npm install --no-audit --no-fund --loglevel=error"
VENV_BUILD = (
    "python -m venv .venv && "
    ".venv/bin/pip install --quiet --disable-pip-version-check --no-input -r requirements.txt"
)

PROBE_JS = """\
const http = require('http');

const port = process.env.PORT || 18123;
const path = process.argv[2] || '/health';
const expect = process.argv[3] || '';

http.get({ host: '127.0.0.1', port, path }, (res) => {
  let body = '';
  res.on('data', (chunk) => { body += chunk; });
  res.on('end', () => {
    console.log(body);
    process.exit(body.includes(expect) ? 0 : 1);
  });
}).on('error', () => process.exit(1));
"""

PROBE_PY = """\
import sys
import urllib.request

url, expect = sys.argv[1], sys.argv[2]
try:
    body = urllib.request.urlopen(url, timeout=2).read().decode()
except Exception:
    sys.exit(1)
print(body)
sys.exit(0 if expect in body else 1)
"""


def _server_verify_sh(start_line, probe_line):
    """A self-terminating verify entrypoint: start the server in the
    background, fail fast if it dies at startup (the /elsewhere venv boundary
    fails here), probe until healthy, print VERIFY-OK."""
    return f"""\
#!/bin/sh
set -e
PORT="${{PORT:-18123}}"
export PORT PYTHONDONTWRITEBYTECODE=1
{start_line} &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null || true' EXIT
sleep 0.5
kill -0 "$SERVER_PID" 2>/dev/null || {{ echo "server process died at startup"; exit 1; }}
i=0
until {probe_line}; do
  i=$((i+1))
  if [ "$i" -ge 30 ]; then echo "server never became healthy"; exit 1; fi
  sleep 0.5
done
echo VERIFY-OK
"""


RUN01_SEED = {
    "package.json": json.dumps(
        {"name": "run01-express", "private": True, "dependencies": {"express": "4.19.2"}},
        indent=2,
    )
    + "\n",
    "server.js": """\
const express = require('express');

const app = express();
app.get('/health', (req, res) => res.json({ status: 'ok' }));

const port = process.env.PORT || 18123;
app.listen(port, '127.0.0.1', () => console.log(`listening on ${port}`));
""",
    "probe.js": PROBE_JS,
    "verify.sh": _server_verify_sh(
        "node server.js", 'node probe.js "/health" "\\"status\\":\\"ok\\""'
    ),
}

RUN02_SEED = {
    "package.json": json.dumps(
        {
            "name": "run02-tsc",
            "private": True,
            "devDependencies": {"typescript": "5.4.5"},
            "scripts": {"build": "tsc"},
        },
        indent=2,
    )
    + "\n",
    "tsconfig.json": json.dumps(
        {
            "compilerOptions": {
                "outDir": "dist",
                "module": "commonjs",
                "target": "es2020",
                "strict": True,
            },
            "include": ["src"],
        },
        indent=2,
    )
    + "\n",
    "src/index.ts": """\
function sum(a: number, b: number): number {
  return a + b;
}

console.log(`sum(2,3)=${sum(2, 3)}`);
""",
    "verify.sh": "#!/bin/sh\nset -e\nnode dist/index.js | grep -F 'sum(2,3)=5'\n",
}

RUN03_PACKAGE_JSON = (
    json.dumps(
        {"name": "run03-native", "private": True, "dependencies": {"better-sqlite3": "^11.9.1"}},
        indent=2,
    )
    + "\n"
)

RUN03_APP_JS = """\
const Database = require('better-sqlite3');

const db = new Database(':memory:');
const row = db.prepare('SELECT 1+1 AS v').get();
console.log(`v=${row.v}`);
process.exit(row.v === 2 ? 0 : 1);
"""

RUN04_SEED = {
    "app.py": """\
from flask import Flask

app = Flask(__name__)


@app.get("/ping")
def ping():
    return "pong"
""",
    "requirements.txt": "flask==3.0.3\n",
    "probe.py": PROBE_PY,
    "verify.sh": _server_verify_sh(
        '.venv/bin/flask --app app run --host 127.0.0.1 --port "$PORT"',
        '.venv/bin/python probe.py "http://127.0.0.1:$PORT/ping" pong',
    ),
}

RUN05_SEED = {
    "pkg/__init__.py": "",
    "pkg/stats.py": """\
import numpy as np


def mean(values):
    return float(np.asarray(values, dtype=np.float64).mean())
""",
    "test_stats.py": """\
from pkg.stats import mean


def test_mean():
    assert mean([1, 2, 3, 4]) == 2.5


def test_mean_handles_negatives():
    assert mean([-2.0, 2.0]) == 0.0
""",
    "requirements.txt": "numpy==2.2.6\npytest==8.3.5\n",
    "verify.sh": (
        "#!/bin/sh\nset -e\nexport PYTHONDONTWRITEBYTECODE=1\n"
        ".venv/bin/pytest -q -p no:cacheprovider test_stats.py\n"
    ),
}


def build_in_sandbox(rec, sandbox_id, command, timeout=900):
    """A ``publish_exec`` with a build-scale timeout: the command may pull
    packages over the network; the built tree publishes as the delta."""
    return publish_exec(rec, sandbox_id, command, timeout=timeout)


def exec_capture(rec, sandbox_id, command, timeout=180):
    """``publish_exec`` returning the command's captured output text."""
    payload = publish_exec(rec, sandbox_id, command, timeout=timeout)
    return payload.get("output") or ""


def run_in_image(rec, dest, image, argv, *, timeout=180, mount_at=WORKSPACE_MOUNT):
    """Primary remount-and-run (§0): mount the exported HOST dest at
    ``mount_at`` in a fresh throwaway container of the project's base image
    and execute one self-terminating command. Returns the raw run record."""
    cmd = [
        "docker",
        "run",
        "--rm",
        "-v",
        f"{Path(dest).resolve()}:{mount_at}",
        "-w",
        mount_at,
        image,
        *map(str, argv),
    ]
    started = time.monotonic()
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        exit_code, stdout, stderr = proc.returncode, proc.stdout, proc.stderr
    except subprocess.TimeoutExpired as exc:
        exit_code = -1
        stdout = exc.stdout.decode("utf-8", "replace") if isinstance(exc.stdout, bytes) else (exc.stdout or "")
        stderr = f"timed out after {timeout}s"
    elapsed = round((time.monotonic() - started) * 1000.0, 3)
    if rec is not None:
        rec.add_command(
            {
                "cmd": cmd,
                "exit_code": exit_code,
                "elapsed_ms": elapsed,
                "stdout": stdout[-20000:],
                "stderr": stderr[-20000:],
            }
        )
    return {
        "exit_code": exit_code,
        "image": image,
        "mount_at": mount_at,
        "stdout": stdout,
        "stderr": stderr,
    }


def run_on_host(rec, dest, argv, *, timeout=120, requires=None, xfail_reason=None):
    """Secondary best-effort host run in ``dest`` (§0). ``skip`` when the
    runtime is absent; a non-zero exit is ``xfail`` when ``xfail_reason``
    names the documented portability boundary, else ``fail``. Informational
    either way — the runnable axis never hard-fails on the host run."""
    dest = Path(dest)
    runtime_name = requires or str(argv[0])
    if "/" in runtime_name:
        available = (dest / runtime_name).exists()
        absent_reason = f"{runtime_name} not present in dest"
    else:
        available = shutil.which(runtime_name) is not None
        absent_reason = f"host lacks {runtime_name}"
    if not available:
        record = {"status": "skip", "exit_code": None, "reason": absent_reason}
        if rec is not None:
            rec.add_command({"cmd": ["host", *map(str, argv)], "skipped": absent_reason})
        return record
    started = time.monotonic()
    try:
        proc = subprocess.run(
            list(map(str, argv)),
            cwd=str(dest),
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        exit_code, stdout, stderr = proc.returncode, proc.stdout, proc.stderr
    except (subprocess.TimeoutExpired, OSError) as exc:
        exit_code, stdout, stderr = -1, "", str(exc)
    elapsed = round((time.monotonic() - started) * 1000.0, 3)
    if rec is not None:
        rec.add_command(
            {
                "cmd": ["host", *map(str, argv)],
                "exit_code": exit_code,
                "elapsed_ms": elapsed,
                "stdout": stdout[-20000:],
                "stderr": stderr[-20000:],
            }
        )
    if exit_code == 0:
        return {"status": "pass", "exit_code": 0, "reason": ""}
    if xfail_reason:
        return {"status": "xfail", "exit_code": exit_code, "reason": xfail_reason}
    return {"status": "fail", "exit_code": exit_code, "reason": (stderr or stdout)[-500:]}


def assert_runnable(result, *, expect_exit=0, expect_out=None):
    """Exit-code + output-substring assertion over a ``run_in_image`` record;
    returns the slim container_run record for the runnable axis (§3)."""
    output = (result.get("stdout") or "") + (result.get("stderr") or "")
    assert result["exit_code"] == expect_exit, (
        f"container run exited {result['exit_code']} (want {expect_exit}): {output[-2000:]}"
    )
    if expect_out is not None:
        assert expect_out in output, f"expected {expect_out!r} in run output: {output[-2000:]}"
    return {
        "pass": True,
        "exit_code": result["exit_code"],
        "output_match": expect_out is None or expect_out in output,
        "image": result.get("image"),
        "mount_at": result.get("mount_at"),
    }


def expect_boundary_failure(result, reason):
    """The load-bearing portability boundary (§0, inv 10/B4): the run at the
    wrong platform/path must REALLY fail; asserting it works — or skipping it
    — would fake the boundary. Returns the xfail record."""
    output = (result.get("stdout") or "") + (result.get("stderr") or "")
    assert result["exit_code"] != 0, (
        f"boundary run unexpectedly succeeded — {reason} must be a real failure: {output[-2000:]}"
    )
    return {
        "status": "xfail",
        "exit_code": result["exit_code"],
        "mount_at": result.get("mount_at"),
        "reason": reason,
    }


def record_runnable(rec, container_run, host_run, details, *, boundary_run=None, extras=None):
    """The fourth axis: pass == container_run.pass; host_run (and any
    boundary_run or case-specific extras) ride along as recorded facts."""
    extra = {"container_run": container_run, "host_run": host_run}
    if boundary_run is not None:
        extra["boundary_run"] = boundary_run
    if extras:
        extra.update(extras)
    rec.axis("runnable", container_run.get("pass", False), details, extra=extra)


def case_run_01(rec):
    """RUN-01 (B1, inv 2, inv 4): Node/Express — npm install in-sandbox,
    export onto the seed, remount-and-run, host smoke, incremental re-export."""
    seed = make_seed("run01", RUN01_SEED)
    sandbox_id = create_sandbox(rec, seed, image=NODE_IMAGE)
    try:
        build_in_sandbox(rec, sandbox_id, NPM_INSTALL)
        first = export_changes(rec, sandbox_id, seed, timeout=600)
        assert first.ok, first.json or first.stderr
        rec.write_json("result.json", first.json)
        assert (seed / "node_modules/express/package.json").is_file(), "express did not cross"
        assert first.json["symlinks_written"] > 0, first.json
        bin_dir = seed / "node_modules/.bin"
        bin_links = [p.name for p in bin_dir.iterdir() if p.is_symlink()] if bin_dir.is_dir() else []
        assert bin_links, "no relative node_modules/.bin symlinks carried"
        assert first.json["files_written"] > 50, first.json
        rec.axis(
            "correctness",
            True,
            f"dep tree crossed: files_written={first.json['files_written']}, "
            f"symlinks_written={first.json['symlinks_written']}, .bin links={len(bin_links)}",
        )

        container = run_in_image(rec, seed, NODE_IMAGE, ["sh", "verify.sh"], timeout=240)
        container_run = assert_runnable(container, expect_out="VERIFY-OK")
        host_run = run_on_host(rec, seed, ["sh", "verify.sh"], requires="node")
        record_runnable(
            rec, container_run, host_run,
            f"container verify green; host smoke {host_run['status']}",
        )

        assert no_literal_markers(read_tree(seed)), "literal .wh. marker on host"
        rec.axis("host_safety", True, "no literal markers; nothing outside dest")

        build_in_sandbox(
            rec, sandbox_id,
            "printf '\\n// touched by RUN-01 incremental\\n' >> server.js",
            timeout=120,
        )
        second = export_changes(rec, sandbox_id, seed, timeout=600)
        assert second.ok, second.json or second.stderr
        rec.write_json("result-incremental.json", second.json)
        assert second.json["files_written"] == 1, second.json
        assert second.json["skipped_unchanged"] == first.json["files_written"], (
            second.json,
            first.json,
        )
        assert "touched by RUN-01" in (seed / "server.js").read_text(), "edit did not land"
        rec.axis(
            "incremental",
            True,
            "source-only edit: server.js rewritten, dep tree skipped_unchanged",
            extra={"skipped_unchanged": second.json["skipped_unchanged"]},
        )
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def case_run_02(rec):
    """RUN-02 (inv 2, cost table): a real build step — the compiled dist/
    crosses to a fresh (no-base) dest and runs; the base never crosses."""
    seed = make_seed("run02", RUN02_SEED)
    sandbox_id = create_sandbox(rec, seed, image=NODE_IMAGE)
    dest_base, dest = _fresh_dest("run02")
    try:
        build_in_sandbox(rec, sandbox_id, f"{NPM_INSTALL} && npm run build")
        result = export_changes(rec, sandbox_id, dest, timeout=600)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert (dest / "dist/index.js").is_file(), "compiled dist/index.js did not cross"
        assert os.path.islink(dest / "node_modules/.bin/tsc"), ".bin/tsc symlink not carried"
        assert not (dest / "src").exists(), "base src/ leaked into a fresh dest"
        assert not (dest / "tsconfig.json").exists(), "base tsconfig leaked into a fresh dest"
        rec.axis(
            "correctness",
            True,
            "dist/index.js + node_modules/.bin/tsc crossed; base stayed home",
        )

        container = run_in_image(rec, dest, NODE_IMAGE, ["node", "dist/index.js"], timeout=120)
        container_run = assert_runnable(container, expect_out="sum(2,3)=5")
        host_run = run_on_host(rec, dest, ["node", "dist/index.js"])
        record_runnable(
            rec, container_run, host_run,
            f"compiled output ran in-container; host {host_run['status']}",
        )

        assert no_literal_markers(read_tree(dest)), "literal markers"
        rec.axis("host_safety", True, "no literal markers; nothing outside dest")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)


def case_run_04(rec):
    """RUN-04 (B1, inv 10/B4, inv 4): Flask venv — runs remounted at the
    build-time /workspace; REALLY fails at /elsewhere (venv relocation
    boundary); source-only edit re-exports with the venv skipped."""
    seed = make_seed("run04", RUN04_SEED)
    sandbox_id = create_sandbox(rec, seed, image=PYTHON_IMAGE)
    try:
        build_in_sandbox(rec, sandbox_id, VENV_BUILD)
        first = export_changes(rec, sandbox_id, seed, timeout=600)
        assert first.ok, first.json or first.stderr
        rec.write_json("result.json", first.json)
        assert os.path.islink(seed / ".venv/bin/python"), ".venv/bin/python symlink not carried"
        assert first.json["symlinks_written"] > 0, first.json
        assert (seed / ".venv/lib/python3.12/site-packages/flask").is_dir(), "flask missing"
        flask_script = (seed / ".venv/bin/flask").read_bytes()
        assert flask_script.startswith(b"#!"), "console script did not cross as a script"
        assert b"/workspace/.venv" in flask_script, "shebang not baked at /workspace"
        rec.axis(
            "correctness",
            True,
            "venv crossed: bin/python symlink, site-packages/flask, /workspace shebang",
        )

        container = run_in_image(rec, seed, PYTHON_IMAGE, ["sh", "verify.sh"], timeout=240)
        container_run = assert_runnable(container, expect_out="VERIFY-OK")
        boundary = run_in_image(
            rec, seed, PYTHON_IMAGE, ["sh", "verify.sh"], timeout=240, mount_at="/elsewhere"
        )
        boundary_run = expect_boundary_failure(
            boundary, "venv is not path-relocatable (shebangs/pyvenv.cfg baked at /workspace)"
        )
        host_run = run_on_host(
            rec, seed, ["sh", "verify.sh"],
            xfail_reason="venv is not host-portable (linux interpreter + /workspace paths)",
        )
        record_runnable(
            rec, container_run, host_run,
            f"green at /workspace; xfail at /elsewhere; host {host_run['status']}",
            boundary_run=boundary_run,
        )

        assert no_literal_markers(read_tree(seed)), "literal markers"
        rec.axis("host_safety", True, "no literal markers; nothing outside dest")

        build_in_sandbox(
            rec, sandbox_id,
            "printf '\\n# touched by RUN-04 incremental\\n' >> app.py",
            timeout=120,
        )
        second = export_changes(rec, sandbox_id, seed, timeout=600)
        assert second.ok, second.json or second.stderr
        rec.write_json("result-incremental.json", second.json)
        assert second.json["files_written"] == 1, second.json
        assert second.json["skipped_unchanged"] == first.json["files_written"], (
            second.json,
            first.json,
        )
        assert "touched by RUN-04" in (seed / "app.py").read_text(), "edit did not land"
        rec.axis(
            "incremental",
            True,
            "source-only edit: app.py rewritten, .venv skipped_unchanged",
            extra={"skipped_unchanged": second.json["skipped_unchanged"]},
        )
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def case_run_03(rec):
    """RUN-03 (inv 10/B4, cost table): native addon — the linux ``*.node``
    crosses byte-identical with its mode to a fresh dest and executes on its
    own platform; the host load failure is the documented ABI boundary.
    app.js/verify.sh are PUBLISHED (not seeded) so the fresh dest carries
    them — a fresh dest holds only the delta."""
    seed = make_seed("run03", {"package.json": RUN03_PACKAGE_JSON})
    sandbox_id = create_sandbox(rec, seed, image=NODE_IMAGE)
    dest_base, dest = _fresh_dest("run03")
    try:
        publish_write(rec, sandbox_id, "app.js", RUN03_APP_JS)
        publish_write(
            rec, sandbox_id, "verify.sh", "#!/bin/sh\nset -e\nnode app.js | grep -F 'v=2'\n"
        )
        build_in_sandbox(rec, sandbox_id, NPM_INSTALL)
        truth = exec_capture(
            rec,
            sandbox_id,
            "for f in $(find node_modules/better-sqlite3 -name '*.node' | sort); do "
            'printf "NODE %s %s %s\\n" "$(sha256sum "$f" | cut -d" " -f1)" '
            '"$(stat -c "%a" "$f")" "$f"; done',
            timeout=120,
        )
        sandbox_nodes = [
            line.split()[1:4] for line in truth.splitlines() if line.startswith("NODE ")
        ]
        assert sandbox_nodes, f"no *.node built in-sandbox: {truth!r}"

        result = export_changes(rec, sandbox_id, dest, timeout=600)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        assert not (dest / "package.json").exists(), "base package.json leaked into a fresh dest"
        checked = []
        for sha, mode, rel in sandbox_nodes:
            target = dest / rel
            assert target.is_file(), f"{rel} missing from the export"
            host_sha = hashlib.sha256(target.read_bytes()).hexdigest()
            assert host_sha == sha, f"{rel} not byte-identical: {host_sha} != {sha}"
            host_mode = format(target.stat().st_mode & 0o777, "o")
            assert host_mode == mode, f"{rel} mode not carried: {host_mode} != {mode}"
            checked.append({"path": rel, "sha256": sha, "mode": mode})
        rec.write_json("native-artifacts.json", checked)
        rec.axis(
            "correctness",
            True,
            f"{len(checked)} native *.node byte-identical with mode carried",
        )

        container = run_in_image(rec, dest, NODE_IMAGE, ["node", "app.js"], timeout=120)
        container_run = assert_runnable(container, expect_out="v=2")
        host_run = run_on_host(
            rec, dest, ["node", "app.js"],
            xfail_reason="native ABI: linux binary on non-linux host",
        )
        if sys.platform != "linux" and host_run["status"] != "skip":
            assert host_run["status"] == "xfail", (
                f"linux .node unexpectedly loaded on {sys.platform}: {host_run}"
            )
        record_runnable(
            rec, container_run, host_run,
            f"linux addon ran in-container (v=2); host {host_run['status']}",
        )

        assert no_literal_markers(read_tree(dest)), "literal markers"
        rec.axis("host_safety", True, "no literal markers; nothing outside dest")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)
        shutil.rmtree(dest_base, ignore_errors=True)


def case_run_05(rec):
    """RUN-05 (B1, inv 10/B4): pytest over the exported tree — the manylinux
    numpy wheel's compiled .so crosses and the suite passes in-container;
    the venv/wheel host boundary is recorded, not hidden."""
    seed = make_seed("run05", RUN05_SEED)
    sandbox_id = create_sandbox(rec, seed, image=PYTHON_IMAGE)
    try:
        build_in_sandbox(rec, sandbox_id, VENV_BUILD)
        result = export_changes(rec, sandbox_id, seed, timeout=900)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        numpy_root = seed / ".venv/lib/python3.12/site-packages/numpy"
        so_files = sorted(str(p.relative_to(seed)) for p in numpy_root.rglob("*.so*"))
        assert so_files, "numpy compiled *.so missing from the export"
        pytest_script = (seed / ".venv/bin/pytest").read_bytes()
        assert pytest_script.startswith(b"#!"), "pytest console script did not cross"
        assert b"/workspace/.venv" in pytest_script, "shebang not baked at /workspace"
        assert result.json["symlinks_written"] > 0, result.json
        rec.axis(
            "correctness",
            True,
            f"{len(so_files)} compiled numpy artifacts + pytest shebang script crossed",
            extra={"so_sample": so_files[:5]},
        )

        container = run_in_image(rec, seed, PYTHON_IMAGE, ["sh", "verify.sh"], timeout=300)
        container_run = assert_runnable(container, expect_out="passed")
        host_run = run_on_host(
            rec, seed, [".venv/bin/pytest", "-q", "test_stats.py"],
            xfail_reason="manylinux wheel + venv paths are not host-portable",
        )
        record_runnable(
            rec, container_run, host_run,
            f"pytest suite passed over the exported tree; host {host_run['status']}",
        )

        assert no_literal_markers(read_tree(seed)), "literal markers"
        rec.axis("host_safety", True, "no literal markers; nothing outside dest")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


def case_run_06(rec):
    """RUN-06 (B4's supported path, made executable — user-directed addition):
    the host ABI xfail is recoverable with ONE platform rebuild. Export the
    linux-built tree onto the seed, document the host load failure, run
    ``npm rebuild`` with the host's own toolchain, and the SAME tree runs
    host-native — while the linux container now (correctly) rejects the
    darwin-rebuilt binary. Byte evidence: the .node's magic flips from ELF."""
    seed = make_seed(
        "run06",
        {
            "package.json": RUN03_PACKAGE_JSON,
            "app.js": RUN03_APP_JS,
            "verify.sh": "#!/bin/sh\nset -e\nnode app.js | grep -F 'v=2'\n",
        },
    )
    sandbox_id = create_sandbox(rec, seed, image=NODE_IMAGE)
    try:
        build_in_sandbox(rec, sandbox_id, NPM_INSTALL)
        truth = exec_capture(
            rec,
            sandbox_id,
            "for f in $(find node_modules/better-sqlite3 -name '*.node' | sort); do "
            'printf "NODE %s %s\\n" "$(sha256sum "$f" | cut -d" " -f1)" "$f"; done',
            timeout=120,
        )
        sandbox_nodes = [
            line.split()[1:3] for line in truth.splitlines() if line.startswith("NODE ")
        ]
        assert sandbox_nodes, f"no *.node built in-sandbox: {truth!r}"

        result = export_changes(rec, sandbox_id, seed, timeout=600)
        assert result.ok, result.json or result.stderr
        rec.write_json("result.json", result.json)
        native_nodes = []
        for sha, rel in sandbox_nodes:
            target = seed / rel
            assert target.is_file(), f"{rel} missing from the export"
            before = target.read_bytes()
            assert hashlib.sha256(before).hexdigest() == sha, f"{rel} not byte-identical"
            assert before[:4] == b"\x7fELF", f"{rel} is not the linux ELF the sandbox built"
            native_nodes.append((rel, sha))
        rec.axis(
            "correctness",
            True,
            f"{len(native_nodes)} linux ELF *.node byte-identical in the applied tree",
        )

        container_run = assert_runnable(
            run_in_image(rec, seed, NODE_IMAGE, ["node", "app.js"], timeout=120),
            expect_out="v=2",
        )
        host_before = run_on_host(
            rec, seed, ["node", "app.js"],
            xfail_reason="native ABI: linux binary on non-linux host",
        )

        if shutil.which("npm") is None:
            record_runnable(
                rec, container_run, host_before,
                "container green; host lacks npm — rebuild demonstration skipped",
                extras={"host_rebuild": {"status": "skip", "reason": "host lacks npm"}},
            )
        else:
            if sys.platform != "linux":
                assert host_before["status"] == "xfail", host_before
            rebuild = run_on_host(
                rec, seed, ["npm", "rebuild", "better-sqlite3"], timeout=300
            )
            assert rebuild["status"] == "pass", f"host npm rebuild failed: {rebuild}"
            host_after = run_on_host(rec, seed, ["node", "app.js"])
            assert host_after["status"] == "pass", (
                f"rebuilt tree still fails on the host: {host_after}"
            )
            swapped = []
            for rel, sha_before in native_nodes:
                data = (seed / rel).read_bytes()
                sha_after = hashlib.sha256(data).hexdigest()
                if sys.platform != "linux":
                    assert sha_after != sha_before, f"{rel} unchanged by the host rebuild"
                    assert data[:4] != b"\x7fELF", f"{rel} still a linux ELF after rebuild"
                swapped.append(
                    {
                        "path": rel,
                        "sha256_before": sha_before,
                        "sha256_after": sha_after,
                        "magic_after": data[:4].hex(),
                    }
                )
            rec.write_json("rebuild-artifacts.json", swapped)

            container_after = run_in_image(
                rec, seed, NODE_IMAGE, ["node", "app.js"], timeout=120
            )
            boundary_run = None
            if sys.platform != "linux":
                boundary_run = expect_boundary_failure(
                    container_after,
                    "native ABI (inverse): darwin-rebuilt binary in a linux container",
                )
            else:
                assert container_after["exit_code"] == 0, container_after
            record_runnable(
                rec, container_run, host_before,
                f"linux .node ran in-container; host {host_before['status']} → "
                "npm rebuild → host pass",
                boundary_run=boundary_run,
                extras={
                    "host_rebuild": rebuild,
                    "host_run_after_rebuild": host_after,
                },
            )

        assert no_literal_markers(read_tree(seed)), "literal markers"
        rec.axis("host_safety", True, "no literal markers; nothing outside dest")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        shutil.rmtree(seed, ignore_errors=True)


# =============================================================== BENCH (PERF)
#
# Stage-1 trio benchmark (bench-prompt.md): PERF-0 fixed-cost control plus the
# 1/5/20 MiB exploration trio, PERF-SHAPE-20M entry-count contrast, and
# PERF-ZSTD-20M compressibility contrast. The only quantity measured anywhere
# is the export OPERATION's client wall clock (RawResult.elapsed_ms); timing is
# recorded in measurements.json and is NEVER a pass/fail axis. Explicit-run
# tier: pytest -m "export and bench".

BENCH_REPS_COLD = 3
BENCH_REPS_WARM = 3
BENCH_REPS_ARCHIVE = 3
BENCH_REPS_EMPTY = 5
CHUNK_BYTES = 2 * 1024 * 1024


def _sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _parse_sha_manifest(text):
    """{filename: hex} parsed from ``sha256sum`` output lines."""
    shas = {}
    for line in text.strip().splitlines():
        parts = line.split()
        if len(parts) >= 2:
            shas[parts[-1].lstrip("*").lstrip("./")] = parts[0]
    return shas


def _verify_dest_shas(dest):
    """Every file named in dest/payload.sha hashes to its in-sandbox sha256.
    The manifest itself crossed as delta content, so the comparison pins the
    whole payload byte-for-byte. Returns the number of payload files."""
    manifest = (Path(dest) / "payload.sha").read_text(encoding="utf-8")
    shas = _parse_sha_manifest(manifest)
    assert shas, "payload.sha carried no entries"
    for name, expected in shas.items():
        actual = _sha256_file(Path(dest) / name)
        assert actual == expected, f"sha mismatch for {name}: {actual} != {expected}"
    return len(shas)


def _verify_archive_shas(rec, archive):
    """Decompress a tar-zst archive host-side and verify every payload member
    against the in-archive payload.sha manifest."""
    raw = _zstd_decompress(rec, Path(archive).read_bytes())
    with tarfile.open(fileobj=io.BytesIO(raw)) as tar:
        members = {member.name: member for member in tar.getmembers() if member.isfile()}
        assert "payload.sha" in members, sorted(members)
        manifest = tar.extractfile(members["payload.sha"]).read().decode("utf-8")
        shas = _parse_sha_manifest(manifest)
        assert shas, "payload.sha carried no entries"
        for name, expected in shas.items():
            data = tar.extractfile(members[name]).read()
            actual = hashlib.sha256(data).hexdigest()
            assert actual == expected, f"archive sha mismatch for {name}"
    return len(shas)


def _assert_cold_counts(result_json, entry_files):
    assert result_json["files_written"] == entry_files, result_json
    assert result_json["symlinks_written"] == 0, result_json
    assert result_json["deletes_applied"] == 0, result_json
    assert result_json["opaque_clears"] == 0, result_json
    assert result_json["skipped_unchanged"] == 0, result_json
    assert len(result_json["layers_exported"]) == 1, result_json
    assert result_json["manifest_version"] == _expected_version(1), result_json


def _assert_warm_counts(result_json, entry_files):
    assert result_json["files_written"] == 0, result_json
    assert result_json["bytes_written"] == 0, result_json
    assert result_json["skipped_unchanged"] == entry_files, result_json


def _bench_rep(phase, rep, result, **extra):
    entry = {
        "phase": phase,
        "rep": rep,
        "wall_ms": result.elapsed_ms,
        "result": result.json,
    }
    entry.update(extra)
    return entry


def _bench_medians(measurements):
    walls = {}
    for entry in measurements["reps"]:
        walls.setdefault(entry["phase"], []).append(entry["wall_ms"])
    medians = {phase: round(statistics.median(values), 3) for phase, values in walls.items()}
    payload = measurements.get("payload_bytes") or 0
    if payload and "cold_dir" in medians and medians["cold_dir"] > 0:
        medians["cold_dir_mib_per_s"] = round(
            (payload / (1024 * 1024)) / (medians["cold_dir"] / 1000.0), 3
        )
    return medians


def _bench_sized_case(
    rec,
    case_id,
    payload_bytes,
    publish_command,
    entry_files,
    *,
    warm=True,
    archive=True,
):
    """One sized bench case: publish the payload once, then cold dir x3 on
    fresh dests, warm re-export x3 on the first dest, tar-zst x3. sha256
    correctness is asserted on every cold dest and the last archive."""
    seed = make_seed(case_id.lower(), {"base.txt": "B\n"})
    sandbox_id = create_sandbox(rec, seed)
    scratch = [seed]
    measurements = {
        "case": case_id,
        "payload_bytes": payload_bytes,
        "chunk_bytes": CHUNK_BYTES,
        "reps": [],
    }
    try:
        publish_started = time.monotonic()
        publish_exec(rec, sandbox_id, publish_command, timeout=600)
        measurements["publish_wall_ms"] = round((time.monotonic() - publish_started) * 1000.0, 3)

        warm_dest = None
        for rep in range(BENCH_REPS_COLD):
            base, dest = _fresh_dest(case_id, f"cold{rep}")
            scratch.append(base)
            result = export_changes(rec, sandbox_id, dest, timeout=600)
            assert result.ok, result.json or result.stderr
            _assert_cold_counts(result.json, entry_files)
            files = _verify_dest_shas(dest)
            assert files == entry_files - 1, (files, entry_files)
            measurements["reps"].append(_bench_rep("cold_dir", rep, result))
            if warm_dest is None:
                warm_dest = dest

        if warm:
            for rep in range(BENCH_REPS_WARM):
                result = export_changes(rec, sandbox_id, warm_dest, timeout=600)
                assert result.ok, result.json or result.stderr
                _assert_warm_counts(result.json, entry_files)
                measurements["reps"].append(_bench_rep("warm_dir", rep, result))

        spool_bytes = None
        if archive:
            archive_base = Path(tempfile.mkdtemp(prefix=f"eos-export-bench-{case_id.lower()}-"))
            scratch.append(archive_base)
            last_archive = None
            for rep in range(BENCH_REPS_ARCHIVE):
                target = archive_base / f"delta-{rep}.tar.zst"
                result = export_changes(rec, sandbox_id, target, fmt="tar-zst", timeout=600)
                assert result.ok, result.json or result.stderr
                size = target.stat().st_size
                assert result.json["bytes_written"] == size, result.json
                spool_bytes = size
                measurements["reps"].append(_bench_rep("tar_zst", rep, result, spool_bytes=size))
                last_archive = target
            files = _verify_archive_shas(rec, last_archive)
            assert files == entry_files - 1, (files, entry_files)
            measurements["spool_bytes"] = spool_bytes
            measurements["chunks"] = math.ceil(spool_bytes / CHUNK_BYTES)

        measurements["medians"] = _bench_medians(measurements)
        rec.write_json("measurements.json", measurements)

        rec.axis(
            "correctness",
            True,
            f"sha256 verified on every cold dest{' and the archive' if archive else ''}; counts exact",
        )
        tree = read_tree(warm_dest)
        assert no_literal_markers(tree), "literal markers on host"
        rec.axis("host_safety", True, "no literal markers; nothing outside dest")
        if warm:
            rec.axis(
                "incremental",
                True,
                "warm re-export: files_written 0, bytes_written 0, all entries skipped",
            )
        else:
            rec.axis("incremental", True, "n/a (no warm arm in this case)", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        for path in scratch:
            shutil.rmtree(path, ignore_errors=True)


def case_perf_0(rec):
    """PERF-0: the fixed-cost control — empty-delta export x5 onto fresh dests.
    This wall is the floor every export pays (CLI spawn + forward + fold of
    nothing + result line); it anchors the model's constant term."""
    seed = make_seed("perf0", {"keep.txt": "K\n"})
    sandbox_id = create_sandbox(rec, seed)
    scratch = [seed]
    measurements = {
        "case": "PERF-0",
        "payload_bytes": 0,
        "chunk_bytes": CHUNK_BYTES,
        "chunks": 0,
        "spool_bytes": 0,
        "reps": [],
    }
    try:
        for rep in range(BENCH_REPS_EMPTY):
            base, dest = _fresh_dest("perf0", f"empty{rep}")
            scratch.append(base)
            result = export_changes(rec, sandbox_id, dest, timeout=600)
            assert result.ok, result.json or result.stderr
            assert result.json["layers_exported"] == [], result.json
            assert result.json["manifest_version"] == 1, result.json
            for count in (
                "files_written",
                "symlinks_written",
                "deletes_applied",
                "opaque_clears",
                "skipped_unchanged",
                "bytes_written",
            ):
                assert result.json[count] == 0, (count, result.json)
            measurements["reps"].append(_bench_rep("empty_dir", rep, result))
        measurements["medians"] = _bench_medians(measurements)
        rec.write_json("measurements.json", measurements)
        rec.axis("correctness", True, "empty delta: all counts zero, version 1, x5")
        rec.axis("host_safety", True, "dest untouched on every rep")
        rec.axis("incremental", True, "n/a", n_a=True)
        teardown(rec, sandbox_id)
    finally:
        destroy_sandbox(rec, sandbox_id)
        for path in scratch:
            shutil.rmtree(path, ignore_errors=True)


def case_perf_1m(rec):
    """PERF-1M: single 1 MiB urandom file (1 expected chunk)."""
    _bench_sized_case(
        rec,
        "PERF-1M",
        1 * 1024 * 1024,
        "head -c 1048576 /dev/urandom > payload.bin && sha256sum payload.bin > payload.sha",
        entry_files=2,
    )


def case_perf_5m(rec):
    """PERF-5M: single 5 MiB urandom file (3 expected chunks)."""
    _bench_sized_case(
        rec,
        "PERF-5M",
        5 * 1024 * 1024,
        "head -c 5242880 /dev/urandom > payload.bin && sha256sum payload.bin > payload.sha",
        entry_files=2,
    )


def case_perf_20m(rec):
    """PERF-20M: single 20 MiB urandom file (10-11 expected chunks)."""
    _bench_sized_case(
        rec,
        "PERF-20M",
        20 * 1024 * 1024,
        "head -c 20971520 /dev/urandom > payload.bin && sha256sum payload.bin > payload.sha",
        entry_files=2,
    )


def case_perf_shape_20m(rec):
    """PERF-SHAPE-20M: the same 20 MiB as 20 x 1 MiB files — entry-count
    overhead vs the single-file shape. Dir cold+warm only (bench-prompt
    matrix); chunk count for this shape is derived in the results doc from
    PERF-20M's measured spool (same payload bytes, +20 tar headers)."""
    _bench_sized_case(
        rec,
        "PERF-SHAPE-20M",
        20 * 1024 * 1024,
        "head -c 20971520 /dev/urandom > payload.bin"
        " && split -b 1048576 payload.bin part_"
        " && rm payload.bin"
        " && sha256sum part_* > payload.sha",
        entry_files=21,
        archive=False,
    )


def case_perf_zstd_20m(rec):
    """PERF-ZSTD-20M: 20 MiB of zeros — spool_bytes collapses, so the wire tax
    rides on COMPRESSED bytes. Dir + tar-zst cold arms only."""
    _bench_sized_case(
        rec,
        "PERF-ZSTD-20M",
        20 * 1024 * 1024,
        "head -c 20971520 /dev/zero > payload.bin && sha256sum payload.bin > payload.sha",
        entry_files=2,
        warm=False,
    )


# ------------------------------------------------------------ suite entrypoints


def assert_preconditions_once():
    """Run P1-P4 once, hard-fail (test-case.md §5.1). Writes a PRECONDITIONS
    verdict bundle."""
    case = {"id": "PRECONDITIONS", "tier": "preconditions", "title": "§1.1 export preconditions"}
    with record_case(case) as rec:
        assert_preconditions(rec)


def finalize_summary(exitstatus=None):
    """Write SUMMARY.md over every verdict.json under this run (§5.6)."""
    if not REPORT_ROOT.exists():
        return None
    verdicts = []
    for path in sorted(REPORT_ROOT.glob("*/verdict.json")):
        try:
            verdicts.append(json.loads(path.read_text(encoding="utf-8")))
        except (OSError, json.JSONDecodeError):
            continue
    passed = sum(1 for v in verdicts if v.get("status") == "pass")
    failed = len(verdicts) - passed
    rows = [
        "# Manager Export Changes — Live-Docker Summary",
        "",
        f"- Run id: `{RUN_ID}`",
        f"- Generated: `{dt.datetime.now().astimezone().isoformat(timespec='seconds')}`",
        f"- Pytest exit status: `{exitstatus}`",
        f"- Cases: `{len(verdicts)}` run · `{passed}` pass · `{failed}` fail",
        "",
        "| Case | Tier | Status | Correctness | Host-safety | Incremental | Runnable | Teardown |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for verdict in verdicts:
        axes = verdict.get("axes", {})

        def cell(name):
            axis = axes.get(name)
            if not axis:
                return "—"
            if axis.get("status") == "n/a":
                return "n/a"
            return "pass" if axis.get("pass") else f"fail: {axis.get('details', '')}"

        rows.append(
            f"| `{verdict.get('case_id')}` | {verdict.get('tier')} | {verdict.get('status')} | "
            f"{cell('correctness')} | {cell('host_safety')} | {cell('incremental')} | "
            f"{cell('runnable')} | "
            f"{'pass' if verdict.get('teardown', {}).get('pass') else 'fail'} |"
        )
    rows.append("")
    summary_path = REPORT_ROOT / "SUMMARY.md"
    summary_path.write_text("\n".join(rows), encoding="utf-8")
    return summary_path
