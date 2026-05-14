"""Phase 04 OCC commit transaction tests."""

from __future__ import annotations

import asyncio
from pathlib import Path

from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.prepared import CommitOptions
from sandbox.occ.changeset.types import ChangesetResult, FileStatus, WriteChange
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.service import OccService


class _Gitignore:
    def __init__(self, ignored: set[str] | None = None) -> None:
        self._ignored = ignored or set()

    def is_ignored(self, path: str) -> bool:
        return path in self._ignored

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
) -> OccService:
    return OccService(gitignore=_Gitignore(ignored), layer_stack=stack)


def test_apply_changeset_publishes_accepted_tracked_layer(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "src/app.py", b"old\n")
    snapshot = stack.read_active_manifest()

    result = asyncio.run(
        _service(stack).apply_changeset(
            [WriteChange(path="src/app.py", final_content=b"new\n")],
            snapshot=snapshot,
        )
    )

    assert isinstance(result, ChangesetResult)
    assert result.files[0].status is FileStatus.ACCEPTED
    assert result.published_manifest_version == 2
    assert stack.read_bytes("src/app.py") == (b"new\n", True)


def test_stale_tracked_base_hash_aborts_without_publishing(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "src/app.py", b"leased\n")
    snapshot = stack.read_active_manifest()
    _publish(stack, tmp_path, "src/app.py", b"active\n")

    result = asyncio.run(
        _service(stack).apply_changeset(
            [WriteChange(path="src/app.py", final_content=b"new\n")],
            snapshot=snapshot,
        )
    )

    assert isinstance(result, ChangesetResult)
    assert result.files[0].status is FileStatus.ABORTED_VERSION
    assert result.published_manifest_version is None
    assert stack.read_active_manifest().version == 2
    assert stack.read_bytes("src/app.py") == (b"active\n", True)


def test_drop_and_reject_return_file_results_without_publishing(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")

    result = asyncio.run(
        _service(stack).apply_changeset(
            [
                WriteChange(path=".git/config", final_content=b"x"),
                WriteChange(path="../escape", final_content=b"x"),
            ],
            snapshot=stack.read_active_manifest(),
        )
    )

    assert isinstance(result, ChangesetResult)
    assert [file.status for file in result.files] == [
        FileStatus.DROPPED,
        FileStatus.REJECTED,
    ]
    assert result.published_manifest_version is None
    assert stack.read_active_manifest().version == 0


def test_atomic_option_suppresses_otherwise_accepted_paths_on_failure(
    tmp_path: Path,
) -> None:
    stack = LayerStackManager(tmp_path / "stack")

    result = asyncio.run(
        _service(stack).apply_changeset(
            [
                WriteChange(path="src/app.py", final_content=b"x"),
                WriteChange(path="../escape", final_content=b"x"),
            ],
            snapshot=stack.read_active_manifest(),
            options=CommitOptions(atomic=True),
        )
    )

    assert isinstance(result, ChangesetResult)
    assert [file.status for file in result.files] == [
        FileStatus.DROPPED,
        FileStatus.REJECTED,
    ]
    assert result.published_manifest_version is None
    assert stack.read_bytes("src/app.py") == (None, False)
