"""Phase 03 base-hash inference tests."""

from __future__ import annotations

import asyncio

from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.prepared import RouteDecision
from sandbox.occ.changeset.types import DeleteChange, EditChange, WriteChange
from sandbox.occ.content.gitignore_oracle import GitignoreMatcher
from sandbox.occ.content.hashing import content_hash_bytes
from sandbox.occ.service import OccService


class _NeverIgnored:
    def is_ignored(self, _path: str) -> bool:
        return False

    def is_ignored_in_snapshot(self, path: str, _snapshot: object) -> bool:
        return self.is_ignored(path)


def _never_ignored() -> GitignoreMatcher:
    return _NeverIgnored()


def _stack_with_file(tmp_path, rel: str, content: bytes) -> LayerStackManager:
    stack = LayerStackManager(tmp_path / "layers")
    source = tmp_path / "payload"
    source.write_bytes(content)
    stack.publish_changes(
        [
            WriteLayerChange(
                path=rel,
                content_hash=content_hash_bytes(content),
                source_path=str(source),
            )
        ]
    )
    return stack


def test_tracked_write_without_base_hash_uses_leased_snapshot_hash(tmp_path) -> None:
    stack = _stack_with_file(tmp_path, "src/app.py", b"old\n")
    snapshot = stack.acquire_snapshot_lease("req-1").manifest
    # Advance active manifest after the lease; Phase 03 must still use M0.
    source = tmp_path / "new-payload"
    source.write_bytes(b"active\n")
    stack.publish_changes(
        [
            WriteLayerChange(
                path="src/app.py",
                content_hash=content_hash_bytes(b"active\n"),
                source_path=str(source),
            )
        ]
    )

    service = OccService(gitignore=_never_ignored(), layer_stack=stack)
    prepared = asyncio.run(
        service.prepare_changeset(
            [
                WriteChange(
                    path="src/app.py",
                    source="overlay_capture",
                    final_content=b"next\n",
                    base_hash=None,
                )
            ],
            snapshot=snapshot,
        )
    )

    [group] = prepared.path_groups
    [change] = group.changes
    assert group.route is RouteDecision.GATED
    assert isinstance(change, WriteChange)
    assert change.base_hash == content_hash_bytes(b"old\n")


def test_chained_writes_use_running_base_hash(tmp_path) -> None:
    stack = _stack_with_file(tmp_path, "src/app.py", b"old\n")
    snapshot = stack.read_active_manifest()
    service = OccService(gitignore=_never_ignored(), layer_stack=stack)

    prepared = asyncio.run(
        service.prepare_changeset(
            [
                WriteChange(path="src/app.py", final_content=b"first\n"),
                WriteChange(path="src/app.py", final_content=b"second\n"),
            ],
            snapshot=snapshot,
        )
    )

    [group] = prepared.path_groups
    first, second = group.changes
    assert isinstance(first, WriteChange)
    assert isinstance(second, WriteChange)
    assert first.base_hash == content_hash_bytes(b"old\n")
    assert second.base_hash == content_hash_bytes(b"first\n")


def test_missing_snapshot_path_infers_none_base_hash(tmp_path) -> None:
    stack = LayerStackManager(tmp_path / "layers")
    snapshot = stack.read_active_manifest()
    service = OccService(gitignore=_never_ignored(), layer_stack=stack)

    prepared = asyncio.run(
        service.prepare_changeset(
            [WriteChange(path="new.py", source="api_write", final_content=b"x")],
            snapshot=snapshot,
        )
    )

    [group] = prepared.path_groups
    [change] = group.changes
    assert isinstance(change, WriteChange)
    assert change.base_hash is None


def test_edit_changes_keep_anchor_contract_without_base_hash(tmp_path) -> None:
    stack = _stack_with_file(tmp_path, "src/app.py", b"old\n")
    snapshot = stack.read_active_manifest()
    service = OccService(gitignore=_never_ignored(), layer_stack=stack)

    prepared = asyncio.run(
        service.prepare_changeset(
            [EditChange(path="src/app.py", old_text="old", new_text="new")],
            snapshot=snapshot,
        )
    )

    [group] = prepared.path_groups
    [change] = group.changes
    assert isinstance(change, EditChange)
    assert change.old_text == "old"
    assert change.new_text == "new"


def test_shell_delete_can_infer_base_hash_from_snapshot(tmp_path) -> None:
    stack = _stack_with_file(tmp_path, "src/gone.py", b"delete me")
    snapshot = stack.read_active_manifest()
    service = OccService(gitignore=_never_ignored(), layer_stack=stack)

    prepared = asyncio.run(
        service.prepare_changeset(
            [DeleteChange(path="src/gone.py", source="overlay_capture")],
            snapshot=snapshot,
        )
    )

    [group] = prepared.path_groups
    [change] = group.changes
    assert isinstance(change, DeleteChange)
    assert change.base_hash == content_hash_bytes(b"delete me")
