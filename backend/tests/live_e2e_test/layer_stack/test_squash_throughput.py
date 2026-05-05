"""E5 — squash throughput pass bar.

Backs §4.1. Pass bar (from migration plan): depth stays in [40, 90] under
50/s for 5 min; ≤20 layers/s coalesce ratio. The 5-minute soak shape lives
in the integrated load suite (`test_load_profiles.py` :: `sustained`); the
cases here exercise the squash/backpressure contract that pass bar relies
on, scaled to a unit-test budget.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.layer_stack.publisher import CommitBackpressureError

from .._harness.assertions import (
    assert_manifest_depth_within,
    assert_manifest_layers_referenced_on_disk,
)
from .._harness.sandbox_fixture import SandboxHandle
from .._harness.thresholds import with_thresholds
from .._harness.workload import commit_layers, make_write_change


def test_sustained_50_commits_per_sec_keeps_depth_under_90(
    layer_stack_sandbox: SandboxHandle, tmp_path: Path
) -> None:
    """Periodic squash under sustained commits keeps depth bounded."""
    storage = layer_stack_sandbox.extras["storage_root"]
    payloads = tmp_path / "payloads"

    with with_thresholds(storage, MAX_DEPTH=20) as configured:
        manager = configured.manager
        burst = 25
        bursts = 8
        for _ in range(bursts):
            commit_layers(manager, payloads, burst)
            manager.squash(max_depth=configured.max_depth)
        assert_manifest_depth_within(manager, 1, configured.max_depth)
        final = manager.read_active_manifest()
        assert final.depth <= configured.max_depth
        assert_manifest_layers_referenced_on_disk(manager, final)


def test_emergency_depth_triggers_foreground_squash(
    layer_stack_sandbox: SandboxHandle, tmp_path: Path
) -> None:
    """At EMERGENCY_DEPTH the publisher backpressures; a foreground squash
    must let the next commit succeed."""
    storage = layer_stack_sandbox.extras["storage_root"]
    payloads = tmp_path / "payloads"

    with with_thresholds(storage, MAX_DEPTH=4, EMERGENCY_DEPTH=5) as configured:
        manager = configured.manager
        # Fill to the emergency depth.
        commit_layers(manager, payloads, 5)

        with pytest.raises(CommitBackpressureError):
            manager.publish_changes(
                [make_write_change(payloads, "overflow", "x\n")]
            )

        new = manager.squash(max_depth=configured.max_depth)
        assert new is not None
        assert new.depth <= configured.max_depth

        manager.publish_changes([make_write_change(payloads, "after", "y\n")])
        post = manager.read_active_manifest()
        assert post.depth <= configured.max_depth + 1
        assert_manifest_layers_referenced_on_disk(manager, post)


def test_no_backpressure_in_normal_load(
    layer_stack_sandbox: SandboxHandle, tmp_path: Path
) -> None:
    """A normal-shaped burst well under MAX_DEPTH must publish without raising."""
    storage = layer_stack_sandbox.extras["storage_root"]
    payloads = tmp_path / "payloads"

    with with_thresholds(storage, MAX_DEPTH=200, EMERGENCY_DEPTH=500) as configured:
        manager = configured.manager
        final = commit_layers(manager, payloads, 50)
        assert final.depth == 50
        # No squash needed — depth is well under MAX_DEPTH.
        assert manager.squash(max_depth=configured.max_depth) is None
        assert_manifest_layers_referenced_on_disk(manager, final)
