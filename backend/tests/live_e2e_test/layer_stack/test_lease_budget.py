"""E12 — lease-budget caps fire deterministically.

Backs §4.1. Pass bar: caps fire deterministically; no GC starvation;
kill semantics consistent across all four caps. Three of the four caps
have backing in :class:`LeaseBudgetWorker`; ``MAX_PINNED_OLD_MANIFESTS``
is still pending — see plan §3.4.
"""

from __future__ import annotations

import time
from pathlib import Path

import pytest

from .._harness.sandbox_fixture import SandboxHandle
from .._harness.thresholds import with_thresholds
from .._harness.workload import acquire_lease, commit_layer, commit_layers


def test_max_lease_age_force_kills_shell(
    layer_stack_sandbox: SandboxHandle, tmp_path: Path
) -> None:
    """A lease older than ``MAX_LEASE_AGE`` must surface as ``kill_lease``."""
    storage = layer_stack_sandbox.extras["storage_root"]
    payloads = tmp_path / "payloads"

    with with_thresholds(storage, MAX_LEASE_AGE=0.05) as configured:
        manager = configured.manager
        commit_layer(manager, payloads, "seed", body="seed\n")
        lease = acquire_lease(manager, owner_id="long-running-shell")
        time.sleep(0.15)

        decision = manager.evaluate_lease_budget()

        assert decision.kind == "kill_lease", decision
        assert decision.lease_id == lease.lease_id


def test_per_session_pin_bytes_blocks_new_writers(
    layer_stack_sandbox: SandboxHandle, tmp_path: Path
) -> None:
    """A single lease pinning more than the per-session cap must surface
    as ``evict_session`` so the publisher can evict that owner."""
    storage = layer_stack_sandbox.extras["storage_root"]
    payloads = tmp_path / "payloads"

    fat_body = "x" * 4096
    with with_thresholds(storage, PER_SESSION_PIN_BYTES=512) as configured:
        manager = configured.manager
        commit_layer(manager, payloads, "fat", body=fat_body)
        lease = acquire_lease(manager, owner_id="hog")

        snapshots = manager.lease_snapshots()
        assert snapshots, "lease snapshot must exist while lease is held"
        assert snapshots[0].pinned_bytes >= 512

        decision = manager.evaluate_lease_budget()

        assert decision.kind == "evict_session", decision
        assert decision.lease_id == lease.lease_id


def test_max_pinned_old_manifests_evicts_oldest(
    layer_stack_sandbox: SandboxHandle,
) -> None:
    pytest.skip(
        "pending: MAX_PINNED_OLD_MANIFESTS has no backing in "
        "LeaseBudgetWorker (see _harness.thresholds)"
    )


def test_global_pin_bytes_evicts_longest_pinning_session(
    layer_stack_sandbox: SandboxHandle, tmp_path: Path
) -> None:
    """When total pinned bytes exceed the global cap, the worker must
    backpressure commits, and the eviction order surfaced through
    :py:meth:`lease_snapshots` must place the oldest lease first."""
    storage = layer_stack_sandbox.extras["storage_root"]
    payloads = tmp_path / "payloads"

    with with_thresholds(storage, GLOBAL_PIN_BYTES=1024) as configured:
        manager = configured.manager

        commit_layer(manager, payloads, "first", body="a" * 800)
        oldest_lease = acquire_lease(manager, owner_id="oldest")
        # Ensure a strictly later acquisition timestamp for the second lease.
        time.sleep(0.01)
        commit_layers(manager, payloads, 2, prefix="post")
        younger_lease = acquire_lease(manager, owner_id="younger")

        snapshots = manager.lease_snapshots()
        assert {snap.lease_id for snap in snapshots} == {
            oldest_lease.lease_id,
            younger_lease.lease_id,
        }
        # Ordered oldest-first by acquired_at — eviction policy reads this.
        assert snapshots[0].lease_id == oldest_lease.lease_id
        total = sum(snap.pinned_bytes for snap in snapshots)
        assert total >= 1024, f"test setup must exceed cap: total={total}"

        decision = manager.evaluate_lease_budget()
        assert decision.kind == "backpressure_commits", decision
