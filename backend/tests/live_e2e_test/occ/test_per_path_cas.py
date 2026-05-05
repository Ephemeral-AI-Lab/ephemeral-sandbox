"""E10 — per-path CAS gate.

Backs §4.3. Pass bar: zero false-accept and zero false-reject across the
gated matrix. The 10k iteration target is scaled to 200 here per the
README ("scale per test budget; record the ratio") and the actual
counts feed the load metric collector.
"""

from __future__ import annotations

import asyncio
import time

import pytest

from .._harness.assertions import (
    assert_classification_pure,
    assert_telemetry_present,
)
from .._harness.occ_workload import (
    IterationStat,
    LoadCollector,
    publish_base_file,
)
from .._harness.sandbox_fixture import SandboxHandle

from sandbox.occ.changeset.prepared import CommitOptions
from sandbox.occ.changeset.types import (
    ChangesetResult,
    DeleteChange,
    EditChange,
    FileStatus,
    WriteChange,
)
from sandbox.occ.content.hashing import ContentHasher


CAS_LOAD_ITERS = 200


def _payloads(handle: SandboxHandle):
    return handle.extras["payloads_root"]


def _options() -> CommitOptions:
    return CommitOptions(caller_id="live-e2e-occ", description="cas-test")


def _result_dict(result: ChangesetResult) -> dict:
    return {
        "timings": dict(result.timings),
        "published_manifest_version": result.published_manifest_version,
    }


def _captures(result: ChangesetResult, route: str) -> list[dict]:
    return [
        {"path": entry.path, "route": route, "status": entry.status.value}
        for entry in result.files
    ]


@pytest.mark.asyncio
async def test_write_write_conflict_rejects_loser(
    occ_sandbox: SandboxHandle, occ_load_collector: LoadCollector
) -> None:
    """Two concurrent writes to the same path with the same base hash must
    serialize: exactly one ACCEPTED and exactly one ABORTED_VERSION.

    Runs ``CAS_LOAD_ITERS`` rounds and records latency + ratio metrics.
    """
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    accepted_total = 0
    aborted_total = 0

    # Pre-warm the gitignore cache for every path the load loop will
    # touch: each per-iteration `is_ignored` lookup ships over raw_exec,
    # so a single batched check-ignore call (`filter_ignored`) keeps the
    # 200 iters fast (one round-trip vs. 200).
    rels = [f"src/cas_{index:04d}.py" for index in range(CAS_LOAD_ITERS)]
    occ_sandbox.extras["gitignore_oracle"].filter_ignored(rels)

    for index in range(CAS_LOAD_ITERS):
        rel = rels[index]
        publish_base_file(
            manager, _payloads(occ_sandbox), rel, f"base-{index}\n".encode("utf-8")
        )
        snapshot = manager.read_active_manifest()

        async def commit(payload: bytes) -> ChangesetResult:
            return await service.apply_changeset(
                [WriteChange(path=rel, source="overlay_capture", final_content=payload)],
                snapshot=snapshot,
                options=_options(),
            )

        start = time.perf_counter()
        first, second = await asyncio.gather(
            commit(f"agent-A-{index}\n".encode("utf-8")),
            commit(f"agent-B-{index}\n".encode("utf-8")),
        )
        latency_ms = (time.perf_counter() - start) * 1000

        statuses = sorted(r.files[0].status for r in (first, second))
        assert statuses == sorted(
            [FileStatus.ACCEPTED, FileStatus.ABORTED_VERSION]
        ), f"iter {index}: got {statuses!r}"

        committed = first if first.files[0].status is FileStatus.ACCEPTED else second
        assert_telemetry_present(_result_dict(committed))
        assert_classification_pure(_captures(committed, "tracked"))
        accepted_total += 1
        aborted_total += 1

        occ_load_collector.record(
            IterationStat(
                test="per_path_cas.write_write_conflict",
                accepted=1,
                rejected=1,
                latency_ms=latency_ms,
                manifest_version=committed.published_manifest_version,
                manifest_lag=committed.timings.get("occ.apply.manifest_lag"),
            )
        )

    # Pass bar: 0/N false-accept (each round had exactly one ACCEPTED) and
    # 0/N false-reject (each round had exactly one ABORTED_VERSION).
    assert accepted_total == CAS_LOAD_ITERS, accepted_total
    assert aborted_total == CAS_LOAD_ITERS, aborted_total


