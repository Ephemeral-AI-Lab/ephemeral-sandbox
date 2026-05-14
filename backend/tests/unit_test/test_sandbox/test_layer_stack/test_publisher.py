"""Layer publisher and transaction-shell tests."""

from __future__ import annotations

import hashlib
from pathlib import Path

import pytest

import sandbox.layer_stack.layer.publisher as publisher_mod
from sandbox.layer_stack import (
    LayerChange,
    WriteLayerChange,
    LayerStackManager,
    ManifestConflictError,
)
from sandbox.layer_stack.manifest import (
    LAYERS_DIR,
    STAGING_DIR,
    Manifest,
    manifest_path,
    read_manifest,
    write_manifest_atomic,
)
from sandbox.layer_stack.layer.publisher import LayerPublisher


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
            WriteLayerChange(
                path="pkg/created.txt",
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


def test_publish_reads_write_source_once(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    source = _source(tmp_path, "created.txt", b"created")
    original_read_bytes = Path.read_bytes
    source_reads = 0

    def count_source_reads(path: Path) -> bytes:
        nonlocal source_reads
        if path == source:
            source_reads += 1
        return original_read_bytes(path)

    monkeypatch.setattr(Path, "read_bytes", count_source_reads)

    manager.publish_changes(
        [
            WriteLayerChange(
                path="pkg/created.txt",
                content_hash=hashlib.sha256(b"created").hexdigest(),
                source_path=str(source),
            )
        ]
    )

    assert source_reads == 1


def test_same_digest_publish_returns_active_manifest_without_new_layer(
    tmp_path: Path,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    source = _source(tmp_path, "created.txt", b"created")
    change = WriteLayerChange(
                 path="pkg/created.txt",
                 content_hash=hashlib.sha256(b"created").hexdigest(),
                 source_path=str(source),
             )

    first = manager.publish_changes([change])
    second = manager.publish_changes([change])

    assert second == first
    assert second.version == 1
    assert len(tuple((tmp_path / "stack" / LAYERS_DIR).iterdir())) == 1


def test_content_hash_mismatch_preserves_manifest_and_removes_staging(
    tmp_path: Path,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    source = _source(tmp_path, "bad.txt", b"actual")

    with pytest.raises(ValueError, match="content hash mismatch"):
        manager.publish_changes(
            [
                WriteLayerChange(
                    path="bad.txt",
                    content_hash=hashlib.sha256(b"expected").hexdigest(),
                    source_path=str(source),
                )
            ]
        )

    assert manager.read_active_manifest() == Manifest(version=0, layers=())
    assert list((tmp_path / "stack" / "staging").iterdir()) == []


def test_transaction_publish_layer_rejects_source_outside_source_root(
    tmp_path: Path,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    trusted_root = tmp_path / "staging"
    trusted_root.mkdir()
    source = _source(tmp_path, "outside.txt", b"outside")

    with manager.commit_transaction() as transaction:
        with pytest.raises(ValueError, match="outside trusted source root"):
            transaction.publish_layer(
                [
                    WriteLayerChange(
                        path="outside.txt",
                        source_path=str(source),
                    )
                ],
                source_root=trusted_root,
            )

    assert manager.read_active_manifest() == Manifest(version=0, layers=())


def test_late_manifest_conflict_removes_unreferenced_layer(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    storage_root = tmp_path / "stack"
    (storage_root / LAYERS_DIR).mkdir(parents=True)
    (storage_root / STAGING_DIR).mkdir()
    manifest_file = manifest_path(storage_root)
    active = Manifest(version=0, layers=())
    write_manifest_atomic(manifest_file, active)
    source = _source(tmp_path, "created.txt", b"created")
    publisher = LayerPublisher(
        storage_root,
        id_factory=lambda _version: "L000001-fixed",
    )
    calls = 0

    def read_manifest_with_late_conflict(_path: str | Path) -> Manifest:
        nonlocal calls
        calls += 1
        if calls == 1:
            return active
        return Manifest(version=99, layers=())

    monkeypatch.setattr(publisher_mod, "read_manifest", read_manifest_with_late_conflict)

    with pytest.raises(ManifestConflictError):
        publisher.publish_layer(
            [
                WriteLayerChange(
                    path="pkg/created.txt",
                    content_hash=hashlib.sha256(b"created").hexdigest(),
                    source_path=str(source),
                )
            ],
            expected_manifest=active,
        )

    assert list((storage_root / LAYERS_DIR).iterdir()) == []
    assert list((storage_root / STAGING_DIR).iterdir()) == []


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
                    WriteLayerChange(
                        path="created.txt",
                        source_path=str(source),
                    )
                ]
            )
