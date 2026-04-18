"""P0.9 — Overlay lowerdir OCC prototype (pure local filesystem).

Exercises the four properties the Phase 3 OCC-gated ``OverlayAuditor`` must
satisfy. The real auditor uses a kernel overlayfs mount; this prototype
simulates the same logical steps with plain directory snapshots so it runs on
any POSIX host without root.

Properties validated (see plan §4 P0.9):

  1. Drift detection:      strict_base aborts when a peer mutates the workspace
                           between snapshot and commit. Disk unchanged.
  2. Untracked preserved:  scratch.txt in the workspace is neither lost nor
                           flagged as a conflict by an unrelated commit.
  3. Dirty-file base:      uncommitted edits form the base content for OCC,
                           not HEAD.
  4. Symlink rejected:     D3a — unsupported upperdir change kinds abort the
                           whole run; disk unchanged.

Run:

  .venv/bin/python -m pytest backend/tests/experiments/test_p09_overlay_lowerdir_prototype.py -v

Findings feed the Phase 3 OverlayAuditor spec. If any property fails under the
``cp -al`` lowerdir approach, escalate D2/P0.7 to an alternate snapshot
mechanism before starting Phase 3.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path

import pytest


# --- Minimal OCC / overlay simulator -----------------------------------------


def _hash_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


def _read_if_exists(path: Path) -> tuple[bytes, bool]:
    if path.is_symlink() or not path.exists():
        return b"", False
    try:
        return path.read_bytes(), True
    except (IsADirectoryError, OSError):
        return b"", False


@dataclass(frozen=True)
class OperationChange:
    rel_path: str
    base_hash: str
    base_existed: bool
    final_content: bytes | None  # None → delete


class OverlayUnsupportedChangeError(RuntimeError):
    """Raised when upperdir contains a kind OCC can't represent (D3a)."""


class AbortedVersionError(RuntimeError):
    """Raised when strict_base check fails."""


def snapshot_lowerdir(workspace: Path, lowerdir: Path) -> None:
    """Prototype of P0.7's CoW lowerdir snapshot.

    Copies the live workspace — tracked, untracked, and dirty content
    included — giving lowerdir its own inodes. macOS ``cp -a`` uses
    ``clonefile(2)`` on APFS (CoW, no byte copy); Linux/btrfs/XFS will
    CoW via ``--reflink`` in the real auditor. On other filesystems this
    degrades to a byte copy — still correct, just costlier.

    We cannot use ``cp -al`` (hardlinks): Python's ``Path.write_bytes``
    opens ``O_WRONLY|O_TRUNC`` on the existing inode, so a hardlinked
    lowerdir would alias subsequent workspace writes and silently leak
    peer mutations into the OCC base_hash, defeating drift detection.
    """
    lowerdir.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["cp", "-a", f"{workspace}/.", str(lowerdir)],
        check=True,
    )


def diff_upperdir(
    lowerdir: Path, upperdir: Path
) -> list[tuple[str, str, bytes | None]]:
    """Return [(kind, rel_path, payload_or_None)].

    kind ∈ {"modify", "create", "delete", "symlink", "opaque_dir"}.
    A real overlay would read whiteout/opaque xattrs; we use simple conventions:
      - upperdir file present + lowerdir present → modify (or create if absent in lower)
      - upperdir symlink → symlink (unsupported)
      - upperdir directory without files → opaque_dir (unsupported)
      - a ``.whiteout.<name>`` marker in upperdir → delete
    """
    diffs: list[tuple[str, str, bytes | None]] = []
    if not upperdir.exists():
        return diffs

    for root, dirs, files in os.walk(upperdir, followlinks=False):
        # Handle symlinks listed as files or dirs.
        for name in files:
            abs_path = Path(root) / name
            rel = abs_path.relative_to(upperdir).as_posix()
            if abs_path.is_symlink():
                diffs.append(("symlink", rel, None))
                continue
            if name.startswith(".whiteout."):
                target_rel = (
                    Path(root).relative_to(upperdir) / name[len(".whiteout."):]
                ).as_posix()
                diffs.append(("delete", target_rel, None))
                continue
            data = abs_path.read_bytes()
            lower_path = lowerdir / rel
            if lower_path.exists():
                diffs.append(("modify", rel, data))
            else:
                diffs.append(("create", rel, data))
        for name in dirs:
            abs_path = Path(root) / name
            if abs_path.is_symlink():
                rel = abs_path.relative_to(upperdir).as_posix()
                diffs.append(("symlink", rel, None))
    return diffs


