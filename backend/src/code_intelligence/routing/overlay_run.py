"""Sandbox-side overlay CodeAct runner.

Runs inside ``unshare -Urm`` on the sandbox (see
``docs/architecture/overlay-sandbox-plan.md`` §3). Responsibilities in
order:

1. Mount setup — one tmpfs at ``/ns/tmp`` with ``upper``/``work`` subdirs
   (single superblock, as required by overlayfs), a bind of the live
   workspace as ``lowerdir``, a rootless overlay with ``userxattr``, and
   a final bind of ``/ns/merged`` over the workspace root so the user
   command keeps its expected absolute path.
2. User command — runs under the merged view. Writes land in tmpfs
   upperdir; reads pass through to lower.
3. Upperdir classifier — walks upper, gates on ``.git/*`` writes,
   detects whiteouts (privileged ``S_IFCHR`` + rootless
   ``user.overlay.whiteout`` xattr) and opaque dirs (both xattr
   namespaces), batches ``git check-ignore`` to split the
   **gitinclude route** (→ OCC, first-writer-wins) from the
   **gitignore route** (→ direct-merge, per-file last-writer-wins).
   The route key is ``git check-ignore`` only; git index membership is
   not consulted, so brand-new files that are not matched by any
   ``.gitignore`` rule go through the gitinclude / OCC route.
4. Direct-merge gitignore writes into the live workspace via the
   ``/ns/lower`` bind, using per-op unique tempfile + ``os.rename`` for
   per-file atomic last-writer-wins behavior. **Atomicity is per file,
   not per tree.** Concurrent multi-file installs of different versions
   to the same gitignore prefix can interleave; no sandbox-side
   coordination is provided (plan §5.1).
5. Emit gitinclude changes as NDJSON at ``$RUN_DIR/diff.ndjson``
   for the orchestrator's OCC pass.

The classifier (:class:`Classifier`) is pure and dependency-injected:
xattr reads, ``git show <snap>:<path>`` lookups, and ``git check-ignore``
batching all go through callables. That keeps it unit-testable on darwin
where ``unshare`` / overlayfs / Linux xattrs do not exist.

Exit status:
* On success, the script exits with the user command's exit code.
* On policy reject, the script writes a ``_reject`` metadata line to
  ``diff.ndjson`` and exits with ``200 + policy_code`` so the orchestrator
  can distinguish reject-sentinel from a user-command exit.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from typing import Any, Callable, Iterable, Iterator


# ---------------------------------------------------------------------------
# Policy reject codes. Orchestrator surfaces these via
# ``overlay_types.OverlayPolicyReject.reason``.
# ---------------------------------------------------------------------------

REJECT_DOTGIT = "overlay_rejected_dotgit_writes"
REJECT_GITIGNORE_WHITEOUT = "overlay_refused_gitignore_whiteout"
REJECT_GITIGNORE_OPAQUE_DIR = "overlay_refused_opaque_dir"
REJECT_UNSUPPORTED_SYMLINK = "overlay_unsupported_symlink"
REJECT_UNSUPPORTED_OPAQUE_DIR = "overlay_unsupported_opaque_dir"
REJECT_NON_UTF8_GITINCLUDE = "overlay_non_utf8_gitinclude"
REJECT_UPPER_FULL = "overlay_upper_full"

_REJECT_EXIT_BASE = 200
_REJECT_EXIT_CODES: dict[str, int] = {
    REJECT_DOTGIT: _REJECT_EXIT_BASE + 1,
    REJECT_GITIGNORE_WHITEOUT: _REJECT_EXIT_BASE + 2,
    REJECT_GITIGNORE_OPAQUE_DIR: _REJECT_EXIT_BASE + 3,
    REJECT_UNSUPPORTED_SYMLINK: _REJECT_EXIT_BASE + 4,
    REJECT_UNSUPPORTED_OPAQUE_DIR: _REJECT_EXIT_BASE + 5,
    REJECT_NON_UTF8_GITINCLUDE: _REJECT_EXIT_BASE + 6,
    REJECT_UPPER_FULL: _REJECT_EXIT_BASE + 7,
}


# ---------------------------------------------------------------------------
# Overlay kind detection (both privileged and rootless representations).
# ---------------------------------------------------------------------------


def is_whiteout(st: os.stat_result, xattrs: dict[bytes, bytes]) -> bool:
    """True when *st* is an overlay whiteout.

    Privileged overlays emit a char-device with ``rdev == 0``. Rootless
    overlays (``userxattr``) cannot call ``mknod`` and instead mark a
    zero-size regular file with ``user.overlay.whiteout``.
    """
    if stat.S_ISCHR(st.st_mode) and st.st_rdev == 0:
        return True
    if stat.S_ISREG(st.st_mode) and st.st_size == 0:
        if b"user.overlay.whiteout" in xattrs:
            return True
    return False


def is_opaque_dir(st: os.stat_result, xattrs: dict[bytes, bytes]) -> bool:
    """True when *st* marks an overlay opaque directory.

    Privileged overlays use ``trusted.overlay.opaque="y"``. Rootless
    overlays use ``user.overlay.opaque="y"``.
    """
    if not stat.S_ISDIR(st.st_mode):
        return False
    return (
        xattrs.get(b"trusted.overlay.opaque") == b"y"
        or xattrs.get(b"user.overlay.opaque") == b"y"
    )


def is_symlink(st: os.stat_result) -> bool:
    return stat.S_ISLNK(st.st_mode)


# ---------------------------------------------------------------------------
# Classifier — pure-ish, dependency-injected for testability.
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class UpperEntry:
    """One upperdir entry handed to the classifier."""

    rel: str
    st: os.stat_result
    xattrs: dict[bytes, bytes]
    upper_path: str  # absolute path under /ns/tmp/upper (or synthetic root in tests)


@dataclass(frozen=True)
class GitincludeChange:
    """One gitinclude-route change to emit to NDJSON for OCC.

    "Gitinclude" means "everything ``git check-ignore`` did not flag" —
    routing is keyed on ``.gitignore`` rules, not git-index membership,
    so this also covers brand-new files that are not matched by any
    ``.gitignore`` rule.
    """

    path: str
    kind: str  # "create" | "modify" | "delete"
    base_content: str
    base_existed: bool
    final_content: str | None  # None for delete


@dataclass(frozen=True)
class ClassifyOutcome:
    gitinclude: tuple[GitincludeChange, ...]
    gitignore_paths: tuple[str, ...]
    direct_merged_bytes: int
    whiteouts_gitinclude: int
    whiteouts_gitignore_refused: int
    dotgit_rejects: int


@dataclass(frozen=True)
class PolicyRejectOutcome:
    reason: str
    paths: tuple[str, ...]


class Classifier:
    """Classify one upperdir walk into gitinclude / gitignore / rejects.

    Routing is decided by ``git check-ignore`` against the live workspace.
    Files flagged by check-ignore go to the gitignore route (direct-merge,
    per-file last-writer-wins). Everything else goes to the gitinclude
    route (OCC, first-writer-wins) — including new files that are not in
    the git index but also not matched by any ``.gitignore`` rule.

    Callables keep the implementation decoupled from live syscalls so it
    can be exercised on darwin:

    * ``read_upper_bytes(rel)`` — read bytes from the upper file at *rel*.
    * ``git_show_base(rel) -> bytes | None`` — return ``git show <snap>:rel``
      bytes, or ``None`` when the path did not exist at SNAP time.
    * ``check_ignore(rels) -> set[str]`` — batch-check which *rels* are
      gitignore relative to the live workspace (one batched git call).
    * ``direct_merge(rel, upper_path, upper_st)`` — copy bytes from the
      upper into the live workspace via an atomic ``tempfile + rename``.
      Returns the number of bytes written.
    * ``prune_opaque_narrow(rel, upper_dir_path) -> int`` — narrow-prune
      a gitignored opaque directory: delete live children under *rel*
      that are not present in the upperdir copy. Returns the count of
      entries pruned. Defaults to a no-op when omitted (for tests that
      exercise non-opaque paths).
    """

    def __init__(
        self,
        *,
        read_upper_bytes: Callable[[str], bytes],
        git_show_base: Callable[[str], bytes | None],
        check_ignore: Callable[[list[str]], set[str]],
        direct_merge: Callable[[str, str, os.stat_result], int],
        prune_opaque_narrow: Callable[[str, str], int] | None = None,
    ) -> None:
        self._read_upper_bytes = read_upper_bytes
        self._git_show_base = git_show_base
        self._check_ignore = check_ignore
        self._direct_merge = direct_merge
        self._prune_opaque_narrow = prune_opaque_narrow or (lambda _rel, _up: 0)

    def classify(
        self, entries: Iterable[UpperEntry]
    ) -> ClassifyOutcome | PolicyRejectOutcome:
        entries = list(entries)

        # --- Pass 1: .git/* reject gate. Runs first so we never invoke
        # git check-ignore against a workspace the user mutated under
        # ``.git/``.
        dotgit = [e for e in entries if e.rel == ".git" or e.rel.startswith(".git/")]
        if dotgit:
            return PolicyRejectOutcome(
                reason=REJECT_DOTGIT,
                paths=tuple(sorted(e.rel for e in dotgit)),
            )

        # --- Pass 2: detect symlink / opaque-dir on gitinclude entries and
        # split whiteouts.
        whiteouts: list[UpperEntry] = []
        regular: list[UpperEntry] = []
        opaque_dirs: list[UpperEntry] = []
        symlinks: list[UpperEntry] = []

        for entry in entries:
            if is_whiteout(entry.st, entry.xattrs):
                whiteouts.append(entry)
            elif is_symlink(entry.st):
                symlinks.append(entry)
            elif is_opaque_dir(entry.st, entry.xattrs):
                opaque_dirs.append(entry)
            elif stat.S_ISREG(entry.st.st_mode):
                regular.append(entry)
            # Plain (non-opaque, non-whiteout) directories are ignored —
            # they are just containers for their children, already walked.

        # --- Pass 3: one batched git check-ignore call for the surviving
        # candidate paths. Whiteouts, opaque-dirs, symlinks, and regular
        # files all need route classification.
        #
        # Dir candidates (opaque-dirs, and dir-ish symlinks in principle)
        # are wired with a trailing "/" so dir-only .gitignore patterns
        # like ".pytest_cache/" match. Without the slash, git falls back
        # to lstat on the live workspace, which lies on the lower side
        # and may not contain the entry (sandbox-only creation); that
        # misses the match and wrongly routes us to the tracked route.
        # The returned set is normalized back to bare rels.
        candidate_entries = whiteouts + regular + opaque_dirs + symlinks
        candidates_wire = [
            (e.rel + "/") if stat.S_ISDIR(e.st.st_mode) else e.rel
            for e in candidate_entries
        ]
        ignored_wire = (
            self._check_ignore(candidates_wire) if candidates_wire else set()
        )
        ignored = {p.rstrip("/") for p in ignored_wire}

        # --- Pass 4: kind-gate rejections on the gitinclude route.
        bad_symlinks = [e.rel for e in symlinks if e.rel not in ignored]
        if bad_symlinks:
            return PolicyRejectOutcome(
                reason=REJECT_UNSUPPORTED_SYMLINK,
                paths=tuple(sorted(bad_symlinks)),
            )
        bad_opaque = [e.rel for e in opaque_dirs if e.rel not in ignored]
        if bad_opaque:
            return PolicyRejectOutcome(
                reason=REJECT_UNSUPPORTED_OPAQUE_DIR,
                paths=tuple(sorted(bad_opaque)),
            )
        bad_gitignore_whiteout = [e.rel for e in whiteouts if e.rel in ignored]
        if bad_gitignore_whiteout:
            return PolicyRejectOutcome(
                reason=REJECT_GITIGNORE_WHITEOUT,
                paths=tuple(sorted(bad_gitignore_whiteout)),
            )
        # Gitignored opaque dirs used to reject (REJECT_GITIGNORE_OPAQUE_DIR).
        # They now narrow-prune: delete live children not present in upper,
        # then let children direct-merge normally. See Pass 5 below.

        # --- Pass 5: gitinclude-route emits + gitignore direct-merges.
        gitinclude: list[GitincludeChange] = []
        gitignore_paths: list[str] = []
        direct_merged_bytes = 0
        whiteouts_gitinclude = 0

        # Narrow-prune gitignored opaque dirs before children direct-merge.
        # Contract: remove lower children whose name is not also present
        # in the upper opaque-dir copy. Files the sandbox wrote land via
        # the regular-file direct-merge loop below, so they are not
        # touched here. Bounds blast radius for spuriously-opaqued dirs
        # (fuse-overlayfs housekeeping, copy-up races).
        for entry in opaque_dirs:
            if entry.rel not in ignored:
                continue  # already rejected in Pass 4
            self._prune_opaque_narrow(entry.rel, entry.upper_path)
            gitignore_paths.append(entry.rel)

        # Tracked whiteouts (deletions against the SNAP base).
        for entry in whiteouts:
            if entry.rel in ignored:
                continue  # already rejected above; defensive
            whiteouts_gitinclude += 1
            base_bytes = self._git_show_base(entry.rel)
            base_existed = base_bytes is not None
            try:
                base_text = (base_bytes or b"").decode("utf-8")
            except UnicodeDecodeError:
                return PolicyRejectOutcome(
                    reason=REJECT_NON_UTF8_GITINCLUDE, paths=(entry.rel,)
                )
            gitinclude.append(
                GitincludeChange(
                    path=entry.rel,
                    kind="delete",
                    base_content=base_text,
                    base_existed=base_existed,
                    final_content=None,
                )
            )

        # Regular-file upper entries: route, classify create/modify, gate on mode-only.
        for entry in regular:
            if entry.rel in ignored:
                gitignore_paths.append(entry.rel)
                direct_merged_bytes += self._direct_merge(
                    entry.rel, entry.upper_path, entry.st
                )
                continue

            # Tracked route.
            try:
                upper_bytes = self._read_upper_bytes(entry.rel)
            except OSError as exc:
                raise _ClassifierIOError(
                    f"upperdir read failed for {entry.rel!r}: {exc}"
                ) from exc
            base_bytes = self._git_show_base(entry.rel)
            base_existed = base_bytes is not None

            # Mode-only short-circuit: if the upper content equals the SNAP
            # base, overlay copied-up on mode change (or an agent wrote the
            # same content). OCC does not track mode; skip.
            if base_existed and upper_bytes == base_bytes:
                continue

            try:
                final_text = upper_bytes.decode("utf-8")
            except UnicodeDecodeError:
                return PolicyRejectOutcome(
                    reason=REJECT_NON_UTF8_GITINCLUDE, paths=(entry.rel,)
                )
            try:
                base_text = (base_bytes or b"").decode("utf-8")
            except UnicodeDecodeError:
                return PolicyRejectOutcome(
                    reason=REJECT_NON_UTF8_GITINCLUDE, paths=(entry.rel,)
                )
            kind = "modify" if base_existed else "create"
            gitinclude.append(
                GitincludeChange(
                    path=entry.rel,
                    kind=kind,
                    base_content=base_text,
                    base_existed=base_existed,
                    final_content=final_text,
                )
            )

        return ClassifyOutcome(
            gitinclude=tuple(gitinclude),
            gitignore_paths=tuple(sorted(gitignore_paths)),
            direct_merged_bytes=direct_merged_bytes,
            whiteouts_gitinclude=whiteouts_gitinclude,
            whiteouts_gitignore_refused=0,  # refused ones cause early return
            dotgit_rejects=0,
        )


class _ClassifierIOError(RuntimeError):
    """Raised when the classifier can't read an upperdir file it expected."""


