"""Pathspec-only snapshot gitignore oracle tests."""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack import LayerChange, WriteLayerChange, LayerStackManager
from sandbox.occ.content.gitignore_oracle import SnapshotGitignoreOracle
from sandbox.occ.content.hashing import ContentHasher


def _publish(manager: LayerStackManager, tmp_path: Path, rel: str, content: bytes) -> None:
    source = tmp_path / "sources" / rel.replace("/", "-")
    source.parent.mkdir(parents=True, exist_ok=True)
    source.write_bytes(content)
    manager.publish_changes(
        [
            WriteLayerChange(
                path=rel,
                content_hash=ContentHasher().hash_bytes(content),
                source_path=str(source),
            )
        ]
    )


def _seed_repo(manager: LayerStackManager, tmp_path: Path) -> None:
    _publish(manager, tmp_path, ".gitignore", b"build/*\n!build/keep.txt\n")
    _publish(manager, tmp_path, "pkg/.gitignore", b"*.tmp\n!important.tmp\n")


def test_snapshot_oracle_reads_gitignore_from_layer_stack(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    _seed_repo(manager, tmp_path)

    oracle = SnapshotGitignoreOracle(manager)

    assert oracle.is_ignored("build/out.o") is True
    assert oracle.is_ignored("build/keep.txt") is False
    assert oracle.is_ignored("pkg/cache.tmp") is True
    assert oracle.is_ignored("pkg/important.tmp") is False


def test_snapshot_oracle_reuses_pathspec_reader_per_manifest(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    _seed_repo(manager, tmp_path)
    oracle = SnapshotGitignoreOracle(manager)

    assert oracle.is_ignored("build/out.o") is True
    assert oracle.cache_misses == 1
    assert oracle.cache_hits == 0

    assert oracle.is_ignored("pkg/cache.tmp") is True
    assert oracle.cache_misses == 1
    assert oracle.cache_hits == 1


def test_snapshot_oracle_separates_cache_by_manifest_version(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    _publish(manager, tmp_path, ".gitignore", b"*.log\n")
    oracle = SnapshotGitignoreOracle(manager)

    assert oracle.is_ignored("debug.log") is True
    assert oracle.cache_misses == 1

    _publish(manager, tmp_path, ".gitignore", b"")

    assert oracle.is_ignored("debug.log") is False
    assert oracle.cache_misses == 2


def test_snapshot_oracle_does_not_materialize_gitignore_workspace(
    tmp_path: Path,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    _seed_repo(manager, tmp_path)

    oracle = SnapshotGitignoreOracle(manager)
    assert oracle.is_ignored("build/out.o") is True

    assert not (manager.storage_root / "runtime").exists()
