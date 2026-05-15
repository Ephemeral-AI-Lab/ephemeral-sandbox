"""LayerRef.path and resolve_storage_path traversal-rejection tests."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.layer_stack.paths import resolve_storage_path
from sandbox.layer_stack.manifest import LayerRef, Manifest


@pytest.mark.parametrize(
    "bad_path",
    [
        "../escape",
        "foo/../bar",
        "foo/..",
        "/etc/passwd",
        "a\0b",
    ],
)
def test_layer_ref_rejects_traversal_absolute_and_nul(bad_path: str) -> None:
    with pytest.raises(ValueError):
        LayerRef(layer_id="L000001", path=bad_path)


def test_manifest_from_dict_rejects_bad_layer_path() -> None:
    with pytest.raises(ValueError):
        Manifest.from_dict(
            {
                "version": 1,
                "layers": [{"layer_id": "L000001", "path": "../escape"}],
            }
        )


def test_resolve_storage_path_rejects_absolute_input(tmp_path: Path) -> None:
    with pytest.raises(ValueError):
        resolve_storage_path(tmp_path, "/etc/passwd")


def test_resolve_storage_path_rejects_escape_via_traversal(tmp_path: Path) -> None:
    # Even if a caller skipped LayerRef validation and produced a relative
    # path that resolves outside storage_root, the defense-in-depth check
    # in resolve_storage_path must refuse it.
    with pytest.raises(ValueError):
        resolve_storage_path(tmp_path, "../../etc")


def test_resolve_storage_path_accepts_well_formed_relative(tmp_path: Path) -> None:
    resolved = resolve_storage_path(tmp_path, "layers/L000001")
    assert resolved == tmp_path / "layers" / "L000001"
