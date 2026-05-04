"""Layer-stack fsck cleanup tests."""

from __future__ import annotations

import os
from pathlib import Path

from sandbox.layer_stack import LayerStackManager


def test_fsck_removes_orphan_layers_and_old_staging(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    orphan_layer = manager.storage_root / "layers" / "orphan"
    orphan_layer.mkdir()
    old_staging = manager.storage_root / "staging" / "old.staging"
    old_staging.mkdir()
    young_staging = manager.storage_root / "staging" / "young.staging"
    young_staging.mkdir()
    os.utime(old_staging, (0, 0))
    os.utime(young_staging, (95, 95))

    result = manager.fsck_cleanup(young_staging_age_seconds=10, now=100)

    assert result.orphan_layers_removed == ("orphan",)
    assert result.orphan_staging_removed == ("old.staging",)
    assert not orphan_layer.exists()
    assert not old_staging.exists()
    assert young_staging.is_dir()
