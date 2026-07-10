"""Live e2e coverage for exec_command git publish policy: easy cases."""

from __future__ import annotations

import json
import os
import shlex
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path

import pytest

from core import cleanup
from core.cli import is_error, manager
from runtime.file.correctness.test_correctness_sessionless import (
    _assert_stack_unchanged,
)
from runtime.file.helpers import (
    assert_error,
    assert_manifest_delta,
    assert_ok,
    exec_command,
    file_blame,
    file_read,
    layer_ids,
    layerstack,
    read_command_lines,
    write_command_stdin,
)
from runtime.workspace_session.helpers import assert_exec_workspace_not_found

pytestmark = [pytest.mark.git, pytest.mark.easy]

IMAGE = os.environ.get("E2E_IMAGE", "ubuntu:24.04")
RUN_ID = os.environ.get(
    "GIT_POLICY_RUN_ID",
    datetime.now(timezone.utc).strftime("git-%Y%m%d-%H%M%S"),
)
REPORT_ROOT = Path(__file__).resolve().parent / "test-reports" / RUN_ID
GIT_DATE = "2026-07-03T00:00:00+0000"
GIT = (
    f"GIT_AUTHOR_DATE={shlex.quote(GIT_DATE)} "
    f"GIT_COMMITTER_DATE={shlex.quote(GIT_DATE)} "
    "git -c user.email=t@e -c user.name=t"
)


class GitCaseRecorder:
    def __init__(self, case_id: str):
        self.case_id = case_id
        self.dir = REPORT_ROOT / case_id
        self.axes = {
            "correctness": {"pass": False},
            "attribution": {"pass": False},
            "isolation": {"pass": True, "status": "n/a"},
        }
        self.teardown = {"pass": False}
        self.defects: list[dict[str, str]] = []
        self.sandbox_id: str | None = None
        self.started = time.monotonic()

    def __enter__(self):
        self.dir.mkdir(parents=True, exist_ok=True)
        self.add_artifact("case.json", {"case_id": self.case_id, "run_id": RUN_ID})
        return self

    def __exit__(self, exc_type, exc, _tb):
        if exc is not None:
            self.defects.append(
                {
                    "command": self.case_id,
                    "good": "case contract should hold",
                    "defect": f"{exc_type.__name__}: {exc}",
                    "fix": "diagnose from structured JSON artifacts",
                }
            )
        self.write_verdict("fail" if exc is not None else "pass")
        return False

    def add_artifact(self, name: str, payload):
        path = self.dir / name
        if isinstance(payload, str):
            path.write_text(payload, encoding="utf-8")
        else:
            path.write_text(json.dumps(payload, indent=2, sort_keys=True), encoding="utf-8")

    def axis(self, name: str, passed: bool, detail: str, **metrics):
        item = {"pass": bool(passed), "detail": detail}
        item.update(metrics)
        self.axes[name] = item

    def record_result(self, name: str, result):
        self.add_artifact(f"{name}.json", result)
        return result

    def record_cmd(self, command: str):
        with (self.dir / "cmd.log").open("a", encoding="utf-8") as handle:
            handle.write(command)
            handle.write("\n")

    def write_verdict(self, status: str):
        self.add_artifact(
            "verdict.json",
            {
                "case_id": self.case_id,
                "run_id": RUN_ID,
                "status": status if all(a.get("pass") for a in self.axes.values()) else "fail",
                "axes": self.axes,
                "teardown": self.teardown,
                "defects": self.defects,
                "wall_ms": round((time.monotonic() - self.started) * 1000.0, 3),
            },
        )


@contextmanager
def git_case(tmp_path, rec: GitCaseRecorder, files=None, dirs=()):
    root = tmp_path / f"{rec.case_id}-workspace"
    root.mkdir()
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
    rec.add_artifact("create_sandbox.json", created)
    try:
        verify_git(sandbox_id, rec)
        yield sandbox_id
    finally:
        try:
            stack = layerstack(sandbox_id)
            rec.add_artifact("layerstack.json", stack)
            rec.teardown = {
                "pass": stack.get("active_lease_count") == 0,
                "active_lease_count": stack.get("active_lease_count"),
                "layer_count": len(stack.get("layers", [])),
            }
            assert stack["active_lease_count"] == 0, stack
        finally:
            destroyed = manager("destroy_sandbox", "--sandbox-id", sandbox_id, timeout=240)
            cleanup.untrack(sandbox_id)
            rec.add_artifact("destroy_sandbox.json", destroyed)


