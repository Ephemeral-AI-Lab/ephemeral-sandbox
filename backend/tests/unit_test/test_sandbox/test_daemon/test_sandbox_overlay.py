"""Unit tests for daemon-owned SandboxOverlay lifecycle."""

from __future__ import annotations

import os
from pathlib import Path
from types import SimpleNamespace

import pytest

from sandbox.daemon.service import overlay_manager
from sandbox.daemon.service import sandbox_overlay as overlay_mod
from sandbox.daemon.service.sandbox_overlay import SandboxOverlay
from sandbox.layer_stack.manifest import LayerRef, Manifest
from sandbox.occ.changeset import ChangesetResult
from sandbox.layer_stack.workspace_binding import (
    WorkspaceBinding,
    write_workspace_binding_atomic,
)


class _LayerStack:
    def __init__(self, storage_root: Path, manifest: Manifest) -> None:
        self.storage_root = storage_root
        self.manifest = manifest
        self.released: list[str] = []

    def read_active_manifest(self) -> Manifest:
        return self.manifest

    def prepare_workspace_snapshot(
        self,
        *,
        request_id: str,
        lowerdir_root: str | Path | None = None,
        materialize: bool = True,
    ) -> object:
        del request_id, lowerdir_root, materialize
        return SimpleNamespace(
            lease_id=f"lease-{self.manifest.version}",
            manifest=self.manifest,
            manifest_version=self.manifest.version,
            root_hash=f"root-{self.manifest.version}",
            lowerdir=None,
            layer_paths=tuple(
                (self.storage_root / "layers" / layer.path).as_posix()
                for layer in self.manifest.layers
            ),
        )

    def release_lease(self, *, lease_id: str) -> bool:
        self.released.append(lease_id)
        return True


class _OccClient:
    async def run_maintenance_after_publish(self, *args, **kwargs) -> dict[str, float]:
        del args, kwargs
        return {}


class _PublishingOccClient:
    def __init__(self, layer_stack: _LayerStack) -> None:
        self.layer_stack = layer_stack
        self.apply_run_maintenance: list[bool] = []
        self.maintenance_release_order: list[list[str]] = []

    async def apply_changeset(self, *args, **kwargs) -> ChangesetResult:
        del args
        self.apply_run_maintenance.append(bool(kwargs.get("run_maintenance")))
        self.layer_stack.manifest = Manifest(
            version=2,
            layers=(
                LayerRef(layer_id="L2", path="L2"),
                LayerRef(layer_id="L1", path="L1"),
            ),
        )
        return ChangesetResult(files=(), timings={}, published_manifest_version=2)

    async def run_maintenance_after_publish(self, *args, **kwargs) -> dict[str, float]:
        del args, kwargs
        self.maintenance_release_order.append(list(self.layer_stack.released))
        self.layer_stack.manifest = Manifest(
            version=3,
            layers=(LayerRef(layer_id="B3", path="B3"),),
        )
        return {
            "layer_stack.auto_squash.total_s": 0.01,
            "layer_stack.auto_squash.depth_before": 2.0,
            "layer_stack.auto_squash.depth_after": 1.0,
        }


