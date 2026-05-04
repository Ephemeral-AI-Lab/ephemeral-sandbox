"""Layer publisher and transaction-shell tests."""

from __future__ import annotations

import hashlib
from pathlib import Path

import pytest

from sandbox.layer_stack import LayerChange, LayerStackManager, ManifestConflictError
from sandbox.layer_stack.manifest import Manifest, read_manifest, write_manifest_atomic


def _source(tmp_path: Path, name: str, content: bytes) -> Path:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return path


def test_publish_empty_changes_is_noop(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")

    with manager.commit_transaction() as transaction:
        before = transaction.snapshot()
        after = transaction.publish_layer([])

    assert after == before
    assert after.version == 0
    assert after.layers == ()


def test_publish_layer_writes_immutable_layer_and_manifest(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    source = _source(tmp_path, "created.txt", b"created")

    manifest = manager.publish_changes(
        [
            LayerChange(
                path="pkg/created.txt",
                kind="write",
                content_hash=hashlib.sha256(b"created").hexdigest(),
                source_path=str(source),
            )


        ]
    )

    assert manifest.version == 1
    assert len(manifest.layers) == 1
    assert (tmp_path / "stack" / manifest.layers[0].path / "pkg" / "created.txt").exists()
    assert read_manifest(tmp_path / "stack" / "manifest.json") == manifest
    assert manager.read_bytes("pkg/created.txt") == (b"created", True)


def test_content_hash_mismatch_preserves_manifest_and_removes_staging(
    tmp_path: Path,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    source = _source(tmp_path, "bad.txt", b"actual")

    with pytest.raises(ValueError, match="content hash mismatch"):
        manager.publish_changes(
            [
                LayerChange(
                    path="bad.txt",
                    kind="write",
                    content_hash=hashlib.sha256(b"expected").hexdigest(),
                    source_path=str(source),
                )
            ]
        )

    assert manager.read_active_manifest() == Manifest(version=0, layers=())
    assert list((tmp_path / "stack" / "staging").iterdir()) == []


def test_transaction_detects_manifest_conflict_before_publish(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    source = _source(tmp_path, "created.txt", b"created")

    with manager.commit_transaction() as transaction:
        write_manifest_atomic(
            tmp_path / "stack" / "manifest.json",
            Manifest(version=7, layers=()),
        )
        with pytest.raises(ManifestConflictError):
            transaction.publish_layer(
                [
                    LayerChange(
                        path="created.txt",
                        kind="write",
                        source_path=str(source),
                    )
                ]
            )