def wait_command_terminal(sandbox_id: str, command_session_id: str, timeout_s: int = 720):
    deadline = time.monotonic() + timeout_s
    last = None
    while time.monotonic() < deadline:
        last = read_command_lines(
            sandbox_id,
            command_session_id,
            start_offset=0,
            limit=1000,
            timeout=60,
        )
        assert_ok(last)
        if last.get("status") != "running":
            return last
        time.sleep(2.0)
    raise AssertionError(f"command {command_session_id} did not finish: {last}")


def verify_git(sandbox_id: str, rec: GitCaseRecorder):
    rec.record_cmd("git --version")
    version = exec_command(sandbox_id, "git --version", yield_time_ms=30_000, timeout=120)
    rec.record_result("setup-git-version", version)
    assert_ok(version)
    assert version["status"] == "ok" and version["exit_code"] == 0, version
    assert version["output"].startswith("git version "), version


def commit_cmd(message: str) -> str:
    return f"{GIT} commit -qm {shlex.quote(message)}"


def sh(script: str) -> str:
    return "set -eu\ncd /workspace\n" + script


def exec_ok(
    sandbox_id: str,
    command: str,
    rec: GitCaseRecorder | None = None,
    *,
    name: str = "exec",
    allow_publish_reject: bool = False,
    timeout_ms: int = 300_000,
    yield_time_ms: int = 30_000,
    timeout: int = 360,
):
    if rec is not None:
        rec.record_cmd(command)
    result = exec_command(
        sandbox_id,
        command,
        yield_time_ms=yield_time_ms,
        timeout_ms=timeout_ms,
        timeout=timeout,
    )
    if rec is not None:
        rec.record_result(name, result)
    assert_ok(result)
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    if not allow_publish_reject:
        assert result.get("publish_rejected") is not True, result
    return result


def exec_any(
    sandbox_id: str,
    command: str,
    rec: GitCaseRecorder | None = None,
    *,
    name: str = "exec",
    timeout_ms: int = 300_000,
    yield_time_ms: int = 30_000,
    timeout: int = 360,
    allow_error_status: bool = False,
):
    if rec is not None:
        rec.record_cmd(command)
    result = exec_command(
        sandbox_id,
        command,
        yield_time_ms=yield_time_ms,
        timeout_ms=timeout_ms,
        timeout=timeout,
    )
    if rec is not None:
        rec.record_result(name, result)
    assert_ok(result)
    if not allow_error_status:
        assert result["status"] == "ok", result
    return result


def init_repo_cmd() -> str:
    return "git init -q -b main"


def init_repo(sandbox_id: str, rec: GitCaseRecorder, *, name: str = "init"):
    return exec_ok(sandbox_id, sh(init_repo_cmd()), rec, name=name)


def seed_repo(
    sandbox_id: str,
    rec: GitCaseRecorder,
    files: dict[str, str],
    *,
    message: str = "base",
    extra: str = "",
    name: str = "seed-repo",
):
    writes = []
    for path, content in files.items():
        writes.append(f"mkdir -p {shlex.quote(str(Path(path).parent))}")
        writes.append(f"printf %s {shlex.quote(content)} > {shlex.quote(path)}")
    script = "\n".join([init_repo_cmd(), *writes, extra, "git add -A", commit_cmd(message)])
    return exec_ok(sandbox_id, sh(script), rec, name=name)


def read_content(sandbox_id: str, path: str) -> str:
    result = assert_ok(file_read(sandbox_id, path))
    return result["content"]


def assert_content(sandbox_id: str, path: str, expected: str):
    result = assert_ok(file_read(sandbox_id, path))
    expected_content = expected.removesuffix("\n")
    assert result["content"] == expected_content, result
    assert result["bytes_read"] == len(expected_content.encode("utf-8")), result
    return result


def assert_not_found(result):
    assert is_error(result), result
    assert result["error"].get("kind") == "not_found", result
    return result["error"]


def blame_ranges(sandbox_id: str, path: str):
    result = assert_ok(file_blame(sandbox_id, path))
    ranges = result.get("ranges", [])
    assert ranges, result
    return ranges


def owners_by_line_from_ranges(ranges):
    owners = []
    for item in ranges:
        owners.extend([item["owner"]] * int(item["line_count"]))
    return owners


def assert_source_blame(sandbox_id: str, path: str, *, expected_lines: int | None = None):
    ranges = blame_ranges(sandbox_id, path)
    owners = owners_by_line_from_ranges(ranges)
    assert all(owner.startswith("workspace_session:") or owner == "original" for owner in owners), ranges
    if expected_lines is not None:
        assert len(owners) == expected_lines, ranges
    if expected_lines and expected_lines > 1:
        assert not (len(ranges) == 1 and ranges[0]["line_count"] == 1), ranges
    return owners


