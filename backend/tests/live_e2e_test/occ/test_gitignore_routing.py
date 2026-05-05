"""E13 — gitignore-driven CAS/LWW routing.

Backs §4.3. Pass bar: zero classification leaks; gitignored paths never
CAS-rejected; tracked paths never LWW-accepted.
"""

from __future__ import annotations

import pytest

from .._harness.assertions import assert_classification_pure
from .._harness.occ_workload import publish_base_file, write_sandbox_gitignore
from .._harness.sandbox_fixture import SandboxHandle

from sandbox.occ.changeset.prepared import CommitOptions
from sandbox.occ.changeset.types import (
    ChangesetResult,
    EditChange,
    FileStatus,
    WriteChange,
)
from sandbox.occ.content.hashing import ContentHasher


def _payloads(handle: SandboxHandle):
    return handle.extras["payloads_root"]


def _options() -> CommitOptions:
    return CommitOptions(caller_id="live-e2e-occ", description="gitignore-test")


def _captures(result: ChangesetResult, route_for: dict[str, str]) -> list[dict]:
    return [
        {
            "path": entry.path,
            "route": route_for.get(entry.path, "tracked"),
            "status": entry.status.value,
        }
        for entry in result.files
    ]


@pytest.mark.asyncio
async def test_tracked_path_uses_cas(occ_sandbox: SandboxHandle) -> None:
    """A write to a tracked path with a stale base hash must be rejected
    by the CAS gate."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    # Empty .gitignore: every path is tracked.
    await write_sandbox_gitignore(occ_sandbox.sandbox_id, [])
    publish_base_file(manager, _payloads(occ_sandbox), "src/tracked.py", b"v1\n")
    snapshot = manager.read_active_manifest()
    # Now publish a competing v2 so the snapshot above goes stale.
    publish_base_file(manager, _payloads(occ_sandbox), "src/tracked.py", b"v2\n")

    result = await service.apply_changeset(
        [
            WriteChange(
                path="src/tracked.py",
                source="overlay_capture",
                final_content=b"v3-from-stale-snapshot\n",
                base_hash=ContentHasher().hash_bytes(b"v1\n"),
            ),
        ],
        snapshot=snapshot,
        options=_options(),
    )

    assert result.files[0].status is FileStatus.ABORTED_VERSION


@pytest.mark.asyncio
async def test_gitignored_path_uses_lww(occ_sandbox: SandboxHandle) -> None:
    """A write to a gitignored path must be DIRECT-routed and accept
    regardless of base-hash freshness."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    await write_sandbox_gitignore(occ_sandbox.sandbox_id, ["build/"])
    publish_base_file(manager, _payloads(occ_sandbox), "build/out.txt", b"old\n")
    snapshot = manager.read_active_manifest()
    publish_base_file(manager, _payloads(occ_sandbox), "build/out.txt", b"newer\n")

    result = await service.apply_changeset(
        [
            WriteChange(
                path="build/out.txt",
                source="overlay_capture",
                final_content=b"lww-wins\n",
                base_hash=ContentHasher().hash_bytes(b"old\n"),
            ),
        ],
        snapshot=snapshot,
        options=_options(),
    )

    status = result.files[0].status
    assert status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}, status
    content, exists = manager.read_bytes("build/out.txt")
    assert exists is True
    assert content == b"lww-wins\n"