# ---------------------------------------------------------------------------
# Upperdir walker.
# ---------------------------------------------------------------------------


def walk_upperdir(upper_root: str) -> Iterator[UpperEntry]:
    """Yield one :class:`UpperEntry` per non-directory upperdir entry.

    Opaque directories themselves are yielded (so the classifier can
    reject or route them); plain directories act as containers only.
    Whiteouts (both privileged and rootless forms) surface here as
    regular entries; :func:`is_whiteout` distinguishes them from true
    files downstream.
    """
    upper_root = upper_root.rstrip("/")
    if not os.path.isdir(upper_root):
        return
    for dirpath, dirnames, filenames in os.walk(
        upper_root, topdown=True, followlinks=False
    ):
        rel_dir = os.path.relpath(dirpath, upper_root)
        rel_dir = "" if rel_dir == "." else rel_dir

        # Yield opaque directories so the classifier can reject them.
        if rel_dir:
            full = os.path.join(upper_root, rel_dir)
            try:
                st = os.lstat(full)
            except FileNotFoundError:
                pass
            else:
                xattrs = _read_xattrs(full)
                if is_opaque_dir(st, xattrs):
                    yield UpperEntry(
                        rel=rel_dir, st=st, xattrs=xattrs, upper_path=full
                    )

        for name in filenames:
            rel = os.path.join(rel_dir, name) if rel_dir else name
            full = os.path.join(dirpath, name)
            try:
                st = os.lstat(full)
            except FileNotFoundError:
                continue
            xattrs = _read_xattrs(full)
            yield UpperEntry(rel=rel, st=st, xattrs=xattrs, upper_path=full)

        # Sort dirnames for deterministic walk ordering in tests.
        dirnames.sort()


