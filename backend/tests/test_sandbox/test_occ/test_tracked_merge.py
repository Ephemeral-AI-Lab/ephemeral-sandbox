"""Tracked merge validation for Phase 04 OCC commits."""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.occ.changeset.intent import PreparedPathGroup, RouteDecision
from sandbox.occ.changeset.types import EditChange, FileStatus, WriteChange
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.gated.merge import GatedMerge


def _source(tmp_path: Path, name: str, content: bytes) -> Path:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return path


def _publish(stack: LayerStackManager, tmp_path: Path, rel: str, content: bytes) -> None:
    source = _source(tmp_path, rel.replace("/", "-"), content)
    stack.publish_changes(
        [
            LayerChange(
                path=rel,
                kind="write",
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
        return LayerChange(
            path=path,
            kind="write",
            content_hash=ContentHasher().hash_bytes(content),
            source_path=str(source),
        )

    return stage


def test_tracked_write_requires_active_hash_to_match_prepared_base(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "src/app.py", b"active\n")
    merge = GatedMerge(stack)
    group = PreparedPathGroup(
        path="src/app.py",
        route=RouteDecision.TRACKED,
        changes=(
            WriteChange(
                path="src/app.py",
                final_content=b"new\n",
                base_hash=ContentHasher().hash_bytes(b"leased\n"),
            ),
        ),
        base_hash=ContentHasher().hash_bytes(b"leased\n"),
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
    merge = GatedMerge(stack)
    group = PreparedPathGroup(
        path="src/app.py",
        route=RouteDecision.TRACKED,
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
    merge = GatedMerge(stack)
    group = PreparedPathGroup(
        path="src/app.py",
        route=RouteDecision.TRACKED,
        changes=(EditChange(path="src/app.py", old_text="x", new_text="Y"),),
    )

    result, delta = merge.stage_group(
        group,
        active_manifest=stack.read_active_manifest(),
        stage_write=_stage_write(tmp_path),
    )

    assert result.status is FileStatus.ABORTED_OVERLAP
    assert delta is None
