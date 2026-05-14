"""Gitignore-aware OCC policy edge cases.

These tests cover the E13 policy matrix from the per-call snapshot layer-stack
plan: tracked paths use OCC-gated validation, gitignored paths use OCC-skipped
last-writer-wins staging, and route decisions made during prepare are the
authority consumed by commit.
"""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack.layer.change import WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.prepared import RouteDecision
from sandbox.occ.changeset.types import (
    ChangesetResult,
    Change,
    DeleteChange,
    FileStatus,
    WriteChange,
)
from sandbox.occ.stage.transaction import CommitTransaction
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.service import Service


class _MutableGitignore:
    def __init__(self, ignored: set[str] | None = None) -> None:
        self.ignored = ignored or set()

    def is_ignored(self, path: str) -> bool:
        return path in self.ignored

    def is_ignored_in_snapshot(self, path: str, _snapshot: object) -> bool:
        return self.is_ignored(path)


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


def _service(
    stack: LayerStackManager,
    *,
    ignored: set[str] | None = None,
) -> Service:
    return Service(
        gitignore=_MutableGitignore(ignored), snapshot_reader=stack, staging=stack, publisher=stack
    )


def _apply(
    service: Service,
    changes: list[Change],
    *,
    snapshot,
) -> ChangesetResult:
    result = service.apply_changeset_sync(changes, snapshot=snapshot)
    assert isinstance(result, ChangesetResult)
    return result


def _statuses(result: ChangesetResult) -> list[FileStatus]:
    return [file.status for file in result.files]


def test_gitignored_same_path_writes_are_occ_skipped_last_writer_wins(
    tmp_path: Path,
) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    service = _service(stack, ignored={"dist/out.js"})
    stale_snapshot = stack.read_active_manifest()

    first = _apply(
        service,
        [
            WriteChange(
                path="dist/out.js",
                source="overlay_capture",
                final_content=b"first\n",
            )
        ],
        snapshot=stale_snapshot,
    )
    second = _apply(
        service,
        [
            WriteChange(
                path="dist/out.js",
                source="overlay_capture",
                final_content=b"second\n",
            )
        ],
        snapshot=stale_snapshot,
    )

    assert _statuses(first) == [FileStatus.ACCEPTED]
    assert _statuses(second) == [FileStatus.ACCEPTED]
    assert stack.read_bytes("dist/out.js") == (b"second\n", True)


def test_gitignored_delete_uses_last_writer_wins_against_stale_snapshot(
    tmp_path: Path,
) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "dist/cache.bin", b"leased\n")
    stale_snapshot = stack.read_active_manifest()
    _publish(stack, tmp_path, "dist/cache.bin", b"active\n")
    service = _service(stack, ignored={"dist/cache.bin"})

    result = _apply(
        service,
        [DeleteChange(path="dist/cache.bin", source="overlay_capture")],
        snapshot=stale_snapshot,
    )

    assert _statuses(result) == [FileStatus.ACCEPTED]
    assert stack.read_bytes("dist/cache.bin") == (None, False)


def test_tracked_same_path_stale_shell_write_aborts_with_aborted_version(
    tmp_path: Path,
) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "src/app.py", b"leased\n")
    stale_snapshot = stack.read_active_manifest()
    _publish(stack, tmp_path, "src/app.py", b"active\n")
    service = _service(stack)

    result = _apply(
        service,
        [
            WriteChange(
                path="src/app.py",
                source="overlay_capture",
                final_content=b"stale shell\n",
            )
        ],
        snapshot=stale_snapshot,
    )

    assert _statuses(result) == [FileStatus.ABORTED_VERSION]
    assert result.published_manifest_version is None
    assert stack.read_bytes("src/app.py") == (b"active\n", True)


def test_current_mixed_shell_tracked_conflict_drops_gitignored_occ_skipped_output(
    tmp_path: Path,
) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "src/app.py", b"leased\n")
    stale_snapshot = stack.read_active_manifest()
    _publish(stack, tmp_path, "src/app.py", b"active\n")
    service = _service(stack, ignored={"dist/out.js"})

    result = _apply(
        service,
        [
            WriteChange(
                path="src/app.py",
                source="overlay_capture",
                final_content=b"tracked shell\n",
            ),
            WriteChange(
                path="dist/out.js",
                source="overlay_capture",
                final_content=b"occ skipped shell\n",
            ),
        ],
        snapshot=stale_snapshot,
    )

    assert _statuses(result) == [FileStatus.ABORTED_VERSION, FileStatus.DROPPED]
    assert result.published_manifest_version is None
    assert stack.read_bytes("src/app.py") == (b"active\n", True)
    assert stack.read_bytes("dist/out.js") == (None, False)


def test_gitignore_occ_skipped_route_is_fixed_after_prepare_even_if_oracle_changes(
    tmp_path: Path,
) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    gitignore = _MutableGitignore({"dist/out.js"})
    service = Service(gitignore=gitignore, snapshot_reader=stack, staging=stack, publisher=stack)

    prepared = service.prepare_changeset_sync(
        [
            WriteChange(
                path="dist/out.js",
                source="overlay_capture",
                final_content=b"occ skipped\n",
            ),
        ],
        snapshot=stack.read_active_manifest(),
    )
    [group] = prepared.path_groups
    assert group.route is RouteDecision.DIRECT

    gitignore.ignored.clear()
    result = CommitTransaction(
        snapshot_reader=stack,
        staging=stack,
        publisher=stack,
    ).revalidate_and_publish(prepared)

    assert _statuses(result) == [FileStatus.ACCEPTED]
    assert stack.read_bytes("dist/out.js") == (b"occ skipped\n", True)


def test_tracked_route_is_fixed_after_prepare_even_if_path_becomes_ignored(
    tmp_path: Path,
) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "dist/out.js", b"leased\n")
    stale_snapshot = stack.read_active_manifest()
    _publish(stack, tmp_path, "dist/out.js", b"active\n")
    gitignore = _MutableGitignore()
    service = Service(gitignore=gitignore, snapshot_reader=stack, staging=stack, publisher=stack)

    prepared = service.prepare_changeset_sync(
        [
            WriteChange(
                path="dist/out.js",
                source="overlay_capture",
                final_content=b"stale shell\n",
            )
        ],
        snapshot=stale_snapshot,
    )
    [group] = prepared.path_groups
    assert group.route is RouteDecision.GATED

    gitignore.ignored.add("dist/out.js")
    result = CommitTransaction(
        snapshot_reader=stack,
        staging=stack,
        publisher=stack,
    ).revalidate_and_publish(prepared)

    assert _statuses(result) == [FileStatus.ABORTED_VERSION]
    assert result.published_manifest_version is None
    assert stack.read_bytes("dist/out.js") == (b"active\n", True)
