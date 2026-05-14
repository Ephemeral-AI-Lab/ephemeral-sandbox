"""Tracked merge validation for Phase 04 OCC commits."""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.prepared import PreparedPathGroup, RouteDecision
from sandbox.occ.changeset.types import (
    EditChange,
    FileStatus,
    OpaqueDirChange,
    SymlinkChange,
    WriteChange,
)
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.stage.gated import GatedStager


def _source(tmp_path: Path, name: str, content: bytes) -> Path:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return path


def _publish(stack: LayerStackManager, tmp_path: Path, rel: str, content: bytes) -> None:
    source = _source(tmp_path, rel.replace("/", "-"), content)
    stack.publish_changes(
        [
            WriteLayerChange(
                path=rel,
                content_hash=ContentHasher().hash_bytes(content),
                source_path=str(source),
            )
        ]
    )


def _stage_write(tmp_path: Path):
    counter = 0

    def stage(path: str, content: bytes) -> LayerChange:
        nonlocal counter
        counter += 1
        source = _source(tmp_path, f"staged-{counter}.bin", content)
        return WriteLayerChange(
            path=path,
            content_hash=ContentHasher().hash_bytes(content),
            source_path=str(source),
        )

    return stage


def test_tracked_write_requires_active_hash_to_match_prepared_base(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "src/app.py", b"active\n")
    merge = GatedStager(stack)
    group = PreparedPathGroup(
        path="src/app.py",
        route=RouteDecision.GATED,
        changes=(
            WriteChange(
                path="src/app.py",
                final_content=b"new\n",
                base_hash=ContentHasher().hash_bytes(b"leased\n"),
            ),
        ),
    )

    result, delta = merge.stage_group(
        group,
        active_manifest=stack.read_active_manifest(),
        stage_write=_stage_write(tmp_path),
    )

    assert result.status is FileStatus.ABORTED_VERSION
    assert delta is None


def test_tracked_edit_applies_unique_anchor_to_active_content(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "src/app.py", b"alpha\nbeta\n")
    merge = GatedStager(stack)
    group = PreparedPathGroup(
        path="src/app.py",
        route=RouteDecision.GATED,
        changes=(EditChange(path="src/app.py", old_text="beta", new_text="BETA"),),
    )

    result, delta = merge.stage_group(
        group,
        active_manifest=stack.read_active_manifest(),
        stage_write=_stage_write(tmp_path),
    )

    assert result.status is FileStatus.ACCEPTED
    assert delta is not None
    [change] = delta.changes
    assert Path(change.source_path or "").read_bytes() == b"alpha\nBETA\n"


def test_tracked_edit_aborts_when_anchor_is_ambiguous(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "src/app.py", b"x\nx\n")
    merge = GatedStager(stack)
    group = PreparedPathGroup(
        path="src/app.py",
        route=RouteDecision.GATED,
        changes=(EditChange(path="src/app.py", old_text="x", new_text="Y"),),
    )

    result, delta = merge.stage_group(
        group,
        active_manifest=stack.read_active_manifest(),
        stage_write=_stage_write(tmp_path),
    )

    assert result.status is FileStatus.ABORTED_OVERLAP
    assert delta is None


def test_tracked_opaque_dir_overlay_change_stages_storage_change(
    tmp_path: Path,
) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    merge = GatedStager(stack)
    opaque_group = PreparedPathGroup(
        path=".omc/results",
        route=RouteDecision.GATED,
        changes=(OpaqueDirChange(path=".omc/results", kept_children=frozenset()),),
    )

    opaque_result, opaque_delta = merge.stage_group(
        opaque_group,
        active_manifest=stack.read_active_manifest(),
        stage_write=_stage_write(tmp_path),
    )

    assert opaque_result.status is FileStatus.ACCEPTED
    assert opaque_delta is not None
    assert opaque_delta.changes[0].kind == "opaque_dir"


def test_tracked_symlink_overlay_change_is_rejected(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    merge = GatedStager(stack)
    group = PreparedPathGroup(
        path="link",
        route=RouteDecision.GATED,
        changes=(SymlinkChange(path="link", target="../target"),),
    )

    result, delta = merge.stage_group(
        group,
        active_manifest=stack.read_active_manifest(),
        stage_write=_stage_write(tmp_path),
    )

    assert result.status is FileStatus.REJECTED
    assert result.message == "unsupported tracked change kind: SymlinkChange"
    assert delta is None
