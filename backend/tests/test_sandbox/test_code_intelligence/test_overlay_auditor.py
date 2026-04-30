"""Tests for ``overlay_auditor`` NDJSON parsing and result assembly.

End-to-end overlay execution is Linux-only; these tests focus on the
deterministic orchestrator-side logic: NDJSON parsing, policy-reject
surfacing on the ``SimpleNamespace`` result, mixed gitinclude + gitignore
partial-apply metadata, and the committer adapter.
"""

from __future__ import annotations

import io
import json
import subprocess
import sys
import tarfile
from pathlib import Path
from types import SimpleNamespace

import pytest

from sandbox.code_intelligence.overlay import auditor as overlay_auditor_module
from sandbox.code_intelligence.overlay.auditor import (
    OverlayAuditor,
    parse_diff_ndjson,
)
from sandbox.code_intelligence.overlay.command_committer import OverlayCommandCommitter
from sandbox.code_intelligence.overlay.types import (
    OverlayChange,
    OverlayDiff,
    OverlayPolicyReject,
    OverlayRunError,
)
from sandbox.code_intelligence.service import (
    CodeIntelligenceService,
    dispose_all_code_intelligence,
)


@pytest.fixture(autouse=True)
def _registry():
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


# ---------------------------------------------------------------------------
# parse_diff_ndjson
# ---------------------------------------------------------------------------


def _meta_line(**overrides) -> str:
    base = {
        "snap": "deadbeef",
        "exit_code": 0,
        "upper_bytes": 0,
        "upper_files": 0,
        "gitinclude_changes": 0,
        "gitignore_changes": 0,
        "gitignore_paths": [],
        "whiteouts_gitinclude": 0,
        "whiteouts_gitignore_refused": 0,
        "dotgit_rejects": 0,
        "direct_merged_bytes": 0,
        "snapshot_timings": {},
        "run_timings": {},
        "warnings": [],
    }
    base.update(overrides)
    return json.dumps({"_meta": base}, separators=(",", ":"))


def test_parse_ndjson_empty_body_raises() -> None:
    with pytest.raises(OverlayRunError):
        parse_diff_ndjson("")


def test_parse_ndjson_returns_policy_reject() -> None:
    raw = json.dumps(
        {
            "_reject": {
                "snap": "abc",
                "reason": "overlay_rejected_dotgit_writes",
                "paths": [".git/config"],
                "snapshot_timings": {"total": 0.4},
                "run_timings": {"classify": 0.2},
            }
        }
    )
    result = parse_diff_ndjson(raw)
    assert isinstance(result, OverlayPolicyReject)
    assert result.reason == "overlay_rejected_dotgit_writes"
    assert result.paths == (".git/config",)
    assert result.snapshot_timings == {"total": 0.4}
    assert result.run_timings == {"classify": 0.2}


def test_parse_ndjson_meta_and_one_gitinclude_entry() -> None:
    raw = "\n".join(
        [
            _meta_line(
                gitinclude_changes=1,
                gitignore_changes=1,
                gitignore_paths=[".venv/cfg"],
                upper_bytes=42,
            ),
            json.dumps(
                {
                    "path": "src/app.py",
                    "kind": "modify",
                    "base_content": "before\n",
                    "base_existed": True,
                    "final_content": "after\n",
                    "strict_base": True,
                },
                separators=(",", ":"),
            ),
        ]
    )
    result = parse_diff_ndjson(raw)
    assert isinstance(result, OverlayDiff)
    assert result.snap == "deadbeef"
    assert result.upper_bytes == 42
    assert result.gitignore_paths == (".venv/cfg",)
    assert len(result.gitinclude_changes) == 1
    change = result.gitinclude_changes[0]
    assert change.path == "src/app.py"
    assert change.kind == "modify"
    assert change.base_content == "before\n"
    assert change.base_existed is True
    assert change.final_content == "after\n"