def assert_ignored_blame(sandbox_id: str, path: str, *, owner: str | None = None):
    ranges = blame_ranges(sandbox_id, path)
    assert len(ranges) == 1, ranges
    assert ranges[0]["start_line"] == 1, ranges
    assert ranges[0]["line_count"] == 1, ranges
    assert ranges[0]["owner"].startswith("workspace_session:"), ranges
    if owner is not None:
        assert ranges[0]["owner"] == owner, ranges
    return ranges[0]["owner"]


def assert_terminal_publish_rejection(result, reject_class: str):
    assert_ok(result)
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    assert result.get("publish_rejected") is True, result
    assert result.get("publish_reject_class") == reject_class, result
    return result


def assert_git_clean(sandbox_id: str, rec: GitCaseRecorder | None = None, *, name: str = "git-status"):
    status = exec_ok(sandbox_id, "git -C /workspace status --short", rec, name=name)
    assert status["output"] == "", status
    return status


def assert_git_operable(sandbox_id: str, rec: GitCaseRecorder | None = None, *, name: str = "git-operable"):
    fsck = exec_ok(sandbox_id, "git -C /workspace fsck", rec, name=f"{name}-fsck", timeout=480)
    status = exec_ok(sandbox_id, "git -C /workspace status --short", rec, name=f"{name}-status")
    return {"fsck": fsck, "status": status}


def start_gated_command(
    sandbox_id: str,
    command: str,
    rec: GitCaseRecorder | None = None,
    *,
    name: str,
):
    gated = sh("read go\n" + command)
    if rec is not None:
        rec.record_cmd(gated)
    result = exec_command(
        sandbox_id,
        gated,
        yield_time_ms=0,
        timeout_ms=600_000,
        timeout=120,
    )
    if rec is not None:
        rec.record_result(name, result)
    assert_ok(result)
    assert result["status"] == "running", result
    assert result["command_session_id"], result
    assert result["workspace_session_id"], result
    return result


def release_gated_command(
    sandbox_id: str,
    started,
    rec: GitCaseRecorder | None = None,
    *,
    name: str,
    allow_publish_reject: bool = False,
):
    result = write_command_stdin(
        sandbox_id,
        started["command_session_id"],
        "go\n",
        yield_time_ms=30_000,
        timeout=720,
    )
    if rec is not None:
        rec.record_result(name, result)
    assert_ok(result)
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    if not allow_publish_reject:
        assert result.get("publish_rejected") is not True, result
    return result


def assert_layer_ids_unchanged(sandbox_id: str, before):
    after = layerstack(sandbox_id)
    assert layer_ids(sandbox_id) == [layer["layer_id"] for layer in before["layers"]], after
    return after


def route_summary(source: int = 0, ignored: int = 0):
    return {"source_count": source, "ignored_count": ignored}


def axis_source(rec: GitCaseRecorder, detail: str, *, manifest_delta: int, source_count: int = 1):
    rec.axis(
        "correctness",
        True,
        detail,
        route="source",
        manifest_delta=manifest_delta,
        route_summary=route_summary(source=source_count),
    )


def axis_ignored(rec: GitCaseRecorder, detail: str, *, manifest_delta: int, ignored_count: int = 1):
    rec.axis(
        "correctness",
        True,
        detail,
        route="ignored",
        manifest_delta=manifest_delta,
        route_summary=route_summary(ignored=ignored_count),
    )


def axis_rejected(rec: GitCaseRecorder, detail: str, reject_class: str):
    rec.axis(
        "correctness",
        True,
        detail,
        route="rejected",
        manifest_delta=0,
        reject_class=reject_class,
    )


def test_EZ_01_git_init_publishes_dotgit_as_source(tmp_path):
    with GitCaseRecorder("EZ-01") as rec, git_case(tmp_path, rec) as sandbox:
        before = layerstack(sandbox)
        exec_ok(sandbox, sh(init_repo_cmd()), rec, name="git-init")
        assert_manifest_delta(sandbox, before, 1)
        assert_content(sandbox, ".git/HEAD", "ref: refs/heads/main\n")
        assert_source_blame(sandbox, ".git/config")

        axis_source(rec, ".git/HEAD and .git/config published as source", manifest_delta=1, source_count=2)
        rec.axis("attribution", True, "file_read matched .git/HEAD and .git/config blame tiled")


