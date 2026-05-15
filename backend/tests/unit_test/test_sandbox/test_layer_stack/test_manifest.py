"""Manifest and change-object tests for sandbox layer stacks."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.layer_stack.changes import (
    LayerChange,
    WriteLayerChange,
    aggregate_layer_changes,
    normalize_layer_path,
)
from sandbox.layer_stack.manifest import (
    MANIFEST_SCHEMA_VERSION,
    LayerRef,
    Manifest,
    ManifestConflictError,
    read_manifest,
    write_manifest_atomic,
)


def test_manifest_round_trips_layer_refs_newest_first(tmp_path: Path) -> None:
    manifest = Manifest(
        version=2,
        layers=(
            LayerRef(layer_id="L000002", path="layers/L000002"),
            LayerRef(layer_id="L000001", path="layers/L000001"),
        ),
    )

    manifest_file = tmp_path / "manifest.json"
    write_manifest_atomic(manifest_file, manifest)

    assert read_manifest(manifest_file) == manifest
    assert read_manifest(manifest_file).layers[0].layer_id == "L000002"
    assert read_manifest(manifest_file).schema_version == MANIFEST_SCHEMA_VERSION
    assert manifest.to_dict()["schema_version"] == MANIFEST_SCHEMA_VERSION


def test_manifest_rejects_legacy_string_layer_refs() -> None:
    with pytest.raises(ValueError, match="manifest layer entries must be objects"):
        Manifest.from_dict({"version": 1, "layers": ["L000001"]})


def test_manifest_rejects_newer_schema_version() -> None:
    with pytest.raises(ManifestConflictError, match="newer than this runtime"):
        Manifest.from_dict(
            {
                "schema_version": MANIFEST_SCHEMA_VERSION + 1,
                "version": 1,
                "layers": [],
            }
        )


def test_layer_paths_are_normalized_and_cannot_escape_stack() -> None:
    assert normalize_layer_path("pkg//module.py") == "pkg/module.py"
    assert normalize_layer_path("./pkg\\module.py") == "pkg/module.py"

    for path in ("", ".", "/absolute.py", "../escape.py", "pkg/../escape.py"):
        with pytest.raises(ValueError):
            normalize_layer_path(path)


def test_layer_change_validates_storage_level_payload_shape(tmp_path: Path) -> None:
    source = tmp_path / "payload.txt"
    source.write_text("payload\n", encoding="utf-8")

    assert (
        WriteLayerChange(
            path="pkg/new.py",
            source_path=str(source),
        ).path
        == "pkg/new.py"
    )

    with pytest.raises(ValueError, match="write changes require source_path"):
        WriteLayerChange(path="missing.py")

    with pytest.raises(ValueError, match="delete changes must not carry source_path"):
        LayerChange(kind="delete", path="old.py", source_path=str(source))

    with pytest.raises(ValueError, match="symlink changes must not carry content_hash"):
        LayerChange(
            kind="symlink",
            path="link.py",
            source_path="target.py",
            content_hash="x",
        )


def test_layer_change_aggregation_keeps_final_change_per_path(tmp_path: Path) -> None:
    first = tmp_path / "first.txt"
    second = tmp_path / "second.txt"
    other = tmp_path / "other.txt"
    first.write_text("first\n", encoding="utf-8")
    second.write_text("second\n", encoding="utf-8")
    other.write_text("other\n", encoding="utf-8")

    delta = aggregate_layer_changes(
        (
            WriteLayerChange(path="b.txt", source_path=str(first)),
            WriteLayerChange(path="a.txt", source_path=str(other)),
            WriteLayerChange(path="b.txt", source_path=str(second)),
        )
    )

    assert [change.path for change in delta] == ["a.txt", "b.txt"]
    assert delta[1].source_path == str(second)
