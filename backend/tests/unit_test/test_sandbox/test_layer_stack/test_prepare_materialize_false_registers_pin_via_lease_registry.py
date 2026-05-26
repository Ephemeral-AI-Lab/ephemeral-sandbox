"""Layer-stack snapshots expose shared layer paths and register lease pins."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

from sandbox.layer_stack import WriteLayerChange, LayerStack


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def test_prepare_workspace_snapshot_returns_layer_paths(tmp_path: Path) -> None:
    manager = LayerStack(tmp_path / "stack")
    manifest = manager.publish_changes(
        [
            WriteLayerChange(
                path="src/app.py",
                source_path=_source(tmp_path, "app.py", b"print('hi')\n"),
            )
        ]
    )
    result = manager.prepare_workspace_snapshot("request-a")

    assert len(result.layer_paths) == len(manifest.layers)
    for layer_path in result.layer_paths:
        assert Path(layer_path).is_dir()

    manager.release_lease(result.lease_id)


def test_prepare_workspace_snapshot_skips_view_materialization(tmp_path: Path) -> None:
    manager = LayerStack(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="src/app.py",
                source_path=_source(tmp_path, "app.py", b"print('hi')\n"),
            )
        ]
    )
    with patch.object(manager._view, "materialize") as mock_materialize:
        result = manager.prepare_workspace_snapshot("request-a")
        mock_materialize.assert_not_called()

    manager.release_lease(result.lease_id)


def test_prepare_workspace_snapshot_registers_pin_via_lease_registry(
    tmp_path: Path,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    manifest = manager.publish_changes(
        [
            WriteLayerChange(
                path="a.txt",
                source_path=_source(tmp_path, "a.txt", b"a"),
            )
        ]
    )
    assert manager.leased_layers() == ()

    result = manager.prepare_workspace_snapshot("request-pin")

    leased = manager.leased_layers()
    assert set(leased) == set(manifest.layers), (
        f"leased_layers() returned {leased!r}, expected {manifest.layers!r}"
    )

    manager.release_lease(result.lease_id)
    assert manager.leased_layers() == ()


def test_prepare_workspace_snapshot_returns_all_deep_layer_paths(
    tmp_path: Path,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    layer_count = 111
    for i in range(layer_count):
        manager.publish_changes(
            [
                WriteLayerChange(
                    path=f"file_{i}.txt",
                    source_path=_source(tmp_path, f"file_{i}.txt", f"content{i}".encode()),
                )
            ]
        )

    result = manager.prepare_workspace_snapshot("request-deep")

    assert len(result.layer_paths) == layer_count
    assert manager.active_lease_count() == 1
    manager.release_lease(result.lease_id)
    assert manager.active_lease_count() == 0
