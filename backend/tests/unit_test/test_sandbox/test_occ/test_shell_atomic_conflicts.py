"""Shell-captured changes publish atomically when OCC-gated validation conflicts."""

from __future__ import annotations

import asyncio
from pathlib import Path

from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.types import FileStatus, WriteChange
from sandbox.occ.content.hashing import ContentHasher
from sandbox.occ.service import OccService


class _Gitignore:
    def is_ignored(self, path: str) -> bool:
        return path == "dist/out.txt"


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


def test_shell_occ_gated_conflict_holds_occ_skipped_outputs(tmp_path: Path) -> None:
    stack = LayerStackManager(tmp_path / "stack")
    _publish(stack, tmp_path, "src/app.py", b"leased\n")
    snapshot = stack.read_active_manifest()
    _publish(stack, tmp_path, "src/app.py", b"active\n")
    service = OccService(gitignore=_Gitignore(), layer_stack=stack)

    result = asyncio.run(
        service.apply_changeset(
            [
                WriteChange(
                    path="src/app.py",
                    source="overlay_capture",
                    final_content=b"tracked shell\n",
                ),
                WriteChange(
                    path="dist/out.txt",
                    source="overlay_capture",
                    final_content=b"occ skipped shell\n",
                ),
            ],
            snapshot=snapshot,
        )
    )

    assert [file.status for file in result.files] == [
        FileStatus.ABORTED_VERSION,
        FileStatus.DROPPED,
    ]
    assert result.published_manifest_version is None
    assert stack.read_bytes("src/app.py") == (b"active\n", True)
    assert stack.read_bytes("dist/out.txt") == (None, False)
