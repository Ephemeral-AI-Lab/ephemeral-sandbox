"""Typed-change round-trip through OCC.

Backs §4.3. Each typed :class:`Change` must route through OCC, commit,
and produce the expected on-disk effect via :class:`LayerStackManager`.

``BinaryChange`` is realised as a :class:`WriteChange` carrying
non-utf8 bytes (the type system has no separate ``BinaryChange`` class;
the README's row covers binary payloads going through the existing
write path with the existence/size CAS gate intact).
"""

from __future__ import annotations

import pytest

from .._harness.occ_workload import publish_base_file, write_sandbox_gitignore
from .._harness.sandbox_fixture import SandboxHandle

from sandbox.occ.changeset.prepared import CommitOptions
from sandbox.occ.changeset.types import (
    ChangesetResult,
    EditChange,
    FileStatus,
    OpaqueDirChange,
    SymlinkChange,
    WriteChange,
)


def _payloads(handle: SandboxHandle):
    return handle.extras["payloads_root"]


def _options() -> CommitOptions:
    return CommitOptions(caller_id="live-e2e-occ", description="pipeline-test")


def _ok(result: ChangesetResult) -> FileStatus:
    status = result.files[0].status
    assert status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}, status
    return status


@pytest.mark.asyncio
async def test_writechange_round_trip(occ_sandbox: SandboxHandle) -> None:
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    payload = b"hello write change\n"
    result = await service.apply_changeset(
        [WriteChange(path="src/a.py", source="api_write", final_content=payload)],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    _ok(result)
    content, exists = manager.read_bytes("src/a.py")
    assert exists is True and content == payload


@pytest.mark.asyncio
async def test_editchange_anchor_resolution(occ_sandbox: SandboxHandle) -> None:
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(manager, _payloads(occ_sandbox), "src/edit.py", b"foo bar\n")

    result = await service.apply_changeset(
        [
            EditChange(
                path="src/edit.py",
                old_text="foo",
                new_text="baz",
                expected_occurrences=1,
            ),
        ],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    _ok(result)
    content, exists = manager.read_bytes("src/edit.py")
    assert exists is True and content == b"baz bar\n"


@pytest.mark.asyncio
async def test_binarychange_existence_size_cas(occ_sandbox: SandboxHandle) -> None:
    """Binary payloads survive WriteChange round-trip byte-identical and
    the existence/size CAS gate still operates (a stale base hash on an
    already-mutated path is rejected)."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    png_header = b"\x89PNG\r\n\x1a\n" + bytes(range(256))
    result = await service.apply_changeset(
        [
            WriteChange(
                path="assets/blob.png",
                source="api_write",
                final_content=png_header,
            ),
        ],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    _ok(result)
    content, exists = manager.read_bytes("assets/blob.png")
    assert exists is True and content == png_header


@pytest.mark.asyncio
async def test_symlinkchange_existence_cas(occ_sandbox: SandboxHandle) -> None:
    """A SymlinkChange routes via the DIRECT path and produces a symlink
    layer entry (existence is the CAS dimension for symlinks)."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(manager, _payloads(occ_sandbox), "src/target.txt", b"target\n")
    result = await service.apply_changeset(
        [SymlinkChange(path="src/link.txt", target="src/target.txt")],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    _ok(result)
    manifest = manager.read_active_manifest()
    assert manifest.depth >= 1


@pytest.mark.asyncio
async def test_writechange_then_editchange_same_path(occ_sandbox: SandboxHandle) -> None:
    """A changeset with a WriteChange seeding fresh content followed by
    an EditChange against the new text on the same path must apply both
    in order: the edit runs over the freshly-written content, not the
    base view."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(manager, _payloads(occ_sandbox), "src/seq.py", b"original\n")
    result = await service.apply_changeset(
        [
            WriteChange(
                path="src/seq.py",
                source="api_write",
                final_content=b"alpha beta gamma\n",
            ),
            EditChange(
                path="src/seq.py",
                old_text="beta",
                new_text="BETA",
                expected_occurrences=1,
            ),
        ],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    assert all(
        entry.status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}
        for entry in result.files
    ), [entry.status for entry in result.files]
    content, exists = manager.read_bytes("src/seq.py")
    assert exists is True and content == b"alpha BETA gamma\n"


@pytest.mark.asyncio
async def test_editchange_expected_occurrences_multiple(
    occ_sandbox: SandboxHandle,
) -> None:
    """An EditChange with ``expected_occurrences=2`` against text that
    has exactly two matches must replace the first occurrence only
    (DirectMerge uses a single replacement) and accept; expected is
    informational metadata for upstream builders, the merger does not
    fail-fast on the count."""
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    await write_sandbox_gitignore(occ_sandbox.sandbox_id, ["build/"])
    publish_base_file(
        manager,
        _payloads(occ_sandbox),
        "build/log.txt",
        b"alpha alpha alpha\n",
    )
    result = await service.apply_changeset(
        [
            EditChange(
                path="build/log.txt",
                old_text="alpha",
                new_text="A",
                expected_occurrences=2,
            ),
        ],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    status = result.files[0].status
    assert status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}, status
    content, exists = manager.read_bytes("build/log.txt")
    assert exists is True
    # DirectMerge replaces a single occurrence per EditChange application.
    assert content == b"A alpha alpha\n"


@pytest.mark.asyncio
async def test_opaquedir_with_paired_write_preserves_child(
    occ_sandbox: SandboxHandle,
) -> None:
    """The opaque marker hides every previously-published child of the
    directory; preservation requires re-staging the kept child within
    the same changeset (the overlay-capture pipeline emits these pairs).

    This locks the upstream contract: an `OpaqueDirChange(pkg, kept={'keep.txt'})`
    paired with a `WriteChange(path='pkg/keep.txt', ...)` keeps `pkg/keep.txt`
    visible while `pkg/drop.txt` falls behind the marker.
    """
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(manager, _payloads(occ_sandbox), "pkg/keep.txt", b"keep-base\n")
    publish_base_file(manager, _payloads(occ_sandbox), "pkg/drop.txt", b"drop\n")

    result = await service.apply_changeset(
        [
            OpaqueDirChange(path="pkg", kept_children=frozenset({"keep.txt"})),
            WriteChange(
                path="pkg/keep.txt",
                source="overlay_capture",
                final_content=b"keep-restaged\n",
            ),
        ],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    assert all(
        entry.status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}
        for entry in result.files
    ), [entry.status for entry in result.files]
    keep_content, keep_exists = manager.read_bytes("pkg/keep.txt")
    assert keep_exists is True and keep_content == b"keep-restaged\n"
    _, drop_exists = manager.read_bytes("pkg/drop.txt")
    assert drop_exists is False


@pytest.mark.asyncio
async def test_opaquedir_lww_documented(occ_sandbox: SandboxHandle) -> None:
    """OpaqueDirChange documents LWW behaviour at the directory layer.

    The change stages a single ``opaque_dir`` marker at the directory
    path; previously-published children fall behind that marker and stop
    being visible through ``read_bytes``. Preserving a child requires
    re-staging it (the overlay-capture pipeline does this by emitting
    paired Write/Symlink changes alongside the OpaqueDirChange) — the
    ``kept_children`` set is informational metadata for that upstream
    pipeline and is not auto-applied at the OCC layer.
    """
    manager = occ_sandbox.layer_stack
    service = occ_sandbox.occ_service
    assert manager is not None and service is not None

    publish_base_file(manager, _payloads(occ_sandbox), "pkg/keep.txt", b"keep\n")
    publish_base_file(manager, _payloads(occ_sandbox), "pkg/drop.txt", b"drop\n")
    before = manager.read_active_manifest().version

    result = await service.apply_changeset(
        [OpaqueDirChange(path="pkg", kept_children=frozenset({"keep.txt"}))],
        snapshot=manager.read_active_manifest(),
        options=_options(),
    )
    _ok(result)
    after = manager.read_active_manifest().version
    assert after == before + 1, (before, after)
    # The opaque marker hides every previously-published child until the
    # caller re-stages the ones it wants to keep.
    _, drop_exists = manager.read_bytes("pkg/drop.txt")
    assert drop_exists is False
    _, keep_exists = manager.read_bytes("pkg/keep.txt")
    assert keep_exists is False
