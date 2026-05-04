"""Concurrent Phase 04 commit transaction behavior."""

from __future__ import annotations

import asyncio
from pathlib import Path

from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.occ.changeset.types import FileStatus, WriteChange
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.commit_transaction import OccCommitTransaction
from sandbox.occ.service import OccService


class _Gitignore:
    def is_ignored(self, path: str) -> bool:
        del path
        return False


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


def test_concurrent_prepared_commits_revalidate_latest_manifest(
    tmp_path: Path,
) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "src/app.py", b"base\n")
    snapshot = stack.read_active_manifest()
    service = OccService(gitignore=_Gitignore(), layer_stack=stack)

    async def run_commit(index: int):
        prepared = await service.prepare_changeset(
            [
                WriteChange(
                    path="src/app.py",
                    source="shell_capture",
                    final_content=f"agent-{index}\n".encode("utf-8"),
                )
            ],
            snapshot=snapshot,
        )
        transaction = OccCommitTransaction(stack)
        return await asyncio.to_thread(transaction.revalidate_and_publish, prepared)

    async def run_all():
        return await asyncio.gather(*(run_commit(index) for index in range(10)))

    results = asyncio.run(run_all())
    statuses = [result.files[0].status for result in results]

    assert statuses.count(FileStatus.ACCEPTED) == 1
    assert statuses.count(FileStatus.ABORTED_VERSION) == 9
    assert stack.read_active_manifest().version == 2
    content, exists = stack.read_bytes("src/app.py")
    assert exists is True
    assert content in {f"agent-{index}\n".encode("utf-8") for index in range(10)}