def _read_xattrs(path: str) -> dict[bytes, bytes]:
    """Return all extended attributes on *path* as a byte-keyed dict.

    Linux ``os.listxattr`` / ``os.getxattr`` are used when available. On
    platforms without the syscalls (darwin), an empty dict is returned
    — overlay is Linux-only in production; xattr-dependent classifier
    paths are tested by direct-constructing :class:`UpperEntry` objects
    with synthetic xattr dicts.
    """
    listxattr = getattr(os, "listxattr", None)
    getxattr = getattr(os, "getxattr", None)
    if listxattr is None or getxattr is None:
        return {}
    try:
        names = listxattr(path, follow_symlinks=False)
    except OSError:
        return {}
    out: dict[bytes, bytes] = {}
    for name in names:
        key = name.encode("utf-8") if isinstance(name, str) else name
        try:
            out[key] = getxattr(path, name, follow_symlinks=False)
        except OSError:
            continue
    return out


# ---------------------------------------------------------------------------
# Git helpers (run inside the ns; talk to the live repo via /ns/lower).
# ---------------------------------------------------------------------------


def git_show_base_factory(
    *, repo_root: str, snap: str
) -> Callable[[str], bytes | None]:
    """Return a callable that reads ``git show <snap>:<rel>``.

    Returns ``None`` when the path did not exist at SNAP time (missing
    object). Raises :class:`RuntimeError` for anything else.
    """

    def _show(rel: str) -> bytes | None:
        proc = subprocess.run(
            ["git", "-C", repo_root, "show", f"{snap}:{rel}"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if proc.returncode == 0:
            return proc.stdout
        stderr = proc.stderr.decode("utf-8", "replace")
        if "exists on disk, but not in" in stderr or "does not exist" in stderr:
            return None
        # Git returns 128 for missing objects; also treat "Path ... does
        # not exist" text as missing.
        if proc.returncode == 128 and "exists on disk" not in stderr:
            return None
        raise RuntimeError(
            f"git show {snap}:{rel} failed: rc={proc.returncode} stderr={stderr!r}"
        )

    return _show


def check_ignore_factory(*, repo_root: str) -> Callable[[list[str]], set[str]]:
    """Return a callable that batch-checks gitignore membership.

    Uses ``git check-ignore -z --stdin --verbose --non-matching`` so both
    matching and non-matching paths appear in the output. Chunks the
    stdin at 1 MiB to stay under any reasonable kernel pipe limit (plan
    §3.2 / §5.1).
    """

    def _check(paths: list[str]) -> set[str]:
        if not paths:
            return set()
        ignored: set[str] = set()
        for chunk in _chunk_paths(paths, byte_limit=1024 * 1024):
            stdin_bytes = b"\0".join(p.encode("utf-8") for p in chunk) + b"\0"
            proc = subprocess.run(
                [
                    "git",
                    "-C",
                    repo_root,
                    "check-ignore",
                    "-z",
                    "--stdin",
                    "--verbose",
                    "--non-matching",
                ],
                input=stdin_bytes,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            # Exit codes from git check-ignore:
            #   0: at least one path matched (ignored)
            #   1: no path matched
            # 128: fatal error
            # Any other non-(0|1) is an error.
            if proc.returncode not in (0, 1):
                stderr = proc.stderr.decode("utf-8", "replace")
                raise RuntimeError(
                    f"git check-ignore failed: rc={proc.returncode} stderr={stderr!r}"
                )
            # Output format with -z -v --non-matching is a stream of
            # records: source NUL linenum NUL pattern NUL path NUL
            # Non-matching records have empty source/linenum/pattern.
            fields = proc.stdout.split(b"\0")
            # Drop trailing empty from final NUL.
            if fields and fields[-1] == b"":
                fields = fields[:-1]
            for i in range(0, len(fields), 4):
                record = fields[i : i + 4]
                if len(record) < 4:
                    break
                source, _line, _pattern, path = record
                if source:  # non-empty source → this path was ignored
                    ignored.add(path.decode("utf-8"))
        return ignored

    return _check


def _chunk_paths(paths: list[str], *, byte_limit: int) -> Iterator[list[str]]:
    chunk: list[str] = []
    size = 0
    for p in paths:
        plen = len(p.encode("utf-8")) + 1  # +1 for NUL
        if chunk and size + plen > byte_limit:
            yield chunk
            chunk = []
            size = 0
        chunk.append(p)
        size += plen
    if chunk:
        yield chunk


# ---------------------------------------------------------------------------
# Direct-merge of gitignore writes into the live workspace.
# ---------------------------------------------------------------------------


def direct_merge_factory(
    *, live_root: str,
) -> Callable[[str, str, os.stat_result], int]:
    """Return a callable that atomically merges one upperdir file into live.

    Uses ``tempfile.mkstemp`` in the target's parent so the final
    ``os.rename`` is atomic; concurrent writers to the same gitignore
    path produce per-file last-writer-wins semantics on the final
    rename.

    **Atomicity guarantee is per file, not per tree.** Each upperdir
    entry gets its own rename race. Concurrent multi-file installs of
    different versions to the same gitignore prefix can interleave at
    the file level — file ``A`` from op1, file ``B`` from op2. No
    sandbox-side coordination prevents this; callers that need
    coherent dep-tree swap must serialize at the agent layer (plan
    §3.4 / §5.1).
    """

    def _merge(rel: str, upper_path: str, upper_st: os.stat_result) -> int:
        del upper_st  # size is read from the source file on copy
        live_target = os.path.join(live_root, rel)
        parent = os.path.dirname(live_target)
        if parent:
            os.makedirs(parent, exist_ok=True)
        fd, tmp_path = tempfile.mkstemp(
            dir=parent or ".",
            prefix=os.path.basename(live_target) + ".",
            suffix=".overlay-merge",
        )
        os.close(fd)
        try:
            shutil.copyfile(upper_path, tmp_path)
            os.rename(tmp_path, live_target)
        except OSError:
            try:
                os.unlink(tmp_path)
            except OSError:
                pass
            raise
        try:
            return os.path.getsize(live_target)
        except OSError:
            return 0

    return _merge


def narrow_prune_opaque_factory(
    *, live_root: str
) -> Callable[[str, str], int]:
    """Return a callable that narrow-prunes a gitignored opaque directory.

    For each child name under ``live_root/<rel>`` that is **not** also a
    child of ``<upper_dir>`` (the opaque dir's upperdir copy), remove
    the live child — file/symlink via :func:`os.unlink` (never follows
    symlinks), directory via :func:`shutil.rmtree`. Children whose name
    is also present in upper are left in place; the regular-file direct
    merge pass later overwrites them via atomic rename.

    This is the "narrow" opaque semantics (plan §3.4 addendum). Raw
    overlay semantics would ``rmtree`` the whole live dir; narrow bounds
    damage from spurious opaque markers to only the files the sandbox
    did not explicitly write.

    Any per-child error raises :class:`_ClassifierIOError` so the commit
    fails cleanly instead of half-pruning the live tree.
    """

    def _prune(rel: str, upper_dir: str) -> int:
        live_path = os.path.join(live_root, rel)
        # If the live dir is absent (or is a file, pathologically), there
        # is nothing to prune. The caller's direct-merge pass will
        # mkdir-p parents for any upper children as needed.
        try:
            live_st = os.lstat(live_path)
        except FileNotFoundError:
            return 0
        if not stat.S_ISDIR(live_st.st_mode) or stat.S_ISLNK(live_st.st_mode):
            return 0
        try:
            upper_children = set(os.listdir(upper_dir))
        except FileNotFoundError:
            upper_children = set()
        try:
            live_children = os.listdir(live_path)
        except FileNotFoundError:
            return 0
        pruned = 0
        for name in live_children:
            if name in upper_children:
                continue
            child = os.path.join(live_path, name)
            try:
                child_st = os.lstat(child)
            except FileNotFoundError:
                continue
            try:
                if stat.S_ISLNK(child_st.st_mode) or not stat.S_ISDIR(
                    child_st.st_mode
                ):
                    os.unlink(child)
                else:
                    shutil.rmtree(child)
            except OSError as exc:
                raise _ClassifierIOError(
                    f"narrow-prune failed for {child!r}: {exc}"
                ) from exc
            pruned += 1
        return pruned

    return _prune


# ---------------------------------------------------------------------------
# NDJSON emitters.
# ---------------------------------------------------------------------------


def write_diff_ndjson(
    *,
    run_dir: str,
    snap: str,
    exit_code: int,
    outcome: ClassifyOutcome,
    upper_bytes: int,
    upper_files: int,
    warnings: list[str] | None = None,
    snapshot_timings: dict[str, float] | None = None,
    run_timings: dict[str, float] | None = None,
) -> str:
    """Write ``$RUN_DIR/diff.ndjson`` and return its absolute path."""
    path = os.path.join(run_dir, "diff.ndjson")
    os.makedirs(run_dir, exist_ok=True)
    lines: list[str] = []
    meta = {
        "_meta": {
            "snap": snap,
            "exit_code": exit_code,
            "upper_bytes": upper_bytes,
            "upper_files": upper_files,
            "gitinclude_changes": len(outcome.gitinclude),
            "gitignore_changes": len(outcome.gitignore_paths),
            "gitignore_paths": list(outcome.gitignore_paths),
            "whiteouts_gitinclude": outcome.whiteouts_gitinclude,
            "whiteouts_gitignore_refused": outcome.whiteouts_gitignore_refused,
            "dotgit_rejects": outcome.dotgit_rejects,
            "direct_merged_bytes": outcome.direct_merged_bytes,
            "snapshot_timings": dict(snapshot_timings or {}),
            "run_timings": dict(run_timings or {}),
            "warnings": list(warnings or ()),
        }
    }
    lines.append(json.dumps(meta, separators=(",", ":")))
    for change in outcome.gitinclude:
        lines.append(
            json.dumps(
                {
                    "path": change.path,
                    "kind": change.kind,
                    "base_content": change.base_content,
                    "base_existed": change.base_existed,
                    "final_content": change.final_content,
                    "strict_base": True,
                },
                separators=(",", ":"),
            )
        )
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines))
        fh.write("\n")
    return path


def write_reject_ndjson(
    *,
    run_dir: str,
    snap: str,
    reject: PolicyRejectOutcome,
    snapshot_timings: dict[str, float] | None = None,
    run_timings: dict[str, float] | None = None,
) -> str:
    path = os.path.join(run_dir, "diff.ndjson")
    os.makedirs(run_dir, exist_ok=True)
    payload = {
        "_reject": {
            "snap": snap,
            "reason": reject.reason,
            "paths": list(reject.paths),
            "snapshot_timings": dict(snapshot_timings or {}),
            "run_timings": dict(run_timings or {}),
        }
    }
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(json.dumps(payload, separators=(",", ":")))
        fh.write("\n")
    return path


def reject_exit_code(reason: str) -> int:
    return _REJECT_EXIT_CODES.get(reason, _REJECT_EXIT_BASE)


# ---------------------------------------------------------------------------
# Mount + user-command orchestration. Only exercised on Linux; darwin
# tests drive the classifier directly.
# ---------------------------------------------------------------------------


_NS_ROOT = "/tmp/eos-codeact-ns"
_NS_TMP = "/tmp/eos-codeact-ns/tmp"
_NS_UPPER = "/tmp/eos-codeact-ns/tmp/upper"
_NS_WORK = "/tmp/eos-codeact-ns/tmp/work"
_NS_LOWER = "/tmp/eos-codeact-ns/lower"
_NS_MERGED = "/tmp/eos-codeact-ns/merged"


def _run(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs
    )


def _record_timing(timings: dict[str, float], key: str, started_at: float) -> None:
    timings[key] = round(time.perf_counter() - started_at, 6)


def _git(
    args: list[str],
    *,
    cwd: str,
    env: dict[str, str],
    check: bool = True,
) -> subprocess.CompletedProcess[bytes]:
    proc = _run(["git", "-C", cwd, *args], env=env)
    if check and proc.returncode != 0:
        raise RuntimeError(
            "git "
            + " ".join(args)
            + f" failed: rc={proc.returncode} "
            + f"stdout={proc.stdout.decode('utf-8', 'replace')} "
            + f"stderr={proc.stderr.decode('utf-8', 'replace')}"
        )
    return proc


def build_live_snapshot_in_namespace(repo_root: str) -> tuple[str, dict[str, float]]:
    """Build the live git snapshot inside this overlay runner process."""
    total_started = time.perf_counter()
    timings: dict[str, float] = {}

    validate_started = time.perf_counter()
    if not os.path.isdir(repo_root):
        raise RuntimeError(f"repo_root does not exist: {repo_root}")
    if not os.path.isdir(os.path.join(repo_root, ".git")):
        raise RuntimeError(
            "repo_root must be a canonical git checkout with a .git directory "
            f"(linked worktrees are not supported): {repo_root}"
        )
    _record_timing(timings, "validate_repo", validate_started)

    temp_index_started = time.perf_counter()
    tmp_index_fd, tmp_index_path = tempfile.mkstemp(prefix="git-snapshot-idx-")
    os.close(tmp_index_fd)
    os.unlink(tmp_index_path)
    _record_timing(timings, "temp_index", temp_index_started)

    env_started = time.perf_counter()
    env = dict(os.environ)
    env["GIT_INDEX_FILE"] = tmp_index_path
    env.setdefault("GIT_AUTHOR_NAME", "EphemeralOS Snapshot")
    env.setdefault("GIT_AUTHOR_EMAIL", "snapshot@ephemeralos.invalid")
    env.setdefault("GIT_COMMITTER_NAME", "EphemeralOS Snapshot")
    env.setdefault("GIT_COMMITTER_EMAIL", "snapshot@ephemeralos.invalid")
    env.setdefault("GIT_AUTHOR_DATE", "1700000000 +0000")
    env.setdefault("GIT_COMMITTER_DATE", "1700000000 +0000")
    _record_timing(timings, "prepare_env", env_started)

    try:
        head_started = time.perf_counter()
        head_proc = _git(
            ["rev-parse", "--verify", "HEAD"],
            cwd=repo_root,
            env=env,
            check=False,
        )
        has_head = head_proc.returncode == 0
        head_sha = (
            head_proc.stdout.decode("utf-8", "replace").strip()
            if has_head
            else ""
        )
        _record_timing(timings, "rev_parse_head", head_started)
        if has_head:
            read_tree_started = time.perf_counter()
            _git(["read-tree", "HEAD"], cwd=repo_root, env=env)
            _record_timing(timings, "read_tree", read_tree_started)
        else:
            timings["read_tree"] = 0.0

        add_started = time.perf_counter()
        _git(["add", "-A"], cwd=repo_root, env=env)
        _record_timing(timings, "git_add", add_started)

        write_tree_started = time.perf_counter()
        tree_proc = _git(["write-tree"], cwd=repo_root, env=env)
        tree_sha = tree_proc.stdout.decode("utf-8", "replace").strip()
        if not tree_sha:
            raise RuntimeError("git write-tree returned empty sha")
        _record_timing(timings, "write_tree", write_tree_started)

        commit_args = ["commit-tree", tree_sha, "-m", "overlay-snapshot"]
        if has_head:
            commit_args.extend(["-p", head_sha])
        commit_started = time.perf_counter()
        commit_proc = _git(commit_args, cwd=repo_root, env=env)
        commit_sha = commit_proc.stdout.decode("utf-8", "replace").strip()
        if not commit_sha:
            raise RuntimeError("git commit-tree returned empty sha")
        _record_timing(timings, "commit_tree", commit_started)
        timings["total"] = round(time.perf_counter() - total_started, 6)
        return commit_sha, timings
    finally:
        cleanup_started = time.perf_counter()
        try:
            os.unlink(tmp_index_path)
        except OSError:
            pass
        _record_timing(timings, "cleanup", cleanup_started)


def setup_mounts(*, live_root: str, upper_size_mb: int) -> None:
    """Mount the overlay stack inside the ns (see plan §3.1).

    Raises :class:`OverlayMountError` with a descriptive message when a
    step fails. ``upperdir`` and ``workdir`` share one tmpfs superblock
    (kernel requires same-fs), so we mount one tmpfs and use subdirs.
    """
    for d in (_NS_ROOT, _NS_TMP, _NS_LOWER, _NS_MERGED):
        os.makedirs(d, exist_ok=True)
    _check(_run(["mount", "-t", "tmpfs", "-o", f"size={upper_size_mb}m", "tmpfs", _NS_TMP]), step="tmpfs /ns/tmp")
    for d in (_NS_UPPER, _NS_WORK):
        os.makedirs(d, exist_ok=True)
    _check(_run(["mount", "--bind", live_root, _NS_LOWER]), step=f"bind {live_root} -> /ns/lower")
    overlay_opts = f"lowerdir={_NS_LOWER},upperdir={_NS_UPPER},workdir={_NS_WORK},userxattr"
    _check(
        _run(["mount", "-t", "overlay", "overlay", "-o", overlay_opts, _NS_MERGED]),
        step="mount overlay",
    )
    _check(_run(["mount", "--bind", _NS_MERGED, live_root]), step=f"bind /ns/merged -> {live_root}")


def _check(proc: subprocess.CompletedProcess[bytes], *, step: str) -> None:
    if proc.returncode == 0:
        return
    stderr = proc.stderr.decode("utf-8", "replace")
    raise OverlayMountError(f"{step}: rc={proc.returncode} stderr={stderr!r}")


class OverlayMountError(RuntimeError):
    """Raised when the namespace mount setup fails."""


def run_user_command(
    *, user_cmd: str, stdin_bytes: bytes | None, cwd: str, stdout_path: str
) -> tuple[bytes, int]:
    """Run the user command under the merged overlay view.

    The command runs via ``bash -o pipefail -lc`` with *cwd* as the
    working directory. Without the explicit ``cwd``, relative-path
    commands like ``pytest``, ``pip install -r requirements.txt``, and
    ``npm run build`` would resolve against the namespace's initial
    ``pwd`` (typically ``/``) instead of the live workspace.

    stdout and stderr are merged (the orchestrator already expects one
    combined stream via ``_extract_exit_code``).
    """
    proc = subprocess.Popen(
        ["bash", "-o", "pipefail", "-lc", user_cmd],
        stdin=subprocess.PIPE if stdin_bytes is not None else None,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        cwd=cwd,
    )
    if stdin_bytes is not None:
        assert proc.stdin is not None
        proc.stdin.write(stdin_bytes)
        proc.stdin.close()
    assert proc.stdout is not None
    chunks: list[bytes] = []
    with open(stdout_path, "wb") as stdout_file:
        while True:
            chunk = os.read(proc.stdout.fileno(), 8192)
            if not chunk:
                break
            chunks.append(chunk)
            stdout_file.write(chunk)
            stdout_file.flush()
    exit_code = proc.wait()
    return b"".join(chunks), exit_code


# ---------------------------------------------------------------------------
# CLI entry point. Invoked by the orchestrator as
# ``unshare -Urm python3 /path/to/overlay_run.py --workspace-root <...>``.
# ---------------------------------------------------------------------------


def _parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace-root", required=True)
    parser.add_argument("--run-dir", required=True)
    parser.add_argument("--snap", default="")
    parser.add_argument("--upper-size-mb", type=int, required=True)
    parser.add_argument(
        "--user-cmd-b64",
        required=True,
        help="Base64-encoded bash command to run inside the overlay.",
    )
    parser.add_argument(
        "--stdin-b64",
        default="",
        help="Optional base64-encoded stdin payload for the user command.",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:  # pragma: no cover - exercised via e2e
    total_started = time.perf_counter()
    args = _parse_args(argv if argv is not None else sys.argv[1:])
    workspace_root = args.workspace_root.rstrip("/")
    run_dir = args.run_dir.rstrip("/")
    os.makedirs(run_dir, exist_ok=True)

    snap = str(args.snap or "").strip()
    snapshot_timings: dict[str, float] = {}
    run_timings: dict[str, float] = {}
    if not snap:
        try:
            snap, snapshot_timings = build_live_snapshot_in_namespace(workspace_root)
        except Exception as exc:
            print(f"snapshot failed: {exc}", file=sys.stderr)
            return 254

    try:
        setup_started = time.perf_counter()
        setup_mounts(live_root=workspace_root, upper_size_mb=args.upper_size_mb)
        _record_timing(run_timings, "setup_mounts", setup_started)
    except OverlayMountError as exc:
        # Fail hard — orchestrator raises; no reject ndjson emitted
        # because we have no upperdir to audit.
        print(str(exc), file=sys.stderr)
        return 255

    decode_started = time.perf_counter()
    user_cmd = base64.b64decode(args.user_cmd_b64).decode("utf-8")
    stdin_bytes = base64.b64decode(args.stdin_b64) if args.stdin_b64 else None
    _record_timing(run_timings, "decode_command", decode_started)

    user_started = time.perf_counter()
    stdout_path = os.path.join(run_dir, "stdout.bin")
    stdout_bytes, exit_code = run_user_command(
        user_cmd=user_cmd,
        stdin_bytes=stdin_bytes,
        cwd=workspace_root,
        stdout_path=stdout_path,
    )
    _record_timing(run_timings, "user_command", user_started)

    # Walk upperdir (always, regardless of user exit code — plan §1 step 4).
    walk_started = time.perf_counter()
    upper_entries = list(walk_upperdir(_NS_UPPER))
    upper_files = len(upper_entries)
    upper_bytes = sum(e.st.st_size for e in upper_entries if stat.S_ISREG(e.st.st_mode))
    _record_timing(run_timings, "walk_upperdir", walk_started)

    classifier_started = time.perf_counter()
    classifier = Classifier(
        read_upper_bytes=lambda rel: open(os.path.join(_NS_UPPER, rel), "rb").read(),
        git_show_base=git_show_base_factory(repo_root=_NS_LOWER, snap=snap),
        check_ignore=check_ignore_factory(repo_root=_NS_LOWER),
        direct_merge=direct_merge_factory(live_root=_NS_LOWER),
        prune_opaque_narrow=narrow_prune_opaque_factory(live_root=_NS_LOWER),
    )
    _record_timing(run_timings, "build_classifier", classifier_started)

    classify_started = time.perf_counter()
    result = classifier.classify(upper_entries)
    _record_timing(run_timings, "classify", classify_started)
    run_timings["total"] = round(time.perf_counter() - total_started, 6)
    if isinstance(result, PolicyRejectOutcome):
        write_reject_ndjson(
            run_dir=run_dir,
            snap=snap,
            reject=result,
            snapshot_timings=snapshot_timings,
            run_timings=run_timings,
        )
        return reject_exit_code(result.reason)

    write_diff_ndjson(
        run_dir=run_dir,
        snap=snap,
        exit_code=exit_code,
        outcome=result,
        upper_bytes=upper_bytes,
        upper_files=upper_files,
        snapshot_timings=snapshot_timings,
        run_timings=run_timings,
    )
    return exit_code


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())


__all__ = [
    "Classifier",
    "ClassifyOutcome",
    "OverlayMountError",
    "PolicyRejectOutcome",
    "REJECT_DOTGIT",
    "REJECT_GITIGNORE_OPAQUE_DIR",
    "REJECT_GITIGNORE_WHITEOUT",
    "REJECT_NON_UTF8_GITINCLUDE",
    "REJECT_UNSUPPORTED_OPAQUE_DIR",
    "REJECT_UNSUPPORTED_SYMLINK",
    "REJECT_UPPER_FULL",
    "GitincludeChange",
    "UpperEntry",
    "build_live_snapshot_in_namespace",
    "check_ignore_factory",
    "direct_merge_factory",
    "git_show_base_factory",
    "narrow_prune_opaque_factory",
    "is_opaque_dir",
    "is_symlink",
    "is_whiteout",
    "reject_exit_code",
    "run_user_command",
    "setup_mounts",
    "walk_upperdir",
    "write_diff_ndjson",
    "write_reject_ndjson",
]
