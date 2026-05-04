"""Phase 03 base-hash inference tests."""

from __future__ import annotations

import asyncio

from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.occ.changeset.intent import RouteDecision
from sandbox.occ.changeset.types import DeleteChange, EditChange, WriteChange
from sandbox.occ.content.gitignore_oracle import GitignoreOracle, RunOutcome
from sandbox.occ.runtime_ops import content_hash_bytes
from sandbox.occ.service import OccService


def _never_ignored() -> GitignoreOracle:
    return GitignoreOracle(
        "/unused",
        run=lambda _argv, _stdin_bytes: RunOutcome(returncode=1, stdout=b"", stderr=b""),
    )


def _stack_with_file(tmp_path, rel: str, content: bytes) -> LayerStackManager:
    stack = LayerStackManager(tmp_path / "layers")
    source = tmp_path / "payload"
    source.write_bytes(content)
    stack.publish_changes(
        [
            LayerChange(
                path=rel,
                kind="write",
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
            LayerChange(
                path="src/app.py",
                kind="write",
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
                    source="shell_capture",
                    final_content=b"next\n",
                    base_hash=None,
                )
            ],
            snapshot=snapshot,
        )
    )

    [group] = prepared.path_groups
    [change] = group.changes
    assert group.route is RouteDecision.TRACKED
    assert group.base_hash == content_hash_bytes(b"old\n")
    assert isinstance(change, WriteChange)
    assert change.base_hash == content_hash_bytes(b"old\n")


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
    assert group.base_hash is None
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
    assert group.base_hash is None
    assert isinstance(change, EditChange)
    assert change.old_text == "old"
    assert change.new_text == "new"


def test_shell_delete_can_infer_base_hash_from_snapshot(tmp_path) -> None:
    stack = _stack_with_file(tmp_path, "src/gone.py", b"delete me")
    snapshot = stack.read_active_manifest()
    service = OccService(gitignore=_never_ignored(), layer_stack=stack)

    prepared = asyncio.run(
        service.prepare_changeset(
            [DeleteChange(path="src/gone.py", source="shell_capture")],
            snapshot=snapshot,
        )
    )

    [group] = prepared.path_groups
    [change] = group.changes
    assert isinstance(change, DeleteChange)
    assert change.base_hash == content_hash_bytes(b"delete me")