@pytest.mark.asyncio
async def test_start_mounts_active_manifest_and_stop_unmounts(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manifest = Manifest(
        version=1,
        layers=(LayerRef(layer_id="L1", path="L1"),),
    )
    layer_stack = _LayerStack(tmp_path / "stack", manifest)
    (layer_stack.storage_root / "layers" / "L1").mkdir(parents=True)
    workspace = tmp_path / "testbed"
    workspace.mkdir()
    mounts: list[tuple[Path, tuple[Path, ...], Path, Path]] = []
    unmounts: list[Path] = []

    def fake_mount_overlay(**kwargs) -> None:
        mounts.append(
            (
                kwargs["workspace_root"],
                kwargs["layer_paths"],
                kwargs["upperdir"],
                kwargs["workdir"],
            )
        )

    monkeypatch.setattr(overlay_mod, "mount_overlay", fake_mount_overlay)
    monkeypatch.setattr(overlay_mod, "umount", lambda path: unmounts.append(path))

    overlay = SandboxOverlay(
        occ_client=_OccClient(),  # type: ignore[arg-type]
        workspace_ref=layer_stack.storage_root.as_posix(),
        layer_stack=layer_stack,
        workspace_root=workspace.as_posix(),
    )

    await overlay.start()
    await overlay.stop()

    assert mounts[0][0] == workspace
    assert len(mounts[0][1]) == 1
    assert all(path.as_posix().startswith("/proc/self/fd/") for path in mounts[0][1])
    assert mounts[0][2].as_posix().startswith("/proc/self/fd/")
    assert mounts[0][3].as_posix().startswith("/proc/self/fd/")
    assert unmounts == [workspace]
    assert layer_stack.released == ["lease-1"]


def test_overlay_runtime_uses_command_exec_scratch_root(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    scratch_root = tmp_path / "scratch"
    monkeypatch.setenv("EPHEMERALOS_COMMAND_EXEC_SCRATCH_ROOT", scratch_root.as_posix())
    manifest = Manifest(version=1, layers=(LayerRef(layer_id="L1", path="L1"),))
    layer_stack = _LayerStack(tmp_path / "stack", manifest)

    overlay = SandboxOverlay(
        occ_client=_OccClient(),  # type: ignore[arg-type]
        workspace_ref=layer_stack.storage_root.as_posix(),
        layer_stack=layer_stack,
        workspace_root=(tmp_path / "testbed").as_posix(),
    )

    assert overlay.scratch_root == scratch_root
    assert os.fspath(overlay.runtime_dir).startswith(os.fspath(scratch_root))
    assert os.fspath(overlay.upperdir).startswith(os.fspath(scratch_root))


def test_operation_overlay_uses_shared_snapshot_layers_and_private_upperdir(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    scratch_root = tmp_path / "scratch"
    monkeypatch.setenv("EPHEMERALOS_COMMAND_EXEC_SCRATCH_ROOT", scratch_root.as_posix())
    manifest = Manifest(version=1, layers=(LayerRef(layer_id="L1", path="L1"),))
    layer_stack = _LayerStack(tmp_path / "stack", manifest)
    (layer_stack.storage_root / "layers" / "L1").mkdir(parents=True)

    overlay = SandboxOverlay(
        occ_client=_OccClient(),  # type: ignore[arg-type]
        workspace_ref=layer_stack.storage_root.as_posix(),
        layer_stack=layer_stack,
        workspace_root="/testbed",
    )

    first = overlay.acquire_operation_overlay(
        request_id="lsp-hover",
        workspace_root="/testbed",
        materialize=False,
    )
    second = overlay.acquire_operation_overlay(
        request_id="lsp-rename",
        workspace_root="/testbed",
        materialize=False,
    )

    assert first.layer_paths == second.layer_paths
    assert first.upperdir != second.upperdir
    assert first.workdir != second.workdir
    assert Path(first.upperdir).is_relative_to(scratch_root)
    assert Path(second.upperdir).is_relative_to(scratch_root)

    first.release()
    second.release()

    assert layer_stack.released == ["lease-1", "lease-1"]
    assert not Path(first.run_dir).exists()
    assert not Path(second.run_dir).exists()


@pytest.mark.asyncio
async def test_stop_unmounts_stale_workspace_mount_even_when_manager_is_cold(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manifest = Manifest(version=1, layers=(LayerRef(layer_id="L1", path="L1"),))
    layer_stack = _LayerStack(tmp_path / "stack", manifest)
    workspace = tmp_path / "testbed"
    workspace.mkdir()
    unmounts: list[Path] = []
    monkeypatch.setattr(overlay_mod, "umount", lambda path: unmounts.append(path))

    overlay = SandboxOverlay(
        occ_client=_OccClient(),  # type: ignore[arg-type]
        workspace_ref=layer_stack.storage_root.as_posix(),
        layer_stack=layer_stack,
        workspace_root=workspace.as_posix(),
    )

    await overlay.stop()

    assert unmounts == [workspace]


@pytest.mark.asyncio
async def test_manager_stop_unmounts_requested_and_bound_workspace_roots(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    stack_root = tmp_path / "runtime" / "layer-stack"
    stack_root.mkdir(parents=True)
    requested_workspace = tmp_path / "testbed"
    bound_workspace = tmp_path / "ephemeral-os"
    requested_workspace.mkdir()
    bound_workspace.mkdir()
    write_workspace_binding_atomic(
        WorkspaceBinding(
            workspace_root=bound_workspace.as_posix(),
            layer_stack_root=stack_root.as_posix(),
            active_manifest_version=1,
            active_root_hash="root",
            base_manifest_version=1,
            base_root_hash="root",
        )
    )
    stopped: list[str] = []
    unmounts: list[Path] = []

    class _CachedOverlay:
        async def stop(self) -> None:
            stopped.append("cached")

    overlay_manager.clear_overlay_manager_for_tests()
    key = f"{stack_root.resolve(strict=False).as_posix()}\0{bound_workspace.as_posix()}"
    overlay_manager._OVERLAYS[key] = _CachedOverlay()  # type: ignore[assignment]  # noqa: SLF001
    monkeypatch.setattr(overlay_manager, "umount", lambda path: unmounts.append(path))

    result = await overlay_manager.stop_sandbox_overlay(
        stack_root,
        workspace_root=requested_workspace,
    )

    assert result["success"] is True
    assert stopped == ["cached"]
    assert unmounts == [requested_workspace, bound_workspace]
    assert not overlay_manager._OVERLAYS  # noqa: SLF001


@pytest.mark.asyncio
async def test_ensure_current_remounts_and_emits_foreign_publish(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    first = Manifest(version=1, layers=(LayerRef(layer_id="L1", path="L1"),))
    second = Manifest(version=2, layers=(LayerRef(layer_id="L2", path="L2"),))
    layer_stack = _LayerStack(tmp_path / "stack", first)
    (layer_stack.storage_root / "layers" / "L1").mkdir(parents=True)
    (layer_stack.storage_root / "layers" / "L2").mkdir(parents=True)
    workspace = tmp_path / "testbed"
    workspace.mkdir()
    mounts: list[tuple[Path, ...]] = []
    unmounts: list[Path] = []

    monkeypatch.setattr(
        overlay_mod,
        "mount_overlay",
        lambda **kwargs: mounts.append(kwargs["layer_paths"]),
    )
    monkeypatch.setattr(overlay_mod, "umount", lambda path: unmounts.append(path))

    overlay = SandboxOverlay(
        occ_client=_OccClient(),  # type: ignore[arg-type]
        workspace_ref=layer_stack.storage_root.as_posix(),
        layer_stack=layer_stack,
        workspace_root=workspace.as_posix(),
    )
    queue = overlay.event_bus.subscribe("test")

    await overlay.start()
    layer_stack.manifest = second
    await overlay.ensure_current(reason="lsp:hover:enter")

    assert len(mounts[-1]) == 1
    assert mounts[-1][0].as_posix().startswith("/proc/self/fd/")
    assert unmounts == [workspace]
    assert layer_stack.released == ["lease-1"]
    event = queue.get_nowait()
    assert event.reason == "foreign_publish"
    assert event.from_version == 1
    assert event.to_version == 2
    await overlay.stop()


@pytest.mark.asyncio
async def test_publish_releases_mounted_lease_before_maintenance_and_remounts_latest(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    initial = Manifest(version=1, layers=(LayerRef(layer_id="L1", path="L1"),))
    layer_stack = _LayerStack(tmp_path / "stack", initial)
    for name in ("L1", "L2", "B3"):
        (layer_stack.storage_root / "layers" / name).mkdir(parents=True)
    workspace = tmp_path / "testbed"
    workspace.mkdir()
    mounts: list[tuple[Path, ...]] = []
    unmounts: list[Path] = []

    monkeypatch.setattr(
        overlay_mod,
        "mount_overlay",
        lambda **kwargs: mounts.append(kwargs["layer_paths"]),
    )
    monkeypatch.setattr(overlay_mod, "umount", lambda path: unmounts.append(path))

    occ_client = _PublishingOccClient(layer_stack)
    overlay = SandboxOverlay(
        occ_client=occ_client,  # type: ignore[arg-type]
        workspace_ref=layer_stack.storage_root.as_posix(),
        layer_stack=layer_stack,
        workspace_root=workspace.as_posix(),
    )

    await overlay.start()
    (overlay.upperdir / "changed.py").write_text("print('changed')\n", encoding="utf-8")
    publish = await overlay.publish_pending_changes(
        snapshot=overlay.current_manifest(),
        reason="publish",
        run_maintenance=True,
    )

    assert occ_client.apply_run_maintenance == [False]
    assert occ_client.maintenance_release_order == [["lease-1"]]
    assert layer_stack.released == ["lease-1"]
    assert unmounts == [workspace]
    assert len(mounts) == 2
    assert len(mounts[-1]) == 1
    assert publish.timings["layer_stack.auto_squash.depth_after"] == 1.0
    assert overlay.current_manifest().version == 3
    await overlay.stop()