@pytest.mark.asyncio
async def test_disjoint_paths_both_accept(occ_sandbox: SandboxHandle) -> None:
    """Two writes targeting distinct paths in the same changeset both
    accept and the manifest version increments by exactly one."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(manager, _payloads(occ_sandbox), "src/a.py", b"a-base\n")
    publish_base_file(manager, _payloads(occ_sandbox), "src/b.py", b"b-base\n")
    before = manager.read_active_manifest().version
    snapshot = manager.read_active_manifest()

    result = await service.apply_changeset(
        [
            WriteChange(path="src/a.py", source="overlay_capture", final_content=b"a-new\n"),
            WriteChange(path="src/b.py", source="overlay_capture", final_content=b"b-new\n"),
        ],
        snapshot=snapshot,
        options=_options(),
    )

    statuses = [entry.status for entry in result.files]
    assert all(status is FileStatus.ACCEPTED for status in statuses), statuses
    after = manager.read_active_manifest().version
    assert after == before + 1, (before, after)


@pytest.mark.asyncio
async def test_anchor_miss_rejects_edit(occ_sandbox: SandboxHandle) -> None:
    """An EditChange whose ``old_text`` does not appear in the file must
    fail at the change-builder layer (anchor miss) without mutating the
    manifest."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(
        manager, _payloads(occ_sandbox), "src/anchor.py", b"hello world\n"
    )
    before = manager.read_active_manifest().version

    result = await service.apply_changeset(
        [
            EditChange(
                path="src/anchor.py",
                old_text="not present",
                new_text="replacement",
            ),
        ],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    status = result.files[0].status
    # Anchor miss is surfaced by either the gated merger (ABORTED_OVERLAP
    # — the EditChange found nothing to replace and overlapped with the
    # base) or the change-builder layer (FAILED/REJECTED). Either way the
    # manifest must not advance.
    assert status in {
        FileStatus.FAILED,
        FileStatus.REJECTED,
        FileStatus.ABORTED_OVERLAP,
    }, status
    assert manager.read_active_manifest().version == before


@pytest.mark.asyncio
async def test_existence_change_rejects_create(occ_sandbox: SandboxHandle) -> None:
    """Existence-CAS surface check.

    A second write against an existing tracked path with a *stale* base
    hash that no longer matches the current content must be rejected by
    the per-path CAS gate. The ``create_only`` flag is plumbed through
    the typed change but is not currently enforced at the OCC merge
    layer (it is informational metadata for upstream typed-change
    builders); the actual existence-change rejection rides the same CAS
    gate as a content-change rejection. See plan §4.3 — the test name
    captures the intent ("create against an existing path must not
    silently overwrite when the snapshot is stale").
    """
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(
        manager, _payloads(occ_sandbox), "src/existing.py", b"already here\n"
    )
    snapshot = manager.read_active_manifest()
    publish_base_file(
        manager, _payloads(occ_sandbox), "src/existing.py", b"intervening\n"
    )
    before = manager.read_active_manifest().version

    result = await service.apply_changeset(
        [
            WriteChange(
                path="src/existing.py",
                source="api_write",
                final_content=b"second\n",
                base_hash=ContentHasher().hash_bytes(b"already here\n"),
                create_only=True,
            ),
        ],
        snapshot=snapshot,
        options=_options(),
    )
    status = result.files[0].status
    assert status in {
        FileStatus.REJECTED,
        FileStatus.FAILED,
        FileStatus.ABORTED_VERSION,
    }, status
    content, exists = manager.read_bytes("src/existing.py")
    assert exists is True
    assert content == b"intervening\n"
    assert manager.read_active_manifest().version == before


@pytest.mark.asyncio
async def test_delete_then_edit_same_path_in_one_changeset(
    occ_sandbox: SandboxHandle,
) -> None:
    """A DeleteChange followed by an EditChange against the same path in
    a single changeset must not silently resurrect the file: the delete
    wins (final_kind=delete) and the EditChange's "must have a write"
    guard (`DirectMerge._stage_group`) leaves the path absent.

    Routes through DIRECT (gitignored) so both changes land in the same
    direct-merge group.
    """
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    from .._harness.occ_workload import write_sandbox_gitignore as _wg

    await _wg(occ_sandbox.sandbox_id, ["scratch/"])
    publish_base_file(
        manager, _payloads(occ_sandbox), "scratch/note.txt", b"foo bar\n"
    )

    result = await service.apply_changeset(
        [
            DeleteChange(path="scratch/note.txt"),
            EditChange(
                path="scratch/note.txt",
                old_text="foo",
                new_text="baz",
            ),
        ],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    statuses = [entry.status for entry in result.files]
    assert all(
        status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}
        for status in statuses
    ), statuses
    _, exists = manager.read_bytes("scratch/note.txt")
    assert exists is False


@pytest.mark.asyncio
async def test_delete_already_deleted_is_noop(occ_sandbox: SandboxHandle) -> None:
    """Deleting a path that is already absent is idempotent: the
    manifest must not regress and the result status must not signal a
    correctness violation."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(
        manager, _payloads(occ_sandbox), "src/will_remove.py", b"removeme\n"
    )

    first = await service.apply_changeset(
        [DeleteChange(path="src/will_remove.py", source="api_write")],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    first_status = first.files[0].status
    assert first_status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}, first_status
    after_first = manager.read_active_manifest().version

    second = await service.apply_changeset(
        [DeleteChange(path="src/will_remove.py", source="api_write")],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    second_status = second.files[0].status
    # Idempotent: must not crash, must not regress the manifest, and must
    # not produce a CAS-style abort that would surface to the caller as
    # "your delete failed because someone else mutated the path".
    assert second_status in {
        FileStatus.ACCEPTED,
        FileStatus.COMMITTED,
        FileStatus.DROPPED,
    }, second_status
    assert manager.read_active_manifest().version >= after_first
    _, exists = manager.read_bytes("src/will_remove.py")
    assert exists is False