def test_EZ_02_git_commit_persists_into_fresh_exec(tmp_path):
    with GitCaseRecorder("EZ-02") as rec, git_case(tmp_path, rec) as sandbox:
        before = layerstack(sandbox)
        exec_ok(
            sandbox,
            sh(
                "\n".join(
                    [
                        init_repo_cmd(),
                        "printf 'hello\\n' > README.md",
                        "git add -A",
                        commit_cmd("c1"),
                    ]
                )
            ),
            rec,
            name="commit-c1",
        )
        assert_manifest_delta(sandbox, before, 1)
        log = exec_ok(
            sandbox,
            "git -C /workspace --no-pager log --format=%s --max-count=1",
            rec,
            name="fresh-log",
        )
        assert log["output"] == "c1", log
        assert_content(sandbox, "README.md", "hello\n")

        axis_source(rec, "commit object database and refs persisted", manifest_delta=1, source_count=3)
        rec.axis("attribution", True, "fresh git log and README file_read matched")


def test_EZ_03_git_tracked_working_file_has_line_attribution(tmp_path):
    with GitCaseRecorder("EZ-03") as rec, git_case(tmp_path, rec) as sandbox:
        before = layerstack(sandbox)
        exec_ok(
            sandbox,
            sh("\n".join([init_repo_cmd(), "printf 'a\\nb\\nc\\n' > src.txt", "git add src.txt"])),
            rec,
            name="git-add-src",
        )
        assert_manifest_delta(sandbox, before, 1)
        assert_content(sandbox, "src.txt", "a\nb\nc\n")
        owners = assert_source_blame(sandbox, "src.txt", expected_lines=3)
        assert len(set(owners)) == 1 and owners[0].startswith("workspace_session:"), owners

        axis_source(rec, "tracked worktree file routed as source", manifest_delta=1)
        rec.axis("attribution", True, "file_blame returned per-line source ownership")


def test_EZ_04_gitignore_is_source_and_drives_ignored_route(tmp_path):
    with GitCaseRecorder("EZ-04") as rec, git_case(tmp_path, rec) as sandbox:
        init_repo(sandbox, rec)
        before_ignore = layerstack(sandbox)
        exec_ok(sandbox, sh("printf 'out.log\\n' > .gitignore"), rec, name="write-gitignore")
        assert_manifest_delta(sandbox, before_ignore, 1)
        assert_source_blame(sandbox, ".gitignore", expected_lines=1)

        before_log = layerstack(sandbox)
        exec_ok(sandbox, sh("printf 'x\\n' > out.log"), rec, name="write-ignored-log")
        assert_manifest_delta(sandbox, before_log, 1)
        assert_content(sandbox, "out.log", "x\n")
        assert_ignored_blame(sandbox, "out.log")

        rec.axis(
            "correctness",
            True,
            ".gitignore source write then out.log ignored write both landed",
            route="mixed",
            manifest_delta=2,
            route_summary=route_summary(source=1, ignored=1),
        )
        rec.axis("attribution", True, ".gitignore tiled per-line; out.log used wholesale ignored blame")


def test_EZ_05_dotgithub_gitattributes_gitmodules_are_source(tmp_path):
    with GitCaseRecorder("EZ-05") as rec, git_case(tmp_path, rec) as sandbox:
        before = layerstack(sandbox)
        exec_ok(
            sandbox,
            sh(
                "\n".join(
                    [
                        init_repo_cmd(),
                        "mkdir -p .github/workflows",
                        "printf 'name: ci\\n' > .github/workflows/ci.yml",
                        "printf '*.txt text\\n' > .gitattributes",
                        "printf '[submodule \"x\"]\\n\\tpath = x\\n\\turl = ./x\\n' > .gitmodules",
                    ]
                )
            ),
            rec,
            name="write-git-adjacent-files",
        )
        assert_manifest_delta(sandbox, before, 1)
        assert_content(sandbox, ".github/workflows/ci.yml", "name: ci\n")
        assert_content(sandbox, ".gitattributes", "*.txt text\n")
        assert_content(sandbox, ".gitmodules", '[submodule "x"]\n\tpath = x\n\turl = ./x\n')
        assert_source_blame(sandbox, ".github/workflows/ci.yml", expected_lines=1)
        assert_source_blame(sandbox, ".gitattributes", expected_lines=1)
        assert_source_blame(sandbox, ".gitmodules", expected_lines=3)

        axis_source(rec, "git-adjacent names are ordinary source", manifest_delta=1, source_count=3)
        rec.axis("attribution", True, "all three files file_read matched and blamed as source")