def build_changes(
    lowerdir: Path, diffs: list[tuple[str, str, bytes | None]]
) -> list[OperationChange]:
    """Lower base_hash from the lowerdir snapshot (P0.7)."""
    changes: list[OperationChange] = []
    for kind, rel, payload in diffs:
        if kind in {"symlink", "opaque_dir"}:
            raise OverlayUnsupportedChangeError(
                f"upperdir change kind {kind!r} at {rel!r} unsupported (D3a)"
            )
        lower_path = lowerdir / rel
        base_bytes, base_existed = _read_if_exists(lower_path)
        base_hash = _hash_bytes(base_bytes) if base_existed else ""
        changes.append(
            OperationChange(
                rel_path=rel,
                base_hash=base_hash,
                base_existed=base_existed,
                final_content=payload,
            )
        )
    return changes


def commit_strict_base(
    workspace: Path, changes: list[OperationChange]
) -> None:
    """Two-pass: verify all strict_base predicates, then apply.

    Either every change lands or none does (mirrors
    commit_operation_against_base atomicity).
    """
    # Pass 1 — verify.
    for ch in changes:
        current_bytes, current_existed = _read_if_exists(workspace / ch.rel_path)
        current_hash = _hash_bytes(current_bytes) if current_existed else ""
        if ch.base_existed != current_existed:
            raise AbortedVersionError(
                f"aborted_version: {ch.rel_path} existed={current_existed} "
                f"but base said existed={ch.base_existed}"
            )
        if ch.base_existed and current_hash != ch.base_hash:
            raise AbortedVersionError(
                f"aborted_version: {ch.rel_path} drifted "
                f"(base={ch.base_hash}, current={current_hash})"
            )

    # Pass 2 — apply.
    for ch in changes:
        target = workspace / ch.rel_path
        if ch.final_content is None:
            if target.exists():
                target.unlink()
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(ch.final_content)


# --- Test fixtures ------------------------------------------------------------


@pytest.fixture
def work(tmp_path: Path) -> dict[str, Path]:
    workspace = tmp_path / "workspace"
    lowerdir = tmp_path / "lowerdir"
    upperdir = tmp_path / "upperdir"
    workspace.mkdir()
    upperdir.mkdir()
    return {"workspace": workspace, "lowerdir": lowerdir, "upperdir": upperdir}


# --- Property tests -----------------------------------------------------------


def test_drift_detected_against_peer_write(work: dict[str, Path]) -> None:
    """Property 1 — strict_base aborts when a peer mutates workspace mid-run."""
    (work["workspace"] / "foo.py").write_bytes(b"A")
    snapshot_lowerdir(work["workspace"], work["lowerdir"])

    # Actor A plans a modification to foo.py inside upperdir.
    (work["upperdir"] / "foo.py").write_bytes(b"A2-from-actor-A")

    # Actor B writes workspace/foo.py before A commits.
    (work["workspace"] / "foo.py").write_bytes(b"B-wrote-first")

    diffs = diff_upperdir(work["lowerdir"], work["upperdir"])
    changes = build_changes(work["lowerdir"], diffs)

    with pytest.raises(AbortedVersionError, match="aborted_version"):
        commit_strict_base(work["workspace"], changes)

    # Disk still reflects B's write — A's change did NOT land.
    assert (work["workspace"] / "foo.py").read_bytes() == b"B-wrote-first"


