"""Unit tests for MaterializeLayout and LayerPathsLayout."""

from __future__ import annotations

import pytest

from sandbox.execution.overlay.layout import (
    LayerPathsLayout,
    MaterializeLayout,
    OverlayLayout,
)
from sandbox.execution.overlay.new_mount_api import (
    OVL_MAX_STACK_GUARD,
    LayerStackTooDeep,
)


# ---------------------------------------------------------------------------
# MaterializeLayout — existing invariants must be unchanged
# ---------------------------------------------------------------------------


def test_materialize_layout_valid() -> None:
    spec = MaterializeLayout(
        workspace_root="/workspace",
        base_repo="/scratch/base",
        writes="/scratch/upper",
        kernel_scratch="/scratch/work",
        scratch_root="/scratch",
    )
    assert spec.workspace_root == "/workspace"


def test_materialize_layout_rejects_relative_workspace_root() -> None:
    with pytest.raises(ValueError, match="workspace_root must be absolute"):
        MaterializeLayout(
            workspace_root="relative/path",
            base_repo="/scratch/base",
            writes="/scratch/upper",
            kernel_scratch="/scratch/work",
            scratch_root="/scratch",
        )


def test_materialize_layout_rejects_empty_base_repo() -> None:
    with pytest.raises(ValueError, match="base_repo must not be empty"):
        MaterializeLayout(
            workspace_root="/workspace",
            base_repo="",
            writes="/scratch/upper",
            kernel_scratch="/scratch/work",
            scratch_root="/scratch",
        )


def test_materialize_layout_rejects_base_repo_outside_scratch_root() -> None:
    with pytest.raises(ValueError, match="base_repo must be strictly under scratch_root"):
        MaterializeLayout(
            workspace_root="/workspace",
            base_repo="/etc/passwd",
            writes="/scratch/upper",
            kernel_scratch="/scratch/work",
            scratch_root="/scratch",
        )


def test_materialize_layout_rejects_duplicate_paths() -> None:
    with pytest.raises(ValueError, match="must be distinct from"):
        MaterializeLayout(
            workspace_root="/workspace",
            base_repo="/scratch/upper",
            writes="/scratch/upper",
            kernel_scratch="/scratch/work",
            scratch_root="/scratch",
        )


def test_overlay_layout_alias_is_materialize_layout() -> None:
    # Starter alias: OverlayLayout(...) constructs MaterializeLayout.
    spec = OverlayLayout(
        workspace_root="/workspace",
        base_repo="/scratch/base",
        writes="/scratch/upper",
        kernel_scratch="/scratch/work",
        scratch_root="/scratch",
    )
    assert isinstance(spec, MaterializeLayout)


# ---------------------------------------------------------------------------
# LayerPathsLayout
# ---------------------------------------------------------------------------


def _valid_layer_paths_layout(
    *,
    layer_paths: tuple[str, ...] = ("/storage/layers/L1", "/storage/layers/L2"),
    layer_storage_root: str = "/storage/layers",
    scratch_root: str = "/scratch",
) -> LayerPathsLayout:
    return LayerPathsLayout(
        workspace_root="/workspace",
        layer_paths=layer_paths,
        layer_storage_root=layer_storage_root,
        writes="/scratch/upper",
        kernel_scratch="/scratch/work",
        scratch_root=scratch_root,
    )


def test_layer_paths_layout_valid() -> None:
    spec = _valid_layer_paths_layout()
    assert spec.layer_paths == ("/storage/layers/L1", "/storage/layers/L2")


def test_layer_paths_layout_rejects_empty_layer_paths() -> None:
    with pytest.raises(ValueError, match="layer_paths must not be empty"):
        LayerPathsLayout(
            workspace_root="/workspace",
            layer_paths=(),
            layer_storage_root="/storage/layers",
            writes="/scratch/upper",
            kernel_scratch="/scratch/work",
            scratch_root="/scratch",
        )


def test_layer_paths_layout_rejects_path_outside_layer_storage_root() -> None:
    with pytest.raises(ValueError, match="must be under layer_storage_root"):
        LayerPathsLayout(
            workspace_root="/workspace",
            layer_paths=("/etc/passwd",),
            layer_storage_root="/storage/layers",
            writes="/scratch/upper",
            kernel_scratch="/scratch/work",
            scratch_root="/scratch",
        )


def test_layer_paths_layout_rejects_depth_over_guard() -> None:
    too_many = tuple(f"/storage/layers/L{i}" for i in range(OVL_MAX_STACK_GUARD + 1))
    with pytest.raises(LayerStackTooDeep):
        LayerPathsLayout(
            workspace_root="/workspace",
            layer_paths=too_many,
            layer_storage_root="/storage/layers",
            writes="/scratch/upper",
            kernel_scratch="/scratch/work",
            scratch_root="/scratch",
        )


def test_layer_paths_layout_accepts_depth_at_guard() -> None:
    at_guard = tuple(f"/storage/layers/L{i}" for i in range(OVL_MAX_STACK_GUARD))
    spec = LayerPathsLayout(
        workspace_root="/workspace",
        layer_paths=at_guard,
        layer_storage_root="/storage/layers",
        writes="/scratch/upper",
        kernel_scratch="/scratch/work",
        scratch_root="/scratch",
    )
    assert len(spec.layer_paths) == OVL_MAX_STACK_GUARD


def test_layer_paths_layout_rejects_writes_outside_scratch_root() -> None:
    with pytest.raises(ValueError, match="writes must be strictly under scratch_root"):
        LayerPathsLayout(
            workspace_root="/workspace",
            layer_paths=("/storage/layers/L1",),
            layer_storage_root="/storage/layers",
            writes="/tmp/outside",
            kernel_scratch="/scratch/work",
            scratch_root="/scratch",
        )


def test_layer_paths_layout_has_no_base_repo() -> None:
    spec = _valid_layer_paths_layout()
    assert not hasattr(spec, "base_repo")
