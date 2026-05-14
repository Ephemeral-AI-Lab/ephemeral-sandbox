"""Command-exec capture submission tests."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import pytest

from sandbox.execution.contract import ShellProcessResult
from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox.layer_stack.workspace_binding import WorkspaceBinding, write_workspace_binding_atomic
from sandbox.occ.changeset.types import ChangesetResult, FileResult, FileStatus
from sandbox.daemon.service import shell_runner
from sandbox.daemon.service.layer_stack_client import LayerStackClient


@dataclass(frozen=True)
class _Lease:
    lease_id: str
    manifest_version: int
    manifest: Manifest
    lowerdir: str
    timings: dict[str, float]


class _LayerStackClient:
    def __init__(self, lowerdir: Path) -> None:
        self.lease = _Lease(
            lease_id="lease-1",
            manifest_version=1,
            manifest=Manifest(version=1, layers=()),
            lowerdir=str(lowerdir),
            timings={
                "layer_stack.materialize_s": 0.003,
                "layer_stack.prepare_workspace_snapshot.total_s": 0.004,
            },
        )
        self.released: list[str] = []

    def prepare_workspace_snapshot(
        self,
        *,
        workspace_ref: str,
        request_id: str,
    ) -> _Lease:
        del workspace_ref, request_id
        return self.lease

    def release_lease(self, *, workspace_ref: str, lease_id: str) -> bool:
        del workspace_ref
        self.released.append(lease_id)
        return True


class _Client:
    def __init__(self, layer_stack: _LayerStackClient) -> None:
        self.layer_stack = layer_stack
        self.paths: list[str] = []
        self.snapshot: object | None = None
        self.atomic: bool | None = None

    async def apply_changeset(
        self,
        typed_changes,
        *,
        snapshot: object | None = None,
        options: object | None = None,
        workspace_ref: str | None = None,
    ) -> ChangesetResult:
        del workspace_ref
        assert self.layer_stack.released == []
        self.paths = [change.path for change in typed_changes]
        self.snapshot = snapshot
        self.atomic = getattr(options, "atomic", None)
        return ChangesetResult(
            files=(FileResult(path="generated/output.txt", status=FileStatus.COMMITTED),),
            timings={
                "occ.prepare.total_s": 0.003,
                "occ.prepare.route_and_base_hash_s": 0.002,
                "occ.commit.total_s": 0.004,
                "occ.commit.publish_layer_s": 0.001,
                "occ.apply.commit_queue_wait_s": 0.0,
                "occ.apply.commit_worker_s": 0.004,
                "occ.apply.commit_s": 0.004,
                "occ.apply.total_s": 0.01,
            },
            published_manifest_version=2,
        )


class _Gitignore:
    cache_hits = 0
    cache_misses = 0


async def test_shell_capture_goes_through_occ_client_before_lease_release(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    stack = tmp_path / "stack"
    stack.mkdir()
    write_workspace_binding_atomic(
        WorkspaceBinding(
            workspace_root=workspace.as_posix(),
            layer_stack_root=stack.as_posix(),
            active_manifest_version=1,
            active_root_hash="a" * 64,
            base_manifest_version=1,
            base_root_hash="a" * 64,
        )
    )
    lower_parent = stack / "runtime" / "transient-lowerdirs" / "req-1"
    lower = lower_parent / "lower"
    lower.mkdir(parents=True)
    layer_stack = _LayerStackClient(lower)
    occ = _Client(layer_stack)

    def fake_run_workspace_replaced_command(*, spec, request, run_dir, timings):
        del request
        upper = Path(spec.upperdir)
        upper.mkdir(parents=True)
        output = upper / "generated" / "output.txt"
        output.parent.mkdir(parents=True)
        output.write_text("value\n", encoding="utf-8")
        stdout_ref = Path(run_dir) / "stdout.bin"
        stderr_ref = Path(run_dir) / "stderr.bin"
        stdout_ref.write_text("done\n", encoding="utf-8")
        stderr_ref.write_text("", encoding="utf-8")
        timings["command_exec.mount_workspace_s"] = 0.001
        timings["command_exec.run_command_s"] = 0.002
        return ShellProcessResult(
            exit_code=0,
            stdout_ref=str(stdout_ref),
            stderr_ref=str(stderr_ref),
            mounted_workspace_root=str(workspace),
            mount_mode="private_namespace",
        )

    monkeypatch.setattr(
        shell_runner,
        "run_workspace_replaced_command",
        fake_run_workspace_replaced_command,
    )

    result = await shell_runner._execute_shell(
        {
            "layer_stack_root": stack.as_posix(),
            "command": "true",
            "cwd": ".",
            "actor_id": "agent-1",
            "description": "unit shell",
        },
        layer_stack=layer_stack,
        occ_client=occ,
        gitignore=_Gitignore(),
        storage_root=stack,
    )

    assert occ.paths == ["generated/output.txt"]
    assert occ.snapshot is layer_stack.lease.manifest
    # Phase 04.5 follow-up: single-path captures opt out of cross-path
    # atomicity so CommitQueue._disjoint_batches can coalesce them.
    assert occ.atomic is False
    assert layer_stack.released == ["lease-1"]
    assert result.stdout == "done\n"
    assert result.workspace_capture.snapshot_version == 1
    # Unconditional cleanup deletes the lowerdir parent on release.
    assert lower_parent.exists() is False
    _assert_phase08_shell_timings(result.timings)


async def test_shell_uses_transient_lowerdir_and_removes_it(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "input.txt").write_text("base\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    layer_stack = LayerStackClient(stack)
    captured_lowerdirs: list[Path] = []

    def fake_run_workspace_replaced_command(*, spec, request, run_dir, timings):
        del request
        lowerdir = Path(spec.lowerdir)
        captured_lowerdirs.append(lowerdir)
        assert lowerdir.is_dir()
        assert (lowerdir / "input.txt").read_text(encoding="utf-8") == "base\n"
        Path(spec.upperdir).mkdir(parents=True, exist_ok=True)
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
            mounted_workspace_root=str(workspace),
            mount_mode="private_namespace",
        )

    monkeypatch.setattr(
        shell_runner,
        "run_workspace_replaced_command",
        fake_run_workspace_replaced_command,
    )

    result = await shell_runner._execute_shell(
        {
            "layer_stack_root": stack.as_posix(),
            "command": "true",
            "cwd": ".",
        },
        layer_stack=layer_stack,
        occ_client=_Client(_LayerStackClient(tmp_path / "unused-lower")),
        gitignore=_Gitignore(),
        storage_root=stack,
    )

    assert result.exit_code == 0
    assert captured_lowerdirs
    assert captured_lowerdirs[0].exists() is False


def test_drop_transient_lowerdir_refuses_matching_path_outside_storage_root(
    tmp_path: Path,
) -> None:
    storage_root = tmp_path / "stack"
    outside_root = tmp_path / "outside"
    lower = outside_root / "runtime" / "transient-lowerdirs" / "req-1" / "lower"
    lower.mkdir(parents=True)

    shell_runner._drop_transient_lowerdir(
        _Lease(
            lease_id="lease-1",
            manifest_version=1,
            manifest=Manifest(version=1, layers=()),
            lowerdir=lower.as_posix(),
            timings={},
        ),
        storage_root=storage_root,
    )

    assert lower.exists()


def _assert_phase08_shell_timings(timings: dict[str, float]) -> None:
    required = {
        "layer_stack.materialize_s",
        "layer_stack.prepare_workspace_snapshot.total_s",
        "command_exec.prepare_snapshot_s",
        "command_exec.mount_workspace_s",
        "command_exec.run_command_s",
        "command_exec.capture_upperdir_s",
        "command_exec.occ_apply_s",
        "command_exec.release_snapshot_s",
        "command_exec.total_s",
        "api.shell.overlay_s",
        "api.shell.occ_apply_s",
        "api.shell.total_s",
        "occ.prepare.total_s",
        "occ.prepare.route_and_base_hash_s",
        "occ.commit.total_s",
        "occ.commit.publish_layer_s",
        "occ.apply.commit_queue_wait_s",
        "occ.apply.commit_worker_s",
        "occ.apply.commit_s",
        "occ.apply.total_s",
        "gitignore.cache_hits_total",
        "gitignore.cache_misses_total",
    }
    assert required <= timings.keys()
    forbidden = {
        "cache_hit",
        "cache_policy",
        "lowerdir_cache_hit",
        "lowerdir_cache_hits",
        "lowerdir_cache_misses",
        "materialized_byte_count",
    }
    assert timings.keys().isdisjoint(forbidden)
