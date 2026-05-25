"""Shell staleness semantics for sandbox-runtime snapshot layer stacks."""

from __future__ import annotations

import asyncio
import threading
from dataclasses import dataclass
from pathlib import Path
from uuid import uuid4

import pytest

from sandbox.layer_stack import WriteLayerChange, LayerStack
from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox.occ.content_hashing import ContentHasher
from sandbox.occ.changeset import build_api_write_change
from sandbox.occ.changeset import CommitOptions
from sandbox.occ.changeset import FileStatus
from sandbox._shared.models import Intent, ToolCallRequest
from sandbox.daemon import occ_runtime_services
from sandbox.ephemeral_workspace.pipeline import EphemeralPipeline
import sandbox.ephemeral_workspace.pipeline as pipeline_mod


class _BlockingCommandRunner:
    """Pause after snapshot lease preparation so the test can advance active."""

    def __init__(self, snapshot_version: int) -> None:
        self.started = threading.Event()
        self.released = threading.Event()
        self.snapshot_version = snapshot_version

    async def __call__(
        self,
        handle,
        req,
    ) -> dict[str, object]:
        del req
        self.started.set()
        released = await asyncio.to_thread(self.released.wait, 10)
        if not released:
            raise TimeoutError("blocking command runner timed out")
        upper = Path(handle.upperdir)
        upper.mkdir(parents=True, exist_ok=True)
        output = upper / "generated" / "output.json"
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes((Path(handle.layer_paths[0]) / "config.yaml").read_bytes())
        return {
            "success": True,
            "status": "ok",
            "exit_code": 0,
            "stdout": "done\n",
            "stderr": "",
            "timings": {},
        }

    def release(self) -> None:
        self.released.set()


@dataclass(frozen=True)
class _StaleShellRun:
    manager: LayerStack
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
    manager = LayerStack(tmp_path / f"stack-{uuid4().hex}")
    _publish(manager, tmp_path, ".gitignore", b"dist/\n")
    occ_runtime_services.clear_occ_runtime_services()
    services = occ_runtime_services.get_occ_runtime_services(str(manager.storage_root))

    # Reach through the OCC client to its underlying OccService for the assertion.
    result = await services.occ_client._service.apply_changeset(
        [build_api_write_change(path="dist/app.js", final_content="first\n")],
        options=CommitOptions(),
    )

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
    manager = LayerStack(stack)
    runner = _BlockingCommandRunner(manager.read_active_manifest().version)
    monkeypatch.setattr(
        pipeline_mod,
        "run_in_namespace",
        runner,
    )
    services = occ_runtime_services.get_occ_runtime_services(str(manager.storage_root))
    monkeypatch.setattr(
        pipeline_mod,
        "overlay_writable_root",
        lambda: tmp_path / "overlay-writable-root",
    )
    monkeypatch.setattr(
        pipeline_mod.overlay_lifecycle,
        "overlay_writable_root",
        lambda: tmp_path / "overlay-writable-root",
    )
    pipeline = EphemeralPipeline(
        occ_client=services.occ_client,
        workspace_ref=str(manager.storage_root),
        layer_stack=services.layer_stack,
        workspace_root=workspace.as_posix(),
    )

    task = asyncio.create_task(
        pipeline.run_tool_call(
            ToolCallRequest(
                invocation_id="staleness-shell",
                agent_id="agent-staleness",
                verb="shell",
                intent=Intent.WRITE_ALLOWED,
                args={
                    "command": (
                        "mkdir -p generated; cp config.yaml generated/output.json; printf 'done\\n'"
                    ),
                    "cwd": ".",
                    "timeout_seconds": 10,
                },
            )
        )
    )
    try:
        started = await asyncio.to_thread(runner.started.wait, 5)
        if not started:
            raise AssertionError("blocked runner did not start")
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
    manager: LayerStack,
    tmp_path: Path,
    rel: str,
    content: bytes,
) -> None:
    source = _source(tmp_path, rel.replace("/", "-"), content)
    manager.publish_changes(
        [
            WriteLayerChange(
                path=rel,
                content_hash=ContentHasher().hash_bytes(content),
                source_path=str(source),
            )
        ]
    )
