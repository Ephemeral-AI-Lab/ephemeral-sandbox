from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.layer_stack.commit_staging import (
    allocate_commit_staging,
    drop_commit_staging,
)


def test_drop_commit_staging_rejects_empty_staging_id(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="staging_id must not be empty"):
        drop_commit_staging(tmp_path / "stack", "")


def test_drop_commit_staging_removes_allocated_directory(tmp_path: Path) -> None:
    stack = tmp_path / "stack"
    area = allocate_commit_staging(stack, "request/a")

    assert area.path.is_dir()

    drop_commit_staging(stack, area.staging_id)

    assert not area.path.exists()
