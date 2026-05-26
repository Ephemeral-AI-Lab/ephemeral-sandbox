"""Phase 08 squash/GC regression tests for the no-cache layer stack."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.layer_stack import WriteLayerChange, LayerStack
from sandbox.layer_stack.manifest import LayerRef, Manifest, write_manifest_atomic
from sandbox.layer_stack.squash import CheckpointSegment


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def _publish(
    manager: LayerStack,
    tmp_path: Path,
    rel: str,
    content: bytes,
) -> Manifest:
    return manager.publish_changes(
        [
            WriteLayerChange(
                path=rel,
                source_path=_source(tmp_path, rel.replace("/", "_"), content),
            )
        ]
    )


def _layer_path(manager: LayerStack, layer: LayerRef) -> Path:
    return manager.storage_root / layer.path


def _digest_path(manager: LayerStack, layer: LayerRef) -> Path:
    return manager.storage_root / ".layer-metadata" / f"{layer.layer_id}.digest"


def test_squash_gc_keeps_active_and_leased_layers_then_release_removes_only_old_refs(
    tmp_path: Path,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    _publish(manager, tmp_path, "a.txt", b"a1")
    _publish(manager, tmp_path, "b.txt", b"b1")
    _publish(manager, tmp_path, "a.txt", b"a2")
    lease = manager.acquire_snapshot_lease("leased-reader")
    leased_layers = lease.manifest.layers

    _publish(manager, tmp_path, "c.txt", b"c1")
    _publish(manager, tmp_path, "d.txt", b"d1")
    _publish(manager, tmp_path, "e.txt", b"e1")
    squashed = manager.squash(max_depth=2)

    assert squashed is not None
    assert squashed.depth == 3
    assert squashed.layers[0].layer_id.startswith("B")
    assert squashed.layers[1] == leased_layers[0]
    assert squashed.layers[2].layer_id.startswith("B")
    assert manager.read_text("a.txt", manifest=lease.manifest) == ("a2", True)
    assert manager.read_text("b.txt", manifest=lease.manifest) == ("b1", True)
    assert all(_layer_path(manager, layer).is_dir() for layer in leased_layers)

    assert manager.release_lease(lease.lease_id) is True

    assert _layer_path(manager, leased_layers[0]).is_dir()
    assert all(not _layer_path(manager, layer).exists() for layer in leased_layers[1:])
    final_squash = manager.squash(max_depth=2)

    assert final_squash is not None
    assert final_squash.depth == 1
    assert all(not _layer_path(manager, layer).exists() for layer in leased_layers)
    assert manager.read_text("a.txt") == ("a2", True)
    assert manager.read_text("b.txt") == ("b1", True)
    assert manager.read_text("c.txt") == ("c1", True)
    assert manager.read_text("d.txt") == ("d1", True)
    assert manager.read_text("e.txt") == ("e1", True)


def test_squash_gc_removes_digest_metadata_for_deleted_suffix_layers(
    tmp_path: Path,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    for index in range(4):
        _publish(
            manager,
            tmp_path,
            f"value-{index:02d}.txt",
            f"value-{index:02d}\n".encode("utf-8"),
        )
    before = manager.read_active_manifest()
    assert all(_digest_path(manager, layer).is_file() for layer in before.layers)

    squashed = manager.squash(max_depth=2)

    assert squashed is not None
    assert squashed.depth == 1
    assert squashed.layers[0].layer_id.startswith("B")
    for layer in before.layers:
        assert _layer_path(manager, layer).exists() is False
        assert _digest_path(manager, layer).exists() is False


def test_release_lease_does_not_delete_layers_still_in_active_manifest(
    tmp_path: Path,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    manifest = _publish(manager, tmp_path, "active.txt", b"still-active\n")
    lease = manager.acquire_snapshot_lease("active-reader")

    assert manager.release_lease(lease.lease_id) is True

    assert _layer_path(manager, manifest.layers[0]).is_dir()
    assert manager.read_text("active.txt") == ("still-active\n", True)
    assert manager.leased_layers() == ()


def test_checkpoint_relabel_moves_prebuilt_checkpoint_to_publish_version(
    tmp_path: Path,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    for index in range(3):
        _publish(
            manager,
            tmp_path,
            f"base/{index:02d}.txt",
            f"base-{index:02d}\n".encode("utf-8"),
        )
    plan = manager._squash.plan(manager.read_active_manifest(), max_depth=1)
    assert plan is not None
    checkpoint = manager._squash.build_checkpoint(
        plan.checkpoint_segments[0],
        active_version=plan.active_version,
    )
    original_path = _layer_path(manager, checkpoint)

    relabeled = manager._squash.relabel_checkpoint(
        checkpoint,
        manifest_version=42,
    )

    assert original_path.exists() is False
    assert relabeled.layer_id.startswith("B000042-")
    assert _layer_path(manager, relabeled).is_dir()


def test_squash_cas_keeps_concurrent_prefix_append_and_versions_checkpoint(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    for index in range(5):
        _publish(
            manager,
            tmp_path,
            f"base/{index:02d}.txt",
            f"base-{index:02d}\n".encode("utf-8"),
        )
    real_build_checkpoint = manager._squash.build_checkpoint
    built: list[LayerRef] = []

    def build_checkpoint_then_append(
        segment: CheckpointSegment,
        *,
        active_version: int,
    ) -> LayerRef:
        checkpoint = real_build_checkpoint(segment, active_version=active_version)
        built.append(checkpoint)
        _publish(manager, tmp_path, "race/appended.txt", b"appended\n")
        return checkpoint

    monkeypatch.setattr(
        manager._squash,
        "build_checkpoint",
        build_checkpoint_then_append,
    )

    squashed = manager.squash(max_depth=2)

    assert squashed is not None
    assert squashed.depth == 2
    assert squashed.layers[0].layer_id.startswith("L000006-")
    assert squashed.layers[1].layer_id.startswith(f"B{squashed.version:06d}-")
    assert _layer_path(manager, squashed.layers[1]).is_dir()
    assert built
    assert _layer_path(manager, built[0]).exists() is False
    assert manager.read_text("race/appended.txt") == ("appended\n", True)
    for index in range(5):
        assert manager.read_text(f"base/{index:02d}.txt") == (
            f"base-{index:02d}\n",
            True,
        )


def test_squash_cas_mismatch_discards_unpublished_checkpoint(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    for index in range(5):
        _publish(
            manager,
            tmp_path,
            f"base/{index:02d}.txt",
            f"base-{index:02d}\n".encode("utf-8"),
        )
    before = manager.read_active_manifest()
    real_build_checkpoint = manager._squash.build_checkpoint
    built: list[LayerRef] = []

    def build_checkpoint_then_rewrite_manifest(
        segment: CheckpointSegment,
        *,
        active_version: int,
    ) -> LayerRef:
        checkpoint = real_build_checkpoint(segment, active_version=active_version)
        built.append(checkpoint)
        write_manifest_atomic(
            manager.storage_root / "manifest.json",
            Manifest(version=before.version + 1, layers=before.layers[:2]),
        )
        return checkpoint

    monkeypatch.setattr(
        manager._squash,
        "build_checkpoint",
        build_checkpoint_then_rewrite_manifest,
    )

    squashed = manager.squash(max_depth=2)

    assert squashed is None
    assert built
    assert _layer_path(manager, built[0]).exists() is False
    assert manager.read_active_manifest().layers == before.layers[:2]


def test_squash_pins_planned_layers_during_checkpoint_build(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    for index in range(5):
        _publish(
            manager,
            tmp_path,
            f"base/{index:02d}.txt",
            f"base-{index:02d}\n".encode("utf-8"),
        )

    real_build_checkpoint = manager._squash.build_checkpoint
    triggered = False
    concurrent_results: list[Manifest | None] = []

    def build_checkpoint_after_concurrent_squash(
        segment: CheckpointSegment,
        *,
        active_version: int,
    ) -> LayerRef:
        nonlocal triggered
        if not triggered:
            triggered = True
            monkeypatch.setattr(
                manager._squash,
                "build_checkpoint",
                real_build_checkpoint,
            )
            concurrent = manager.squash(max_depth=2)
            concurrent_results.append(concurrent)
            monkeypatch.setattr(
                manager._squash,
                "build_checkpoint",
                build_checkpoint_after_concurrent_squash,
            )
            assert concurrent is not None
            assert concurrent.depth == 2
        return real_build_checkpoint(segment, active_version=active_version)

    monkeypatch.setattr(
        manager._squash,
        "build_checkpoint",
        build_checkpoint_after_concurrent_squash,
    )

    squashed = manager.squash(max_depth=2)

    assert triggered is True
    assert squashed is None
    assert concurrent_results and concurrent_results[0] is not None
    assert manager.active_lease_count() == 0
    active = manager.read_active_manifest()
    assert active.depth == 2
    for index in range(5):
        assert manager.read_text(f"base/{index:02d}.txt") == (
            f"base-{index:02d}\n",
            True,
        )