@pytest.mark.asyncio
async def test_mixed_changeset_partial_commit(occ_sandbox: SandboxHandle) -> None:
    """A changeset blending one tracked-stale path with one gitignored
    path must accept the gitignored one and reject the tracked one;
    ``assert_classification_pure`` must hold over the captures."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    await write_sandbox_gitignore(occ_sandbox.sandbox_id, ["dist/"])
    publish_base_file(manager, _payloads(occ_sandbox), "src/tracked.py", b"v1\n")
    publish_base_file(manager, _payloads(occ_sandbox), "dist/asset.bin", b"old-bin\n")
    snapshot = manager.read_active_manifest()
    publish_base_file(manager, _payloads(occ_sandbox), "src/tracked.py", b"v2\n")

    # Use api_write so the partial-commit path is exercised. (overlay_capture
    # is intentionally all-or-nothing — see _is_overlay_capture_changeset in
    # commit_transaction.py — and would demote the gitignored side to DROPPED
    # if the tracked side fails.)
    result = await service.apply_changeset(
        [
            WriteChange(
                path="src/tracked.py",
                source="api_write",
                final_content=b"v3-stale\n",
                base_hash=ContentHasher().hash_bytes(b"v1\n"),
            ),
            WriteChange(
                path="dist/asset.bin",
                source="api_write",
                final_content=b"new-bin\n",
                base_hash=ContentHasher().hash_bytes(b"old-bin\n"),
            ),
        ],
        snapshot=snapshot,
        options=_options(),
    )

    statuses = {entry.path: entry.status for entry in result.files}
    assert statuses["src/tracked.py"] is FileStatus.ABORTED_VERSION
    assert statuses["dist/asset.bin"] in {FileStatus.ACCEPTED, FileStatus.COMMITTED}

    routes = {"src/tracked.py": "tracked", "dist/asset.bin": "direct"}
    assert_classification_pure(_captures(result, routes))


@pytest.mark.asyncio
async def test_editchange_on_gitignored_path_routes_direct(
    occ_sandbox: SandboxHandle,
) -> None:
    """An EditChange against a gitignored path must take the DIRECT
    route (no CAS gate) and apply via the direct merger's in-place
    string-replace branch.
    """
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    await write_sandbox_gitignore(occ_sandbox.sandbox_id, ["dist/"])
    publish_base_file(
        manager,
        _payloads(occ_sandbox),
        "dist/manifest.json",
        b'{"key": "old"}\n',
    )
    result = await service.apply_changeset(
        [
            EditChange(
                path="dist/manifest.json",
                old_text='"old"',
                new_text='"NEW"',
                expected_occurrences=1,
            ),
        ],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    status = result.files[0].status
    assert status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}, status
    content, exists = manager.read_bytes("dist/manifest.json")
    assert exists is True and content == b'{"key": "NEW"}\n'


@pytest.mark.asyncio
async def test_overlay_capture_changeset_demotes_partial_commit(
    occ_sandbox: SandboxHandle,
) -> None:
    """When ANY change in the batch carries ``source='overlay_capture'``
    and the tracked side fails, the entire publish is skipped — the
    gitignored side that would otherwise ACCEPT must be demoted to
    DROPPED (`commit_transaction.py::_is_overlay_capture_changeset`).

    This is the all-or-nothing semantic the overlay-capture pipeline
    relies on: a torn capture must not produce a half-applied manifest.
    """
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    await write_sandbox_gitignore(occ_sandbox.sandbox_id, ["cache/"])
    publish_base_file(manager, _payloads(occ_sandbox), "src/code.py", b"v1\n")
    publish_base_file(manager, _payloads(occ_sandbox), "cache/blob.bin", b"old\n")
    snapshot = manager.read_active_manifest()
    publish_base_file(manager, _payloads(occ_sandbox), "src/code.py", b"v2\n")

    result = await service.apply_changeset(
        [
            WriteChange(
                path="src/code.py",
                source="overlay_capture",
                final_content=b"stale-overlay\n",
                base_hash=ContentHasher().hash_bytes(b"v1\n"),
            ),
            WriteChange(
                path="cache/blob.bin",
                source="overlay_capture",
                final_content=b"would-have-accepted\n",
            ),
        ],
        snapshot=snapshot,
        options=_options(),
    )

    statuses = {entry.path: entry.status for entry in result.files}
    assert statuses["src/code.py"] is FileStatus.ABORTED_VERSION
    # Demotion: the gitignored side cannot be published when any tracked
    # path in an overlay-capture changeset failed.
    assert statuses["cache/blob.bin"] is FileStatus.DROPPED
    # Manifest must not advance (publish was skipped).
    after = manager.read_active_manifest().version
    assert after == snapshot.version + 1, (snapshot.version, after)
    # cache/blob.bin remains at its base content.
    content, exists = manager.read_bytes("cache/blob.bin")
    assert exists is True and content == b"old\n"


@pytest.mark.asyncio
async def test_gitignore_evaluated_at_snapshot_time(occ_sandbox: SandboxHandle) -> None:
    """Routing must reflect the .gitignore present when the OCC service
    runs ``check-ignore``: switching the ignore pattern between two
    changesets must change the route the second time around."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    await write_sandbox_gitignore(occ_sandbox.sandbox_id, [])  # nothing ignored yet
    publish_base_file(manager, _payloads(occ_sandbox), "logs/app.log", b"v0\n")
    publish_base_file(manager, _payloads(occ_sandbox), "logs/app.log", b"v1\n")

    # First commit — tracked, stale base hash → must be rejected.
    first_snapshot = manager.read_active_manifest()
    publish_base_file(manager, _payloads(occ_sandbox), "logs/app.log", b"v2\n")
    first = await service.apply_changeset(
        [
            WriteChange(
                path="logs/app.log",
                source="overlay_capture",
                final_content=b"stale-attempt\n",
                base_hash=ContentHasher().hash_bytes(b"v0\n"),
            ),
        ],
        snapshot=first_snapshot,
        options=_options(),
    )
    assert first.files[0].status is FileStatus.ABORTED_VERSION

    # Now ignore logs/ and re-attempt with the same kind of payload — the
    # oracle's cache is per-instance so a fresh service confirms the new
    # rule reaches the route. ``GitignoreOracle`` caches per-path, so we
    # rebuild it via a fresh instance to ensure snapshot-time evaluation.
    await write_sandbox_gitignore(occ_sandbox.sandbox_id, ["logs/"])
    fresh_oracle = occ_sandbox.extras["gitignore_oracle"].__class__(
        "/testbed",
        run=occ_sandbox.extras["gitignore_run_fn"],
    )
    from sandbox.occ.service import OccService

    fresh_service = OccService(gitignore=fresh_oracle, layer_stack=manager)
    second_snapshot = manager.read_active_manifest()
    publish_base_file(manager, _payloads(occ_sandbox), "logs/app.log", b"v3\n")
    second = await fresh_service.apply_changeset(
        [
            WriteChange(
                path="logs/app.log",
                source="overlay_capture",
                final_content=b"lww-after-ignore\n",
                base_hash=ContentHasher().hash_bytes(b"v0\n"),
            ),
        ],
        snapshot=second_snapshot,
        options=_options(),
    )
    assert second.files[0].status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}