def test_EZ_06_nested_repo_dotgit_is_source(tmp_path):
    with GitCaseRecorder("EZ-06") as rec, git_case(tmp_path, rec) as sandbox:
        before = layerstack(sandbox)
        exec_ok(
            sandbox,
            sh(
                "\n".join(
                    [
                        "mkdir -p pkg",
                        "cd pkg",
                        init_repo_cmd(),
                        "printf 'hi\\n' > f",
                        "git add -A",
                        commit_cmd("c1"),
                    ]
                )
            ),
            rec,
            name="nested-repo-commit",
        )
        assert_manifest_delta(sandbox, before, 1)
        log = exec_ok(sandbox, "git -C /workspace/pkg --no-pager log --format=%s --max-count=1", rec, name="nested-log")
        assert log["output"] == "c1", log
        assert_content(sandbox, "pkg/f", "hi\n")
        assert_source_blame(sandbox, "pkg/.git/HEAD")

        axis_source(rec, "pkg/.git published as ordinary source", manifest_delta=1, source_count=2)
        rec.axis("attribution", True, "nested git log survived and nested .git/HEAD blamed as source")


def test_EZ_07_git_rm_publishes_deletion(tmp_path):
    with GitCaseRecorder("EZ-07") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"doomed.txt": "delete me\n"})
        before = layerstack(sandbox)
        exec_ok(
            sandbox,
            sh("\n".join(["git rm -q doomed.txt", commit_cmd("drop")])),
            rec,
            name="git-rm-doomed",
        )
        assert_manifest_delta(sandbox, before, 1)
        assert_not_found(file_read(sandbox, "doomed.txt"))

        axis_source(rec, "git rm deletion published as source", manifest_delta=1)
        rec.axis("attribution", True, "deleted path is not_found for read")


def test_EZ_08_git_mv_is_delete_old_write_new(tmp_path):
    with GitCaseRecorder("EZ-08") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {"old.txt": "move me\n"})
        before = layerstack(sandbox)
        exec_ok(
            sandbox,
            sh("\n".join(["git mv old.txt new.txt", commit_cmd("move")])),
            rec,
            name="git-mv",
        )
        assert_manifest_delta(sandbox, before, 1)
        assert_not_found(file_read(sandbox, "old.txt"))
        assert_content(sandbox, "new.txt", "move me\n")
        assert_source_blame(sandbox, "new.txt", expected_lines=1)

        axis_source(rec, "git mv published delete-old/write-new", manifest_delta=1)
        rec.axis("attribution", True, "new path content matched and old path is not_found")


def test_EZ_09_gitignored_build_output_uses_ignored_route(tmp_path):
    with GitCaseRecorder("EZ-09") as rec, git_case(tmp_path, rec) as sandbox:
        seed_repo(sandbox, rec, {".gitignore": "target/\n"})
        before = layerstack(sandbox)
        exec_ok(
            sandbox,
            sh("mkdir -p target\nprintf 'x\\ny\\n' > target/out.bin"),
            rec,
            name="write-target-output",
        )
        assert_manifest_delta(sandbox, before, 1)
        assert_content(sandbox, "target/out.bin", "x\ny\n")
        assert_ignored_blame(sandbox, "target/out.bin")

        axis_ignored(rec, "gitignored target/out.bin routed ignored", manifest_delta=1)
        rec.axis("attribution", True, "ignored path file_read matched and blame was wholesale")


def test_EZ_10_protected_path_still_rejects(tmp_path):
    with GitCaseRecorder("EZ-10") as rec, git_case(tmp_path, rec) as sandbox:
        init_repo(sandbox, rec)
        before = layerstack(sandbox)
        result = exec_ok(
            sandbox,
            sh("mkdir -p layers\nprintf 'x\\n' > layers/evil.txt"),
            rec,
            name="protected-layers-write",
            allow_publish_reject=True,
        )
        assert_terminal_publish_rejection(result, "protected_path")
        _assert_stack_unchanged(sandbox, before)
        assert_layer_ids_unchanged(sandbox, before)
        absent = exec_ok(
            sandbox,
            "test ! -e /workspace/layers/evil.txt",
            rec,
            name="protected-discard-sentinel",
        )
        assert absent["output"] == "", absent

        axis_rejected(rec, "protected layers/ publish rejected and discarded", "protected_path")
        rec.axis("attribution", True, "n/a: rejected protected changeset committed no files")