def test_untracked_preserved_and_not_conflict(work: dict[str, Path]) -> None:
    """Property 2 — untracked file is preserved, no false conflict."""
    (work["workspace"] / "foo.py").write_bytes(b"hello")
    (work["workspace"] / "scratch.txt").write_bytes(b"untracked-note")
    snapshot_lowerdir(work["workspace"], work["lowerdir"])

    (work["upperdir"] / "foo.py").write_bytes(b"hello-edited")

    diffs = diff_upperdir(work["lowerdir"], work["upperdir"])
    changes = build_changes(work["lowerdir"], diffs)
    commit_strict_base(work["workspace"], changes)

    # foo.py updated; scratch.txt untouched; no OperationChange emitted for
    # scratch.txt since it was not in upperdir (no "new" flag).
    assert (work["workspace"] / "foo.py").read_bytes() == b"hello-edited"
    assert (work["workspace"] / "scratch.txt").read_bytes() == b"untracked-note"
    assert [c.rel_path for c in changes] == ["foo.py"]


def test_dirty_file_is_base(work: dict[str, Path]) -> None:
    """Property 3 — dirty uncommitted content forms the OCC base, not HEAD.

    We simulate "HEAD" by initializing bar.py with committed content, then
    replacing it with dirty content before the snapshot. The commit should
    succeed because the base_hash derives from dirty content and workspace
    still holds dirty content at commit time.
    """
    (work["workspace"] / "bar.py").write_bytes(b"HEAD-content")
    # Dirty edit replaces HEAD content before snapshot.
    (work["workspace"] / "bar.py").write_bytes(b"DIRTY-uncommitted")

    snapshot_lowerdir(work["workspace"], work["lowerdir"])

    assert (work["lowerdir"] / "bar.py").read_bytes() == b"DIRTY-uncommitted", (
        "lowerdir must capture dirty content, not HEAD — if this fails, cp -al "
        "with --reflink or --no-dereference is behaving unexpectedly."
    )

    (work["upperdir"] / "bar.py").write_bytes(b"DIRTY-then-edited")

    diffs = diff_upperdir(work["lowerdir"], work["upperdir"])
    changes = build_changes(work["lowerdir"], diffs)
    commit_strict_base(work["workspace"], changes)

    assert (work["workspace"] / "bar.py").read_bytes() == b"DIRTY-then-edited"
    # Crucial: base_hash equals hash of the dirty content, not HEAD.
    assert changes[0].base_hash == _hash_bytes(b"DIRTY-uncommitted")


def test_symlink_upperdir_rejected(work: dict[str, Path]) -> None:
    """Property 4 — D3a: symlink in upperdir aborts the whole run."""
    (work["workspace"] / "foo.py").write_bytes(b"hello")
    snapshot_lowerdir(work["workspace"], work["lowerdir"])

    # Upperdir contains both a legit edit and a symlink.
    (work["upperdir"] / "foo.py").write_bytes(b"hello-edited")
    (work["upperdir"] / "link_to_foo").symlink_to(work["upperdir"] / "foo.py")

    diffs = diff_upperdir(work["lowerdir"], work["upperdir"])
    with pytest.raises(OverlayUnsupportedChangeError, match="symlink"):
        build_changes(work["lowerdir"], diffs)

    # Disk unchanged: no commit ever happened.
    assert (work["workspace"] / "foo.py").read_bytes() == b"hello"
    assert not (work["workspace"] / "link_to_foo").exists()


# --- Regression guards against the old HEAD-worktree assumption ---------------


def test_head_worktree_assumption_would_false_positive(work: dict[str, Path]) -> None:
    """Documents WHY P0.7 re-scoped D2 away from HEAD.

    If lowerdir were a git HEAD worktree, the untracked file ``scratch.txt``
    would not exist in lowerdir. Any diff that touched files *adjacent* to
    scratch.txt could confuse upperdir walk OR (worse) a codeact command that
    reads scratch.txt would see it missing from the overlay merged view.

    This test asserts the ``cp -al`` lowerdir choice does NOT exhibit that
    failure mode — scratch.txt is present in the snapshot.
    """
    (work["workspace"] / "foo.py").write_bytes(b"a")
    (work["workspace"] / "scratch.txt").write_bytes(b"untracked")
    snapshot_lowerdir(work["workspace"], work["lowerdir"])

    # The critical assertion: lowerdir sees the untracked file.
    assert (work["lowerdir"] / "scratch.txt").read_bytes() == b"untracked"


if __name__ == "__main__":
    import sys
    sys.exit(pytest.main([__file__, "-v"]))
