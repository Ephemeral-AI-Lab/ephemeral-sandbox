"""Critic M3: materialize=False must register layer pins via LeaseRegistry."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from sandbox.execution.overlay.new_mount_api import OVL_MAX_STACK_GUARD
from sandbox.layer_stack import WriteLayerChange, LayerStack
from sandbox.layer_stack.stack import LayerStackTooDeep


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def test_prepare_materialize_false_returns_layer_paths(tmp_path: Path) -> None:
    manager = LayerStack(tmp_path / "stack")
    manifest = manager.publish_changes(
        [
            WriteLayerChange(
                path="src/app.py",
                source_path=_source(tmp_path, "app.py", b"print('hi')\n"),
            )
        ]
    )
    result = manager.prepare_workspace_snapshot("request-a", materialize=False)

    assert result.layer_paths is not None
    assert result.lowerdir is None
    assert len(result.layer_paths) == len(manifest.layers)
    for layer_path in result.layer_paths:
        assert Path(layer_path).is_dir()

    manager.release_lease(result.lease_id)


def test_prepare_materialize_false_skips_view_materialize(tmp_path: Path) -> None:
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
        result = manager.prepare_workspace_snapshot("request-a", materialize=False)
        mock_materialize.assert_not_called()

    manager.release_lease(result.lease_id)


def test_prepare_materialize_false_registers_pin_via_lease_registry(
    tmp_path: Path,
) -> None:
    """LeaseRegistry.pinned_layers() must return the manifest's layers after materialize=False."""
    manager = LayerStack(tmp_path / "stack")
    manifest = manager.publish_changes(
        [
            WriteLayerChange(
                path="a.txt",
                source_path=_source(tmp_path, "a.txt", b"a"),
            )
        ]
    )
    assert manager.pinned_layers() == ()

    result = manager.prepare_workspace_snapshot("request-pin", materialize=False)

    pinned = manager.pinned_layers()
    assert set(pinned) == set(manifest.layers), (
        f"pinned_layers() returned {pinned!r}, expected {manifest.layers!r}"
    )

    manager.release_lease(result.lease_id)
    assert manager.pinned_layers() == ()


def test_prepare_materialize_false_raises_when_depth_exceeds_guard(
    tmp_path: Path,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    # Publish OVL_MAX_STACK_GUARD + 1 layers, each with a distinct file.
    for i in range(OVL_MAX_STACK_GUARD + 1):
        manager.publish_changes(
            [
                WriteLayerChange(
                    path=f"file_{i}.txt",
                    source_path=_source(tmp_path, f"file_{i}.txt", f"content{i}".encode()),
                )
            ]
        )

    with pytest.raises(LayerStackTooDeep):
        manager.prepare_workspace_snapshot("request-deep", materialize=False)

    # Verify no lease was left dangling.
    assert manager.active_lease_count() == 0


def test_prepare_materialize_true_still_works_unchanged(tmp_path: Path) -> None:
    """Regression: existing materialize=True path must be unaffected."""
    manager = LayerStack(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="src/app.py",
                source_path=_source(tmp_path, "app.py", b"print('hi')\n"),
            )
        ]
    )
    result = manager.prepare_workspace_snapshot("request-b", materialize=True)

    assert result.lowerdir is not None
    assert result.layer_paths is None
    assert Path(result.lowerdir).is_dir()

    manager.release_lease(result.lease_id)
