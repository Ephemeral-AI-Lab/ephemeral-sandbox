"""Shell staleness semantics for sandbox-runtime snapshot layer stacks."""

from __future__ import annotations

import asyncio
import threading
from dataclasses import dataclass
from pathlib import Path
from uuid import uuid4

import pytest

from sandbox.layer_stack import LayerChange, LayerStackManager
from sandbox.layer_stack.workspace.base import build_workspace_base
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.changeset.builders import build_api_write_change
from sandbox.occ.changeset.prepared import CommitOptions, PreparedChangeset
from sandbox.occ.changeset.types import FileStatus
from sandbox.command_exec.contract.result import ShellProcessResult
from sandbox.runtime.daemon.service import occ_backend, shell_runner
from sandbox.runtime.daemon.handler.request_context import _services


class _BlockingCommandRunner:
    """Pause after snapshot lease preparation so the test can advance active."""

    def __init__(self) -> None:
        self.started = threading.Event()
        self.released = threading.Event()
        self.snapshot_version: int | None = None

    def __call__(
        self,
        *,
        spec,
        request,
        run_dir,
        timings,
    ) -> ShellProcessResult:
        del request
        self.snapshot_version = spec.manifest_version
        self.started.set()
        if not self.released.wait(timeout=10):
            raise TimeoutError("blocking command runner timed out")
        upper = Path(spec.upperdir)
        upper.mkdir(parents=True, exist_ok=True)
        output = upper / "generated" / "output.json"
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes((Path(spec.lowerdir) / "config.yaml").read_bytes())
        stdout_ref = Path(run_dir) / "stdout.bin"
        stderr_ref = Path(run_dir) / "stderr.bin"
        stdout_ref.write_text("done\n", encoding="utf-8")
        stderr_ref.write_text("", encoding="utf-8")
        timings["command_exec.mount_workspace_s"] = 0.001
        timings["command_exec.run_command_s"] = 0.001
        return ShellProcessResult(
            exit_code=0,
            stdout_ref=str(stdout_ref),
            stderr_ref=str(stderr_ref),
            mounted_workspace_root=spec.workspace_root,
            mount_mode="private_namespace",
        )

    def release(self) -> None:
        self.released.set()


@dataclass(frozen=True)
class _StaleShellRun:
    manager: LayerStackManager
    result: dict[str, object]
    snapshot_version: int
    active_version_before_release: int

    @property
    def manifest_lag(self) -> int:
        return self.active_version_before_release - self.snapshot_version


@pytest.mark.parametrize("advance_count", (1, 2, 4, 5, 6, 10, 20))
async def test_shell_accepts_occ_clean_write_after_manifest_advances(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    advance_count: int,
) -> None:
    run = await _run_occ_clean_stale_shell(
        tmp_path,
        monkeypatch=monkeypatch,
        advance_count=advance_count,
    )

    assert run.snapshot_version == 1
    assert run.manifest_lag == advance_count
    assert run.result["success"] is True
    assert run.result["status"] == "ok"
    assert run.result["changed_paths"] == ["generated/output.json"]
    assert run.result["stdout"] == "done\n"
    assert run.manager.read_text("generated/output.json") == ("value: v1\n", True)


async def test_daemon_gitignore_uses_layer_stack_snapshot(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / f"stack-{uuid4().hex}")
    _publish(manager, tmp_path, ".gitignore", b"dist/\n")
    occ_backend._backend_cache_clear()
    services = _services(str(manager.storage_root))

    # Reach through the OCC client to its underlying OccService for the assertion.
    result = await services.occ_client._service.apply_changeset(
        [build_api_write_change(path="dist/app.js", final_content="first\n")],
        options=CommitOptions(caller_id="test", description="ignored output"),
    )

    assert not isinstance(result, PreparedChangeset)
    assert result.files[0].status is FileStatus.ACCEPTED
    assert result.files[0].timings["occ.direct.read_current_s"] >= 0.0
    assert manager.read_text("dist/app.js") == ("first\n", True)


async def _run_occ_clean_stale_shell(
    tmp_path: Path,
    *,
    monkeypatch: pytest.MonkeyPatch,
    advance_count: int,
) -> _StaleShellRun:
    workspace = tmp_path / f"workspace-{uuid4().hex}"
    workspace.mkdir()
    (workspace / "config.yaml").write_text("value: v1\n", encoding="utf-8")
    stack = tmp_path / f"stack-{uuid4().hex}"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    manager = LayerStackManager(stack)
    runner = _BlockingCommandRunner()
    monkeypatch.setattr(
        shell_runner,
        "run_workspace_replaced_command",
        runner,
    )

    task = asyncio.create_task(
        shell_runner.execute_shell_api(
            {
                "layer_stack_root": str(manager.storage_root),
                "command": (
                    "mkdir -p generated; "
                    "cp config.yaml generated/output.json; "
                    "printf 'done\\n'"
                ),
                "cwd": ".",
                "timeout_seconds": 10,
                "actor_id": "agent-staleness",
                "description": "staleness clean write",
            }
        )
    )
    try:
        started = await asyncio.to_thread(runner.started.wait, 5)
        if not started:
            raise AssertionError("blocked runner did not start")
        if runner.snapshot_version is None:
            raise AssertionError("blocked runner did not record snapshot version")
        for index in range(advance_count):
            _publish(
                manager,
                tmp_path,
                f"unrelated/{advance_count}/{index}.txt",
                f"unrelated-{index}\n".encode(),
            )
        active_version = manager.read_active_manifest().version
        runner.release()
        result = await asyncio.wait_for(task, timeout=10)
        return _StaleShellRun(
            manager=manager,
            result=result,
            snapshot_version=runner.snapshot_version,
            active_version_before_release=active_version,
        )
    finally:
        runner.release()
        if not task.done():
            task.cancel()


def _source(tmp_path: Path, name: str, content: bytes) -> Path:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return path


def _publish(
    manager: LayerStackManager,
    tmp_path: Path,
    rel: str,
    content: bytes,
) -> None:
    source = _source(tmp_path, rel.replace("/", "-"), content)
    manager.publish_changes(
        [
            LayerChange(
                path=rel,
                kind="write",
                content_hash=ContentHasher().hash_bytes(content),
                source_path=str(source),
            )
        ]
    )
