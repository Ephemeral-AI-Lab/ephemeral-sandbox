"""Snapshot lease pinning tests for layer stacks."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.layer_stack import WriteLayerChange, LayerStack


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def test_acquire_and_release_pin_exact_layer_refs(tmp_path: Path) -> None:
    manager = LayerStack(tmp_path / "stack")
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
    assert manager.leased_layers() == (top_layer,)

    assert manager.release_lease(lease_a.lease_id) is True
    assert manager.leased_layers() == (top_layer,)
    assert manager.release_lease(lease_a.lease_id) is False
    assert manager.leased_layers() == (top_layer,)

    assert manager.release_lease(lease_b.lease_id) is True
    assert manager.leased_layers() == ()


def test_releasing_old_snapshot_does_not_unpin_new_active_layer(tmp_path: Path) -> None:
    manager = LayerStack(tmp_path / "stack")
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

    assert set(manager.leased_layers()) == set(new_manifest.layers)

    manager.release_lease(old_lease.lease_id)

    assert set(manager.leased_layers()) == set(new_manifest.layers)
    assert manager.release_lease(new_lease.lease_id) is True
    assert manager.leased_layers() == ()


def test_release_lease_keeps_active_layer_storage(tmp_path: Path) -> None:
    manager = LayerStack(tmp_path / "stack")
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
    assert manager.leased_layers() == ()


def test_release_lease_removes_unreferenced_layers_outside_manager_lock(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manager = LayerStack(tmp_path / "stack")
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


def test_prepare_workspace_snapshot_returns_shared_layer_paths_per_lease(
    tmp_path: Path,
) -> None:
    manager = LayerStack(tmp_path / "stack")
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
    assert first.layer_paths == second.layer_paths
    assert first.layer_paths
    assert all(Path(path).is_dir() for path in first.layer_paths)
    assert any(
        (Path(path) / "src" / "app.py").read_text(encoding="utf-8")
        == "print('hi')\n"
        for path in first.layer_paths
        if (Path(path) / "src" / "app.py").exists()
    )

    assert manager.release_lease(first.lease_id) is True
    assert manager.release_lease(second.lease_id) is True


def test_prepare_workspace_snapshot_does_not_materialize_projection(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="src/app.py",
                source_path=_source(tmp_path, "app.py", b"print('hi')\n"),
            )
        ]
    )
    def fail_if_materialized(*_args: object, **_kwargs: object) -> None:
        pytest.fail("prepare_workspace_snapshot must expose layer_paths directly")

    monkeypatch.setattr(manager._view, "materialize", fail_if_materialized)

    snapshot = manager.prepare_workspace_snapshot("request-direct")

    assert snapshot.layer_paths
    assert manager.active_lease_count() == 1
    assert manager.release_lease(snapshot.lease_id) is True
    assert manager.active_lease_count() == 0


def test_layer_stack_manager_preserves_existing_materialized_dir_on_init(
    tmp_path: Path,
) -> None:
    stack = tmp_path / "stack"
    legacy = stack / "materialized" / "manifest-000001" / "lower"
    legacy.mkdir(parents=True)
    marker = legacy / "marker"
    marker.write_text("keep\n", encoding="utf-8")

    manager = LayerStack(stack)

    assert manager.storage_root == stack
    assert marker.read_text(encoding="utf-8") == "keep\n"