def test_parse_ndjson_delete_entry_has_none_final_content() -> None:
    raw = "\n".join(
        [
            _meta_line(gitinclude_changes=1, whiteouts_gitinclude=1),
            json.dumps(
                {
                    "path": "old.py",
                    "kind": "delete",
                    "base_content": "bye\n",
                    "base_existed": True,
                    "final_content": None,
                    "strict_base": True,
                },
                separators=(",", ":"),
            ),
        ]
    )
    result = parse_diff_ndjson(raw)
    assert isinstance(result, OverlayDiff)
    assert result.gitinclude_changes[0].final_content is None
    assert result.gitinclude_changes[0].kind == "delete"


def test_parse_ndjson_invalid_meta_raises() -> None:
    with pytest.raises(OverlayRunError):
        parse_diff_ndjson("not-json\n")


def test_parse_ndjson_invalid_entry_raises() -> None:
    raw = _meta_line(gitinclude_changes=1) + "\nnot-valid-json"
    with pytest.raises(OverlayRunError):
        parse_diff_ndjson(raw)


@pytest.mark.asyncio
async def test_read_diff_error_includes_overlay_output() -> None:
    async def _missing_diff_exec(_sandbox, _command, *, timeout=None):
        return SimpleNamespace(
            result="cat: /tmp/run/diff.ndjson: No such file or directory",
            exit_code=1,
        )

    auditor = OverlayAuditor(
        sandbox_id="overlay-missing-diff",
        workspace_root="/workspace",
        exec_process=_missing_diff_exec,
        write_coordinator=object(),
    )

    with pytest.raises(OverlayRunError) as exc_info:
        await auditor._read_diff(
            object(),
            SimpleNamespace(run_dir="/tmp/run"),
            overlay_stdout="mount setup failed",
            overlay_exit_code=255,
        )

    message = str(exc_info.value)
    assert "overlay_exit_code=255" in message
    assert "mount setup failed" in message


# ---------------------------------------------------------------------------
# OverlayCommandCommitter end-to-end against a real WriteCoordinator.
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_committer_applies_strict_base_modify(tmp_path: Path) -> None:
    target = tmp_path / "app.py"
    target.write_text("old\n", encoding="utf-8")
    svc = CodeIntelligenceService(
        sandbox_id=f"overlay-committer-{tmp_path.name}",
        workspace_root=str(tmp_path),
    )
    committer = OverlayCommandCommitter(
        svc._write_coordinator, workspace_root=str(tmp_path)
    )

    change = OverlayChange(
        path="app.py",
        kind="modify",
        base_content="old\n",
        base_existed=True,
        final_content="new\n",
    )
    result = await committer.commit([change])
    assert result.success is True
    assert result.status == "committed"
    assert target.read_text(encoding="utf-8") == "new\n"


@pytest.mark.asyncio
async def test_committer_aborts_on_strict_base_mismatch(tmp_path: Path) -> None:
    target = tmp_path / "app.py"
    target.write_text("old\n", encoding="utf-8")
    svc = CodeIntelligenceService(
        sandbox_id=f"overlay-committer-abort-{tmp_path.name}",
        workspace_root=str(tmp_path),
    )
    committer = OverlayCommandCommitter(
        svc._write_coordinator, workspace_root=str(tmp_path)
    )
    change = OverlayChange(
        path="app.py",
        kind="modify",
        base_content="old\n",
        base_existed=True,
        final_content="new\n",
    )

    # Peer write lands between SNAP and commit.
    target.write_text("peer-changed\n", encoding="utf-8")
    result = await committer.commit([change])
    assert result.success is False
    assert result.status == "aborted_version"
    # Peer write remains live.
    assert target.read_text(encoding="utf-8") == "peer-changed\n"


@pytest.mark.asyncio
async def test_committer_creates_new_gitinclude_file(tmp_path: Path) -> None:
    svc = CodeIntelligenceService(
        sandbox_id=f"overlay-committer-create-{tmp_path.name}",
        workspace_root=str(tmp_path),
    )
    committer = OverlayCommandCommitter(
        svc._write_coordinator, workspace_root=str(tmp_path)
    )
    change = OverlayChange(
        path="new.py",
        kind="create",
        base_content="",
        base_existed=False,
        final_content="print('hi')\n",
    )
    result = await committer.commit([change])
    assert result.success is True
    assert (tmp_path / "new.py").read_text(encoding="utf-8") == "print('hi')\n"


