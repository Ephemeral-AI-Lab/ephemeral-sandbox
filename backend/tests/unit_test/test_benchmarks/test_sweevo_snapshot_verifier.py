"""Tests for the sweevo snapshot verifier.

``verify_sweevo_snapshot_exists`` is a fail-fast probe the CLI calls
before any sandbox is created so a missing snapshot surfaces before a
long agent run. Post-migration, the benchmark is docker-only and the
docker provider has no inactive/error state to normalize against —
presence-in-list is the sole acceptance criterion.
"""

from __future__ import annotations

import pytest

import sandbox.api as sandbox_api
from task_center_runner.benchmarks.sweevo._snapshot import (
    SnapshotNotRegisteredError,
    verify_sweevo_snapshot_exists,
)
from task_center_runner.benchmarks.sweevo.models import (
    SWEEvoInstance,
    default_sweevo_snapshot_name,
)


def _instance(instance_id: str = "dask__dask_2023.3.2_2023.4.0") -> SWEEvoInstance:
    return SWEEvoInstance(
        instance_id=instance_id,
        repo="dask/dask",
        base_commit="abc",
        problem_statement="",
        patch="",
        fail_to_pass=[],
        pass_to_pass=[],
        docker_image="sweevo/dask:abc",
        test_cmds="pytest",
        environment_setup_commit="",
    )


def test_verify_returns_name_when_snapshot_present(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    inst = _instance()
    expected = default_sweevo_snapshot_name(inst)
    monkeypatch.setattr(
        sandbox_api,
        "list_snapshots",
        lambda: [{"name": expected}, {"name": "other"}],
    )

    assert verify_sweevo_snapshot_exists(inst) == expected


def test_verify_raises_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    inst = _instance()
    expected = default_sweevo_snapshot_name(inst)
    monkeypatch.setattr(
        sandbox_api,
        "list_snapshots",
        lambda: [{"name": "unrelated"}],
    )

    with pytest.raises(SnapshotNotRegisteredError) as exc_info:
        verify_sweevo_snapshot_exists(inst)

    message = str(exc_info.value)
    assert expected in message
    assert inst.instance_id in message
    assert "register_sweevo_snapshot" in message
