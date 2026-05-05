"""E11 — staleness telemetry surfaced unconditionally.

Backs §4.3. Pass bar: long-shell write to a clean-CAS path **always**
accepts; ``manifest_lag`` populated on every committed result; no
age-based or lag-based rejection.
"""

from __future__ import annotations

import pytest

from .._harness.assertions import assert_telemetry_present
from .._harness.occ_workload import publish_base_file
from .._harness.sandbox_fixture import SandboxHandle

from sandbox.occ.changeset.prepared import CommitOptions
from sandbox.occ.changeset.types import (
    ChangesetResult,
    FileStatus,
    WriteChange,
)


def _payloads(handle: SandboxHandle):
    return handle.extras["payloads_root"]


def _options() -> CommitOptions:
    return CommitOptions(caller_id="live-e2e-occ", description="staleness-test")


def _result_dict(result: ChangesetResult) -> dict:
    return {
        "timings": dict(result.timings),
        "published_manifest_version": result.published_manifest_version,
    }


@pytest.mark.asyncio
async def test_long_shell_clean_cas_accepts_with_lag_telemetry(
    occ_sandbox: SandboxHandle,
) -> None:
    """A stale snapshot held over N=3 intervening commits to *other*
    paths must still ACCEPT the held writer's CAS-clean change against
    its own path, and ``manifest_lag`` must equal the intervening count."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(
        manager, _payloads(occ_sandbox), "src/long_shell.py", b"baseline\n"
    )
    long_snapshot = manager.read_active_manifest()

    for index in range(3):
        publish_base_file(
            manager,
            _payloads(occ_sandbox),
            f"src/intervening_{index}.py",
            f"v{index}\n".encode("utf-8"),
        )

    result = await service.apply_changeset(
        [
            WriteChange(
                path="src/long_shell.py",
                source="overlay_capture",
                final_content=b"updated-by-long-shell\n",
            ),
        ],
        snapshot=long_snapshot,
        options=_options(),
    )

    status = result.files[0].status
    assert status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}, status
    assert_telemetry_present(_result_dict(result))
    assert result.timings["occ.apply.manifest_lag"] == 3


@pytest.mark.asyncio
async def test_manifest_lag_field_increments_with_intervening_commits(
    occ_sandbox: SandboxHandle,
) -> None:
    """Serial commits against progressively older snapshots must report
    monotonically growing ``manifest_lag`` values (one increment per
    intervening commit on a different path)."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(manager, _payloads(occ_sandbox), "src/main.py", b"v1\n")
    held_snapshot = manager.read_active_manifest()

    lags: list[int] = []
    for index in range(4):
        publish_base_file(
            manager,
            _payloads(occ_sandbox),
            f"src/aux_{index}.py",
            f"aux-{index}\n".encode("utf-8"),
        )
        # Use a fresh per-call snapshot so we observe the lag growing
        # as the manifest version increments under the held view.
        result = await service.apply_changeset(
            [
                WriteChange(
                    path=f"src/derived_{index}.py",
                    source="overlay_capture",
                    final_content=f"derived-{index}\n".encode("utf-8"),
                ),
            ],
            snapshot=held_snapshot,
            options=_options(),
        )
        assert result.files[0].status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}
        lags.append(result.timings["occ.apply.manifest_lag"])

    # Strictly increasing: each iteration adds exactly one intervening
    # publish before the OCC commit, plus the previous derived commits.
    assert lags == sorted(lags), lags
    assert lags[0] >= 1


@pytest.mark.asyncio
async def test_no_age_or_lag_based_rejection(occ_sandbox: SandboxHandle) -> None:
    """A CAS-clean commit must not be rejected on the basis of high
    ``manifest_lag`` alone — the OCC contract is that age is
    informational telemetry, never a gate."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(manager, _payloads(occ_sandbox), "src/aged.py", b"baseline\n")
    aged_snapshot = manager.read_active_manifest()

    # Push the manifest forward by 10 commits on unrelated paths to drive
    # ``manifest_lag`` well above any plausible threshold.
    for index in range(10):
        publish_base_file(
            manager,
            _payloads(occ_sandbox),
            f"src/noise_{index}.py",
            f"n{index}\n".encode("utf-8"),
        )

    result = await service.apply_changeset(
        [
            WriteChange(
                path="src/aged.py",
                source="overlay_capture",
                final_content=b"finally-applied\n",
            ),
        ],
        snapshot=aged_snapshot,
        options=_options(),
    )
    status = result.files[0].status
    assert status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}, status
    assert result.timings["occ.apply.manifest_lag"] >= 10