@pytest.mark.asyncio
async def test_committer_deletes_gitinclude_file(tmp_path: Path) -> None:
    target = tmp_path / "gone.py"
    target.write_text("bye\n", encoding="utf-8")
    svc = CodeIntelligenceService(
        sandbox_id=f"overlay-committer-delete-{tmp_path.name}",
        workspace_root=str(tmp_path),
    )
    committer = OverlayCommandCommitter(
        svc._write_coordinator, workspace_root=str(tmp_path)
    )
    change = OverlayChange(
        path="gone.py",
        kind="delete",
        base_content="bye\n",
        base_existed=True,
        final_content=None,
    )
    result = await committer.commit([change])
    assert result.success is True
    assert not target.exists()


# ---------------------------------------------------------------------------
# OverlayAuditor full-trip with a scripted fake exec transport.
# ---------------------------------------------------------------------------


class _ScriptedSandbox:
    """Fake sandbox: intercepts only the ``unshare -Urm`` step.

    The orchestrator issues these commands in order:
      1. ``git_snapshot`` script → runs for real on the host.
      2. Overlay runtime upload → writes the script/package for real.
      3. ``unshare -Urm ... overlay_run.py`` → intercepted. Darwin has no
         unshare/overlayfs, so we pretend to run the user command, write
         ``diff.ndjson`` into the lease's run dir, and return the scripted
         user exit code.
      4. ``cat diff.ndjson`` → runs for real against the run dir we just
         populated.
      5. ``rm -rf run_dir`` → runs for real.

    Darwin ``bash`` supports the subset of features the auditor wraps
    commands in (``pipefail``, ``-lc``), so steps 1/2/4/5 execute in the
    host shell identically to how they would inside a real sandbox.
    """

    def __init__(
        self,
        *,
        repo_root: Path,
        diff_contents: str,
        user_exit: int,
        stdout_contents: str = "",
    ) -> None:
        self._repo_root = repo_root
        self._diff_contents = diff_contents
        self._user_exit = user_exit
        self._stdout_contents = stdout_contents
        self.commands: list[str] = []
        self._run_dir: str | None = None

    async def exec(self, command: str, timeout: int | None = None):
        import asyncio
        import subprocess
        from types import SimpleNamespace

        self.commands.append(command)

        # Step 3: intercept the unshare invocation so we never try to run
        # unshare/overlayfs on darwin. ``--run-dir`` sits inside the
        # quoted inner command, so pull it out with a regex rather than
        # shell-tokenizing.
        if "unshare -Urm" in command:
            import re

            match = re.search(r"--run-dir\s+(\S+)", command)
            if match is None:
                return SimpleNamespace(result="missing run-dir", exit_code=1)
            run_dir = match.group(1)
            Path(run_dir).mkdir(parents=True, exist_ok=True)
            Path(run_dir, "diff.ndjson").write_text(
                self._diff_contents, encoding="utf-8"
            )
            Path(run_dir, "stdout.bin").write_text(
                self._stdout_contents, encoding="utf-8"
            )
            self._run_dir = run_dir
            return SimpleNamespace(result="", exit_code=self._user_exit)

        # Every other command is safe to run on the host shell.
        completed = await asyncio.to_thread(
            subprocess.run,
            command,
            shell=True,
            text=True,
            capture_output=True,
            timeout=timeout,
            check=False,
        )
        return SimpleNamespace(
            result=completed.stdout + completed.stderr,
            exit_code=completed.returncode,
        )


