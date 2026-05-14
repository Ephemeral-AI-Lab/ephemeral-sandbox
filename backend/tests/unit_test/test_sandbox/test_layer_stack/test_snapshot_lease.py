"""Snapshot lease pinning tests for layer stacks."""

from __future__ import annotations

import shutil
from pathlib import Path

import pytest

from sandbox.layer_stack import WriteLayerChange, LayerStackManager


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def test_acquire_and_release_pin_exact_layer_refs(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manifest = manager.publish_changes(
        [
            WriteLayerChange(
                path="a.txt",
                source_path=_source(tmp_path, "a.txt", b"a"),
            )
        ]
    )
    top_layer = manifest.layers[0]

    lease_a = manager.acquire_snapshot_lease("request-a")
    lease_b = manager.acquire_snapshot_lease("request-b")

    assert lease_a.manifest == manifest
    assert lease_b.manifest == manifest
    assert manager.pinned_layers() == (top_layer,)

    assert manager.release_lease(lease_a.lease_id) is True
    assert manager.pinned_layers() == (top_layer,)
    assert manager.release_lease(lease_a.lease_id) is False
    assert manager.pinned_layers() == (top_layer,)

    assert manager.release_lease(lease_b.lease_id) is True
    assert manager.pinned_layers() == ()


def test_releasing_old_snapshot_does_not_unpin_new_active_layer(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="a.txt",
                source_path=_source(tmp_path, "old.txt", b"old"),
            )
        ]
    )
    old_lease = manager.acquire_snapshot_lease("old-request")
    new_manifest = manager.publish_changes(
        [
            WriteLayerChange(
                path="b.txt",
                source_path=_source(tmp_path, "new.txt", b"new"),
            )
        ]
    )
    new_lease = manager.acquire_snapshot_lease("new-request")

    assert set(manager.pinned_layers()) == set(new_manifest.layers)

    manager.release_lease(old_lease.lease_id)

    assert set(manager.pinned_layers()) == set(new_manifest.layers)
    assert manager.release_lease(new_lease.lease_id) is True
    assert manager.pinned_layers() == ()


def test_release_lease_keeps_active_layer_storage(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manifest = manager.publish_changes(
        [
            WriteLayerChange(
                path="active.txt",
                source_path=_source(tmp_path, "active.txt", b"active\n"),
            )
        ]
    )
    active_layer = manifest.layers[0]
    lease = manager.acquire_snapshot_lease("request-a")

    assert manager.release_lease(lease.lease_id) is True

    assert (manager.storage_root / active_layer.path).is_dir()
    assert manager.read_text("active.txt") == ("active\n", True)
    assert manager.pinned_layers() == ()


def test_release_lease_removes_unreferenced_layers_outside_manager_lock(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="old.txt",
                source_path=_source(tmp_path, "old.txt", b"old\n"),
            )
        ]
    )
    lease = manager.acquire_snapshot_lease("old")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="new.txt",
                source_path=_source(tmp_path, "new.txt", b"new\n"),
            )
        ]
    )
    observed_unlocked = False
    original_remove_layers = manager._remove_layers

    def assert_unlocked(layers):
        nonlocal observed_unlocked
        observed_unlocked = not manager._lock._is_owned()
        return original_remove_layers(layers)

    monkeypatch.setattr(manager, "_remove_layers", assert_unlocked)

    assert manager.release_lease(lease.lease_id) is True
    assert observed_unlocked is True


def test_prepare_workspace_snapshot_returns_distinct_transient_lowerdirs_per_lease(
    tmp_path: Path,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="src/app.py",
                source_path=_source(tmp_path, "app.py", b"print('hi')\n"),
            )
        ]
    )

    first = manager.prepare_workspace_snapshot("request-a")
    second = manager.prepare_workspace_snapshot("request-b")

    assert first.manifest_version == second.manifest_version
    assert first.root_hash == second.root_hash
    assert first.lowerdir != second.lowerdir
    assert Path(first.lowerdir).is_dir()
    assert Path(second.lowerdir).is_dir()
    assert (Path(first.lowerdir) / "src" / "app.py").read_text(
        encoding="utf-8",
    ) == "print('hi')\n"

    # release_lease drops bookkeeping; the transient lowerdir is the caller's
    # responsibility. Simulate the caller cleanup that shell_runner does.
    assert manager.release_lease(first.lease_id) is True
    shutil.rmtree(Path(first.lowerdir).parent, ignore_errors=True)
    assert Path(first.lowerdir).exists() is False

    assert manager.release_lease(second.lease_id) is True
    shutil.rmtree(Path(second.lowerdir).parent, ignore_errors=True)
    assert Path(second.lowerdir).exists() is False


def test_prepare_workspace_snapshot_failure_releases_lease_and_drops_partial_lowerdir(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="src/app.py",
                source_path=_source(tmp_path, "app.py", b"print('hi')\n"),
            )
        ]
    )
    partial_lowerdirs: list[Path] = []

    def fail_materialize(
        destination: str | Path,
        manifest: object,
        *,
        share_inodes: bool = False,
    ) -> None:
        del manifest, share_inodes
        lowerdir = Path(destination)
        lowerdir.mkdir(parents=True)
        (lowerdir / "partial.txt").write_text("partial\n", encoding="utf-8")
        partial_lowerdirs.append(lowerdir)
        raise RuntimeError("materialize failed")

    monkeypatch.setattr(manager._view, "materialize", fail_materialize)

    with pytest.raises(RuntimeError, match="materialize failed"):
        manager.prepare_workspace_snapshot("request-fails")

    assert manager.active_lease_count() == 0
    assert manager.pinned_layers() == ()
    assert partial_lowerdirs
    assert partial_lowerdirs[0].parent.exists() is False


def test_layer_stack_manager_preserves_existing_materialized_dir_on_init(
    tmp_path: Path,
) -> None:
    stack = tmp_path / "stack"
    legacy = stack / "materialized" / "manifest-000001" / "lower"
    legacy.mkdir(parents=True)
    marker = legacy / "marker"
    marker.write_text("keep\n", encoding="utf-8")

    manager = LayerStackManager(stack)

    assert manager.storage_root == stack
    assert marker.read_text(encoding="utf-8") == "keep\n"
