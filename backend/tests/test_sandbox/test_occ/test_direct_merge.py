"""Direct merge staging for Phase 04 OCC commits."""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.occ.changeset.intent import PreparedPathGroup, RouteDecision
from sandbox.occ.changeset.types import (
    EditChange,
    FileStatus,
    OpaqueDirChange,
    SymlinkChange,
    WriteChange,
)
from sandbox.occ.direct.merge import DirectMerge
from sandbox.occ.content.hashing import ContentHasher


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
        source = _source(tmp_path, f"direct-{counter}.bin", content)
        return LayerChange(
            path=path,
            kind="write",
            content_hash=ContentHasher().hash_bytes(content),
            source_path=str(source),
        )

    return stage


def test_direct_write_stages_last_writer_wins_content(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    merge = DirectMerge(stack)
    group = PreparedPathGroup(
        path="dist/app.js",
        route=RouteDecision.DIRECT,
        changes=(
            WriteChange(path="dist/app.js", final_content=b"first"),
            WriteChange(path="dist/app.js", final_content=b"second"),
        ),
    )

    result, delta = merge.stage_group(
        group,
        active_manifest=stack.read_active_manifest(),
        stage_write=_stage_write(tmp_path),
    )

    assert result.status is FileStatus.ACCEPTED
    assert delta is not None
    [change] = delta.changes
    assert Path(change.source_path or "").read_bytes() == b"second"


def test_direct_edit_is_best_effort(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "dist/app.js", b"alpha\n")
    merge = DirectMerge(stack)
    group = PreparedPathGroup(
        path="dist/app.js",
        route=RouteDecision.DIRECT,
        changes=(EditChange(path="dist/app.js", old_text="missing", new_text="X"),),
    )

    result, delta = merge.stage_group(
        group,
        active_manifest=stack.read_active_manifest(),
        stage_write=_stage_write(tmp_path),
    )

    assert result.status is FileStatus.ACCEPTED
    assert delta is not None
    [change] = delta.changes
    assert Path(change.source_path or "").read_bytes() == b"alpha\n"


def test_direct_symlink_and_opaque_dir_stage_storage_changes(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    merge = DirectMerge(stack)
    symlink_group = PreparedPathGroup(
        path="link",
        route=RouteDecision.DIRECT,
        changes=(SymlinkChange(path="link", target="../target"),),
    )
    opaque_group = PreparedPathGroup(
        path="cache",
        route=RouteDecision.DIRECT,
        changes=(OpaqueDirChange(path="cache", kept_children=frozenset({"keep"})),),
    )

    symlink_result, symlink_delta = merge.stage_group(
        symlink_group,
        active_manifest=stack.read_active_manifest(),
        stage_write=_stage_write(tmp_path),
    )
    opaque_result, opaque_delta = merge.stage_group(
        opaque_group,
        active_manifest=stack.read_active_manifest(),
        stage_write=_stage_write(tmp_path),
    )

    assert symlink_result.status is FileStatus.ACCEPTED
    assert symlink_delta is not None
    assert symlink_delta.changes[0].kind == "symlink"
    assert symlink_delta.changes[0].source_path == "../target"
    assert opaque_result.status is FileStatus.ACCEPTED
    assert opaque_delta is not None
    assert opaque_delta.changes[0].kind == "opaque_dir"


def test_direct_same_path_opaque_dir_respects_later_write(
    tmp_path: Path,
) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    merge = DirectMerge(stack)
    group = PreparedPathGroup(
        path="cache",
        route=RouteDecision.DIRECT,
        changes=(
            OpaqueDirChange(path="cache", kept_children=frozenset()),
            WriteChange(path="cache", final_content=b"file wins"),
        ),
    )

    result, delta = merge.stage_group(
        group,
        active_manifest=stack.read_active_manifest(),
        stage_write=_stage_write(tmp_path),
    )

    assert result.status is FileStatus.ACCEPTED
    assert delta is not None
    [change] = delta.changes
    assert change.kind == "write"
    assert Path(change.source_path or "").read_bytes() == b"file wins"