class _StreamingScriptedSandbox(_ScriptedSandbox):
    def __init__(
        self,
        *,
        repo_root: Path,
        diff_contents: str,
        user_exit: int,
        first_progress_seen,
    ) -> None:
        super().__init__(
            repo_root=repo_root,
            diff_contents=diff_contents,
            user_exit=user_exit,
        )
        self._first_progress_seen = first_progress_seen

    async def exec(self, command: str, timeout: int | None = None):
        if "unshare -Urm" not in command:
            return await super().exec(command, timeout=timeout)

        import asyncio
        import re

        match = re.search(r"--run-dir\s+(\S+)", command)
        if match is None:
            return SimpleNamespace(result="missing run-dir", exit_code=1)
        run_dir = match.group(1)
        Path(run_dir).mkdir(parents=True, exist_ok=True)
        stdout_path = Path(run_dir, "stdout.bin")
        stdout_path.write_text("first\n", encoding="utf-8")
        try:
            await asyncio.wait_for(self._first_progress_seen.wait(), timeout=1.0)
        except asyncio.TimeoutError:
            pass
        stdout_path.write_text("first\nsecond\n", encoding="utf-8")
        Path(run_dir, "diff.ndjson").write_text(
            self._diff_contents, encoding="utf-8"
        )
        self._run_dir = run_dir
        return SimpleNamespace(result="", exit_code=self._user_exit)


def _init_fixture_repo(path: Path) -> None:
    import subprocess

    subprocess.run(["git", "-C", str(path), "init", "-q"], check=True)
    subprocess.run(
        ["git", "-C", str(path), "config", "user.email", "t@example.invalid"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(path), "config", "user.name", "Test"], check=True
    )
    subprocess.run(
        ["git", "-C", str(path), "symbolic-ref", "HEAD", "refs/heads/main"],
        check=True,
    )


def _commit_all(path: Path) -> None:
    import subprocess

    subprocess.run(["git", "-C", str(path), "add", "-A"], check=True)
    subprocess.run(
        ["git", "-C", str(path), "commit", "-q", "-m", "seed"], check=True
    )


async def _noop_exec(sandbox, command, *, timeout=None):
    return await sandbox.exec(command, timeout=timeout)


def test_overlay_runtime_bundle_contains_executable_facade_and_runtime_package(
    tmp_path: Path,
) -> None:
    raw = overlay_auditor_module._overlay_runtime_bundle_bytes()
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as tar:
        names = set(tar.getnames())
        try:
            tar.extractall(tmp_path, filter="data")
        except TypeError:
            tar.extractall(tmp_path)

    assert "overlay_run.py" in names
    assert "overlay_runtime/__init__.py" in names
    assert "overlay_runtime/runner.py" in names
    assert "overlay_runtime/classifier.py" in names

    proc = subprocess.run(
        [sys.executable, str(tmp_path / "overlay_run.py"), "--help"],
        text=True,
        capture_output=True,
        check=False,
    )
    assert proc.returncode == 0
    assert "overlay_run.py" in proc.stdout


@pytest.mark.asyncio
async def test_auditor_commits_gitinclude_changes_via_occ(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_fixture_repo(repo)
    target = repo / "app.py"
    target.write_text("old\n", encoding="utf-8")
    _commit_all(repo)

    diff_payload = "\n".join(
        [
            _meta_line(gitinclude_changes=1, upper_files=1, upper_bytes=4),
            json.dumps(
                {
                    "path": "app.py",
                    "kind": "modify",
                    "base_content": "old\n",
                    "base_existed": True,
                    "final_content": "new\n",
                    "strict_base": True,
                }
            ),
        ]
    )
    sandbox = _ScriptedSandbox(
        repo_root=repo, diff_contents=diff_payload, user_exit=0
    )
    svc = CodeIntelligenceService(
        sandbox_id=f"overlay-auditor-commit-{tmp_path.name}",
        workspace_root=str(repo),
    )
    auditor = OverlayAuditor(
        sandbox_id=f"overlay-auditor-commit-{tmp_path.name}",
        workspace_root=str(repo),
        exec_process=_noop_exec,
        write_coordinator=svc._write_coordinator,
        max_concurrent=2,
    )

    result = await auditor.execute(sandbox, "echo hi", agent_id="alice", timeout=60)

    assert result.exit_code == 0
    assert result.git_commit_status == "committed"
    assert result.changed_paths == [str(target)]
    assert result.mixed_gitinclude_gitignore is False
    assert result.mixed_partial_apply is False
    assert target.read_text(encoding="utf-8") == "new\n"


@pytest.mark.asyncio
async def test_auditor_returns_user_command_stdout_from_overlay_run_dir(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_fixture_repo(repo)
    (repo / "app.py").write_text("old\n", encoding="utf-8")
    _commit_all(repo)

    sandbox = _ScriptedSandbox(
        repo_root=repo,
        diff_contents=_meta_line(exit_code=0),
        user_exit=0,
        stdout_contents="hello from overlay\n",
    )
    svc = CodeIntelligenceService(
        sandbox_id=f"overlay-stdout-{tmp_path.name}",
        workspace_root=str(repo),
    )
    auditor = OverlayAuditor(
        sandbox_id=f"overlay-stdout-{tmp_path.name}",
        workspace_root=str(repo),
        exec_process=_noop_exec,
        write_coordinator=svc._write_coordinator,
        max_concurrent=2,
    )

    result = await auditor.execute(sandbox, "echo hello from overlay", timeout=60)

    assert result.result == "hello from overlay\n"


@pytest.mark.asyncio
async def test_auditor_forwards_live_stdout_progress(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import asyncio

    monkeypatch.setattr(
        overlay_auditor_module,
        "_PROGRESS_POLL_INTERVAL_SECONDS",
        0.01,
    )
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_fixture_repo(repo)
    (repo / "app.py").write_text("old\n", encoding="utf-8")
    _commit_all(repo)

    first_progress_seen = asyncio.Event()
    sandbox = _StreamingScriptedSandbox(
        repo_root=repo,
        diff_contents=_meta_line(exit_code=0),
        user_exit=0,
        first_progress_seen=first_progress_seen,
    )
    svc = CodeIntelligenceService(
        sandbox_id=f"overlay-progress-{tmp_path.name}",
        workspace_root=str(repo),
    )
    auditor = OverlayAuditor(
        sandbox_id=f"overlay-progress-{tmp_path.name}",
        workspace_root=str(repo),
        exec_process=_noop_exec,
        write_coordinator=svc._write_coordinator,
        max_concurrent=2,
    )
    progress: list[str] = []

    def on_progress(line: str) -> None:
        progress.append(line)
        if "first" in line:
            first_progress_seen.set()

    result = await auditor.execute(
        sandbox,
        "echo first && sleep 1 && echo second",
        timeout=60,
        on_progress_line=on_progress,
    )

    assert result.result == "first\nsecond\n"
    assert first_progress_seen.is_set()
    assert any("first" in line for line in progress)


@pytest.mark.asyncio
async def test_auditor_reports_noop_for_gitignore_only_changes(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_fixture_repo(repo)
    (repo / ".gitignore").write_text(".venv/\n", encoding="utf-8")
    (repo / "app.py").write_text("old\n", encoding="utf-8")
    _commit_all(repo)

    (repo / ".venv").mkdir()
    (repo / ".venv" / "cfg").write_text("home=/usr\n", encoding="utf-8")
    sandbox = _ScriptedSandbox(
        repo_root=repo,
        diff_contents=_meta_line(
            exit_code=0,
            gitignore_changes=1,
            gitignore_paths=[".venv/cfg"],
            direct_merged_bytes=10,
        ),
        user_exit=0,
    )
    svc = CodeIntelligenceService(
        sandbox_id=f"overlay-gitignore-only-{tmp_path.name}",
        workspace_root=str(repo),
    )
    auditor = OverlayAuditor(
        sandbox_id=f"overlay-gitignore-only-{tmp_path.name}",
        workspace_root=str(repo),
        exec_process=_noop_exec,
        write_coordinator=svc._write_coordinator,
        max_concurrent=2,
    )

    result = await auditor.execute(sandbox, "python -m venv .venv", timeout=60)

    assert result.git_commit_status == "noop"
    assert result.changed_paths == []
    assert result.gitignore_direct_merged_paths == [str(repo / ".venv" / "cfg")]


@pytest.mark.asyncio
async def test_auditor_surfaces_mixed_partial_apply_on_occ_abort(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_fixture_repo(repo)
    target = repo / "requirements.txt"
    target.write_text("foo==0.1\n", encoding="utf-8")
    _commit_all(repo)

    # The classifier inside the ns would already have direct-merged the
    # gitignore file before OCC runs; we simulate that by writing the
    # file directly to the live workspace and letting the NDJSON declare
    # it as merged.
    (repo / ".venv").mkdir()
    (repo / ".venv" / "bar.cfg").write_text("home=/usr\n", encoding="utf-8")

    # Peer write to the gitinclude file so strict OCC aborts.
    target.write_text("peer-changed\n", encoding="utf-8")

    diff_payload = "\n".join(
        [
            _meta_line(
                gitinclude_changes=1,
                gitignore_changes=1,
                gitignore_paths=[".venv/bar.cfg"],
                upper_files=2,
                direct_merged_bytes=10,
            ),
            json.dumps(
                {
                    "path": "requirements.txt",
                    "kind": "modify",
                    # base_content matches SNAP (pre-peer-write). OCC sees
                    # live != base_content and aborts.
                    "base_content": "foo==0.1\n",
                    "base_existed": True,
                    "final_content": "foo==0.2\n",
                    "strict_base": True,
                }
            ),
        ]
    )
    sandbox = _ScriptedSandbox(
        repo_root=repo, diff_contents=diff_payload, user_exit=0
    )
    svc = CodeIntelligenceService(
        sandbox_id=f"overlay-partial-{tmp_path.name}",
        workspace_root=str(repo),
    )
    auditor = OverlayAuditor(
        sandbox_id=f"overlay-partial-{tmp_path.name}",
        workspace_root=str(repo),
        exec_process=_noop_exec,
        write_coordinator=svc._write_coordinator,
        max_concurrent=2,
    )

    # Before building SNAP, reset gitinclude file to its committed content
    # so SNAP captures "foo==0.1\n" as base, then apply the peer write
    # so OCC mismatches.
    target.write_text("foo==0.1\n", encoding="utf-8")

    # The scripted sandbox sequences SNAP before the user-cmd
    # intercept — we use that intercept to apply the peer write.
    original_exec = sandbox.exec

    async def _exec_with_peer(command, timeout=None):
        result = await original_exec(command, timeout=timeout)
        # Apply the peer write the first time we see the unshare step.
        if "unshare -Urm" in command and target.read_text(encoding="utf-8") == "foo==0.1\n":
            target.write_text("peer-changed\n", encoding="utf-8")
        return result

    sandbox.exec = _exec_with_peer  # type: ignore[assignment]

    result = await auditor.execute(sandbox, "pip install foo && echo foo >> requirements.txt", timeout=60)

    assert result.mixed_gitinclude_gitignore is True
    assert result.mixed_partial_apply is True
    assert result.git_commit_status == "aborted_version"
    assert result.changed_paths == []
    # Tracked live path appears as ambient (the user tried to change it).
    assert str(target) in result.ambient_changed_paths
    # Gitignored direct-merged path surfaces in the additive metadata.
    assert str(repo / ".venv" / "bar.cfg") in result.gitignore_direct_merged_paths
    assert any("gitinclude changes aborted" in w for w in result.warnings)


@pytest.mark.asyncio
async def test_auditor_surfaces_policy_reject(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_fixture_repo(repo)
    (repo / "README.md").write_text("seed\n", encoding="utf-8")
    _commit_all(repo)

    reject_payload = json.dumps(
        {
            "_reject": {
                "snap": "x",
                "reason": "overlay_rejected_dotgit_writes",
                "paths": [".git/config"],
            }
        }
    )
    sandbox = _ScriptedSandbox(
        repo_root=repo, diff_contents=reject_payload, user_exit=201
    )
    svc = CodeIntelligenceService(
        sandbox_id=f"overlay-reject-{tmp_path.name}",
        workspace_root=str(repo),
    )
    auditor = OverlayAuditor(
        sandbox_id=f"overlay-reject-{tmp_path.name}",
        workspace_root=str(repo),
        exec_process=_noop_exec,
        write_coordinator=svc._write_coordinator,
        max_concurrent=2,
    )

    result = await auditor.execute(sandbox, "echo .git/hack", timeout=30)

    assert result.git_commit_status == "rejected"
    assert result.git_conflict_reason
    assert "overlay_rejected_dotgit_writes" in result.git_conflict_reason
    assert result.changed_paths == []
    assert result.ambient_changed_paths == []
