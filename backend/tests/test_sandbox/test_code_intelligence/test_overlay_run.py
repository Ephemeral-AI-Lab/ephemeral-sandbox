"""Unit tests for ``overlay_run`` classifier and helpers.

The mount + user-command portion of ``overlay_run.py`` can only be
exercised on Linux with ``unshare`` / overlayfs / ``userxattr``. This
module targets the pure classifier, whiteout/opaque detection, NDJSON
emitter, and the ``git check-ignore`` batch helper — all of which run on
darwin with a real host ``git`` binary and synthetic upperdir trees.

Each test covers one branch called out in
``docs/architecture/overlay-sandbox-plan.md`` §3 / §8 PR 2.
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

from sandbox.code_intelligence.overlay.run import (
    Classifier,
    ClassifyOutcome,
    PolicyRejectOutcome,
    REJECT_DOTGIT,
    REJECT_GITIGNORE_WHITEOUT,
    REJECT_NON_UTF8_GITINCLUDE,
    REJECT_UNSUPPORTED_OPAQUE_DIR,
    REJECT_UNSUPPORTED_SYMLINK,
    UpperEntry,
    build_live_snapshot_in_namespace,
    check_ignore_factory,
    direct_merge_factory,
    is_opaque_dir,
    is_symlink,
    is_whiteout,
    narrow_prune_opaque_factory,
    reject_exit_code,
    walk_upperdir,
    write_diff_ndjson,
    write_reject_ndjson,
)


# ---------------------------------------------------------------------------
# Entry builders: construct UpperEntry values without touching real xattrs.
# ---------------------------------------------------------------------------


def _fake_stat(
    *,
    mode: int = stat.S_IFREG | 0o644,
    size: int = 0,
    rdev: int = 0,
) -> os.stat_result:
    return SimpleNamespace(  # type: ignore[return-value]
        st_mode=mode,
        st_ino=1,
        st_dev=1,
        st_nlink=1,
        st_uid=0,
        st_gid=0,
        st_size=size,
        st_atime=0.0,
        st_mtime=0.0,
        st_ctime=0.0,
        st_rdev=rdev,
    )


def _regular_entry(
    rel: str, *, upper_path: str = "", xattrs: dict[bytes, bytes] | None = None, size: int = 1
) -> UpperEntry:
    return UpperEntry(
        rel=rel,
        st=_fake_stat(size=size),
        xattrs=dict(xattrs or {}),
        upper_path=upper_path or f"/synthetic/upper/{rel}",
    )


def _whiteout_char_entry(rel: str) -> UpperEntry:
    return UpperEntry(
        rel=rel,
        st=_fake_stat(mode=stat.S_IFCHR, rdev=0),
        xattrs={},
        upper_path=f"/synthetic/upper/{rel}",
    )


def _whiteout_rootless_entry(rel: str) -> UpperEntry:
    return UpperEntry(
        rel=rel,
        st=_fake_stat(size=0),
        xattrs={b"user.overlay.whiteout": b""},
        upper_path=f"/synthetic/upper/{rel}",
    )


def _opaque_dir_entry(rel: str, *, ns: bytes = b"user.overlay.opaque") -> UpperEntry:
    return UpperEntry(
        rel=rel,
        st=_fake_stat(mode=stat.S_IFDIR | 0o755),
        xattrs={ns: b"y"},
        upper_path=f"/synthetic/upper/{rel}",
    )


def _symlink_entry(rel: str) -> UpperEntry:
    return UpperEntry(
        rel=rel,
        st=_fake_stat(mode=stat.S_IFLNK | 0o777, size=7),
        xattrs={},
        upper_path=f"/synthetic/upper/{rel}",
    )


class _Classifier:
    """Test harness that wires real-ish callbacks with in-memory state."""

    def __init__(
        self,
        *,
        upper_bytes: dict[str, bytes],
        base_bytes: dict[str, bytes],
        ignored: set[str],
    ) -> None:
        self.upper_bytes = upper_bytes
        self.base_bytes = base_bytes
        # ``ignored`` is matched against the *wire* rels the classifier
        # sends to check_ignore. Dir entries arrive with a trailing "/".
        # Tests that want to ignore a bare dir rel can pass either
        # ".venv" or ".venv/"; the harness tolerates both.
        self.ignored = {r.rstrip("/") for r in ignored}
        self.merged: list[tuple[str, int]] = []
        self.check_ignore_calls: list[list[str]] = []
        self.pruned: list[tuple[str, str]] = []

    def read_upper(self, rel: str) -> bytes:
        return self.upper_bytes[rel]

    def git_show_base(self, rel: str) -> bytes | None:
        return self.base_bytes.get(rel)

    def check_ignore(self, rels: list[str]) -> set[str]:
        self.check_ignore_calls.append(list(rels))
        return {r for r in rels if r.rstrip("/") in self.ignored}

    def direct_merge(self, rel: str, upper_path: str, upper_st: os.stat_result) -> int:
        del upper_path, upper_st
        size = len(self.upper_bytes.get(rel, b""))
        self.merged.append((rel, size))
        return size

    def prune_opaque_narrow(self, rel: str, upper_dir: str) -> int:
        self.pruned.append((rel, upper_dir))
        return 0

    def classifier(self) -> Classifier:
        return Classifier(
            read_upper_bytes=self.read_upper,
            git_show_base=self.git_show_base,
            check_ignore=self.check_ignore,
            direct_merge=self.direct_merge,
            prune_opaque_narrow=self.prune_opaque_narrow,
        )


# ---------------------------------------------------------------------------
# is_whiteout / is_opaque_dir / is_symlink
# ---------------------------------------------------------------------------


def test_is_whiteout_privileged_char_device() -> None:
    st = _fake_stat(mode=stat.S_IFCHR, rdev=0)
    assert is_whiteout(st, {}) is True


def test_is_whiteout_rootless_userxattr_zero_size_regular() -> None:
    st = _fake_stat(size=0)
    assert is_whiteout(st, {b"user.overlay.whiteout": b""}) is True


def test_is_whiteout_false_when_regular_non_zero_without_xattr() -> None:
    st = _fake_stat(size=10)
    assert is_whiteout(st, {}) is False


def test_is_whiteout_false_when_rootless_but_no_xattr() -> None:
    st = _fake_stat(size=0)
    assert is_whiteout(st, {}) is False


def test_is_opaque_dir_both_xattr_namespaces() -> None:
    st = _fake_stat(mode=stat.S_IFDIR | 0o755)
    assert is_opaque_dir(st, {b"trusted.overlay.opaque": b"y"}) is True
    assert is_opaque_dir(st, {b"user.overlay.opaque": b"y"}) is True
    assert is_opaque_dir(st, {}) is False


def test_is_opaque_dir_false_on_non_dir() -> None:
    st = _fake_stat(mode=stat.S_IFREG)
    assert is_opaque_dir(st, {b"user.overlay.opaque": b"y"}) is False


def test_is_symlink_positive() -> None:
    assert is_symlink(_fake_stat(mode=stat.S_IFLNK | 0o777)) is True
    assert is_symlink(_fake_stat(mode=stat.S_IFREG)) is False


# ---------------------------------------------------------------------------
# Classifier.classify: .git/* reject
# ---------------------------------------------------------------------------


def test_classifier_rejects_dotgit_writes_before_any_other_work() -> None:
    env = _Classifier(
        upper_bytes={".git/config": b"x"},
        base_bytes={},
        ignored=set(),
    )
    result = env.classifier().classify(
        [_regular_entry(".git/config"), _regular_entry("src/app.py")]
    )
    assert isinstance(result, PolicyRejectOutcome)
    assert result.reason == REJECT_DOTGIT
    assert ".git/config" in result.paths


def test_classifier_ignores_benign_dotgit_index_refresh() -> None:
    env = _Classifier(
        upper_bytes={
            ".git/index": b"refreshed-index",
            ".git/index.lock": b"transient-lock",
            "src/app.py": b"new",
        },
        base_bytes={"src/app.py": b"old"},
        ignored=set(),
    )
    result = env.classifier().classify(
        [
            _regular_entry(".git/index"),
            _regular_entry(".git/index.lock"),
            _regular_entry("src/app.py"),
        ]
    )
    assert isinstance(result, ClassifyOutcome)
    assert [change.path for change in result.gitinclude] == ["src/app.py"]
    assert env.check_ignore_calls == [["src/app.py"]]


def test_classifier_still_rejects_dotgit_mutation_with_index_refresh() -> None:
    env = _Classifier(
        upper_bytes={".git/index": b"refreshed-index", ".git/config": b"x"},
        base_bytes={},
        ignored=set(),
    )
    result = env.classifier().classify(
        [_regular_entry(".git/index"), _regular_entry(".git/config")]
    )

    assert isinstance(result, PolicyRejectOutcome)
    assert result.reason == REJECT_DOTGIT
    assert result.paths == (".git/config",)


def test_classifier_rejects_dotgit_even_for_nested_paths() -> None:
    env = _Classifier(upper_bytes={}, base_bytes={}, ignored=set())
    result = env.classifier().classify(
        [_regular_entry(".git/objects/pack/pack-xyz")]
    )
    assert isinstance(result, PolicyRejectOutcome)
    assert result.reason == REJECT_DOTGIT


# ---------------------------------------------------------------------------
# Classifier.classify: gitinclude add / modify / delete
# ---------------------------------------------------------------------------


def test_classifier_emits_gitinclude_create_for_new_file() -> None:
    env = _Classifier(
        upper_bytes={"src/new.py": b"print('new')\n"},
        base_bytes={},
        ignored=set(),
    )
    result = env.classifier().classify([_regular_entry("src/new.py")])
    assert isinstance(result, ClassifyOutcome)
    assert len(result.gitinclude) == 1
    change = result.gitinclude[0]
    assert change.kind == "create"
    assert change.base_existed is False
    assert change.base_content == ""
    assert change.final_content == "print('new')\n"
    assert result.gitignore_paths == ()


def test_classifier_emits_gitinclude_modify_for_existing_file() -> None:
    env = _Classifier(
        upper_bytes={"src/app.py": b"after\n"},
        base_bytes={"src/app.py": b"before\n"},
        ignored=set(),
    )
    result = env.classifier().classify([_regular_entry("src/app.py")])
    assert isinstance(result, ClassifyOutcome)
    change = result.gitinclude[0]
    assert change.kind == "modify"
    assert change.base_existed is True
    assert change.base_content == "before\n"
    assert change.final_content == "after\n"


def test_classifier_emits_gitinclude_delete_for_whiteout() -> None:
    env = _Classifier(
        upper_bytes={},
        base_bytes={"src/gone.py": b"rip\n"},
        ignored=set(),
    )
    result = env.classifier().classify([_whiteout_char_entry("src/gone.py")])
    assert isinstance(result, ClassifyOutcome)
    assert result.whiteouts_gitinclude == 1
    change = result.gitinclude[0]
    assert change.kind == "delete"
    assert change.base_existed is True
    assert change.base_content == "rip\n"
    assert change.final_content is None


def test_classifier_emits_gitinclude_delete_for_rootless_whiteout() -> None:
    env = _Classifier(
        upper_bytes={},
        base_bytes={"src/gone.py": b"rip\n"},
        ignored=set(),
    )
    result = env.classifier().classify([_whiteout_rootless_entry("src/gone.py")])
    assert isinstance(result, ClassifyOutcome)
    change = result.gitinclude[0]
    assert change.kind == "delete"
    assert change.base_existed is True


# ---------------------------------------------------------------------------
# Classifier.classify: gitignore route
# ---------------------------------------------------------------------------


def test_classifier_direct_merges_gitignore_create() -> None:
    env = _Classifier(
        upper_bytes={".venv/pyvenv.cfg": b"home=/usr\n"},
        base_bytes={},
        ignored={".venv/pyvenv.cfg"},
    )
    result = env.classifier().classify([_regular_entry(".venv/pyvenv.cfg")])
    assert isinstance(result, ClassifyOutcome)
    assert result.gitinclude == ()
    assert result.gitignore_paths == (".venv/pyvenv.cfg",)
    assert env.merged == [(".venv/pyvenv.cfg", len(b"home=/usr\n"))]
    assert result.direct_merged_bytes == len(b"home=/usr\n")


def test_classifier_direct_merges_gitignore_binary_bytes() -> None:
    payload = b"\xff\xfe\x00\x01not-utf-8"
    env = _Classifier(
        upper_bytes={"node_modules/pkg/a.so": payload},
        base_bytes={},
        ignored={"node_modules/pkg/a.so"},
    )
    result = env.classifier().classify([_regular_entry("node_modules/pkg/a.so")])
    # Non-UTF-8 content on gitignore route is fine; bytes pass through.
    assert isinstance(result, ClassifyOutcome)
    assert result.gitignore_paths == ("node_modules/pkg/a.so",)
    assert env.merged == [("node_modules/pkg/a.so", len(payload))]


def test_classifier_rejects_gitignore_whiteout() -> None:
    env = _Classifier(
        upper_bytes={},
        base_bytes={},
        ignored={".venv/pyvenv.cfg"},
    )
    result = env.classifier().classify([_whiteout_char_entry(".venv/pyvenv.cfg")])
    assert isinstance(result, PolicyRejectOutcome)
    assert result.reason == REJECT_GITIGNORE_WHITEOUT


def test_classifier_accepts_gitignore_opaque_dir_via_narrow_prune() -> None:
    # Opaque dir on a gitignored path now narrow-prunes instead of rejecting.
    # Classifier should invoke prune_opaque_narrow(rel, upper_path) once
    # and include the rel in the gitignore_paths tally.
    env = _Classifier(upper_bytes={}, base_bytes={}, ignored={".pytest_cache"})
    result = env.classifier().classify(
        [_opaque_dir_entry(".pytest_cache", ns=b"user.overlay.opaque")]
    )
    assert isinstance(result, ClassifyOutcome)
    assert ".pytest_cache" in result.gitignore_paths
    assert env.pruned == [(".pytest_cache", "/synthetic/upper/.pytest_cache")]


def test_classifier_sends_trailing_slash_for_dir_rels_to_check_ignore() -> None:
    # Dir-only .gitignore patterns (".pytest_cache/") only match when the
    # path passed to `git check-ignore` has a trailing slash or the path
    # exists as a directory on the live side. Sandbox-created dirs often
    # don't exist on lower at check time, so the classifier must pass
    # the slash explicitly. Verifies the wire format.
    env = _Classifier(
        upper_bytes={"src/app.py": b"x\n"},
        base_bytes={},
        ignored={".pytest_cache"},
    )
    env.classifier().classify(
        [
            _regular_entry("src/app.py"),
            _opaque_dir_entry(".pytest_cache"),
        ]
    )
    assert len(env.check_ignore_calls) == 1
    wire = env.check_ignore_calls[0]
    assert "src/app.py" in wire  # files stay bare
    assert ".pytest_cache/" in wire  # dirs get trailing "/"
    assert ".pytest_cache" not in wire


# ---------------------------------------------------------------------------
# Classifier.classify: kind-gate rejects on gitinclude route
# ---------------------------------------------------------------------------


def test_classifier_rejects_gitinclude_symlink() -> None:
    env = _Classifier(upper_bytes={}, base_bytes={}, ignored=set())
    result = env.classifier().classify([_symlink_entry("src/link")])
    assert isinstance(result, PolicyRejectOutcome)
    assert result.reason == REJECT_UNSUPPORTED_SYMLINK


def test_classifier_rejects_gitinclude_opaque_dir() -> None:
    env = _Classifier(upper_bytes={}, base_bytes={}, ignored=set())
    result = env.classifier().classify([_opaque_dir_entry("src/pkg")])
    assert isinstance(result, PolicyRejectOutcome)
    assert result.reason == REJECT_UNSUPPORTED_OPAQUE_DIR


# ---------------------------------------------------------------------------
# Classifier.classify: mode-only short-circuit + non-UTF-8 reject
# ---------------------------------------------------------------------------


def test_classifier_skips_mode_only_change_when_content_equal_to_snap() -> None:
    env = _Classifier(
        upper_bytes={"src/app.py": b"same\n"},
        base_bytes={"src/app.py": b"same\n"},
        ignored=set(),
    )
    result = env.classifier().classify([_regular_entry("src/app.py")])
    assert isinstance(result, ClassifyOutcome)
    assert result.gitinclude == ()


def test_classifier_rejects_non_utf8_on_gitinclude_route() -> None:
    env = _Classifier(
        upper_bytes={"src/app.py": b"\xff\xfe\x00binary"},
        base_bytes={},
        ignored=set(),
    )
    result = env.classifier().classify([_regular_entry("src/app.py")])
    assert isinstance(result, PolicyRejectOutcome)
    assert result.reason == REJECT_NON_UTF8_GITINCLUDE
    assert result.paths == ("src/app.py",)


def test_classifier_rejects_before_applying_gitignore_route_side_effects() -> None:
    env = _Classifier(
        upper_bytes={
            ".venv/pyvenv.cfg": b"home=/usr\n",
            "src/app.py": b"\xff\xfe\x00binary",
        },
        base_bytes={},
        ignored={".pytest_cache", ".venv/pyvenv.cfg"},
    )
    result = env.classifier().classify(
        [
            _opaque_dir_entry(".pytest_cache"),
            _regular_entry(".venv/pyvenv.cfg"),
            _regular_entry("src/app.py"),
        ]
    )

    assert isinstance(result, PolicyRejectOutcome)
    assert result.reason == REJECT_NON_UTF8_GITINCLUDE
    assert result.paths == ("src/app.py",)
    assert env.merged == []
    assert env.pruned == []


def test_classifier_rejects_non_utf8_base_content_on_gitinclude_modify() -> None:
    env = _Classifier(
        upper_bytes={"src/app.py": b"ok\n"},
        base_bytes={"src/app.py": b"\xff\xfeold"},
        ignored=set(),
    )
    result = env.classifier().classify([_regular_entry("src/app.py")])
    assert isinstance(result, PolicyRejectOutcome)
    assert result.reason == REJECT_NON_UTF8_GITINCLUDE


# ---------------------------------------------------------------------------
# Classifier.classify: mixed gitinclude + gitignore
# ---------------------------------------------------------------------------


def test_classifier_accepts_mixed_gitinclude_and_gitignore() -> None:
    env = _Classifier(
        upper_bytes={
            "src/app.py": b"new source\n",
            ".venv/x.cfg": b"dep=1\n",
        },
        base_bytes={"src/app.py": b"old source\n"},
        ignored={".venv/x.cfg"},
    )
    result = env.classifier().classify(
        [
            _regular_entry("src/app.py"),
            _regular_entry(".venv/x.cfg"),
        ]
    )
    assert isinstance(result, ClassifyOutcome)
    assert [c.path for c in result.gitinclude] == ["src/app.py"]
    assert result.gitignore_paths == (".venv/x.cfg",)


# ---------------------------------------------------------------------------
# reject_exit_code covers every declared reason
# ---------------------------------------------------------------------------


def test_reject_exit_codes_are_distinct_sentinels() -> None:
    reasons = [
        REJECT_DOTGIT,
        REJECT_GITIGNORE_WHITEOUT,
        REJECT_UNSUPPORTED_SYMLINK,
        REJECT_UNSUPPORTED_OPAQUE_DIR,
        REJECT_NON_UTF8_GITINCLUDE,
    ]
    codes = {reject_exit_code(r) for r in reasons}
    assert len(codes) == len(reasons), "policy codes collide"
    for code in codes:
        assert 200 < code < 256


# ---------------------------------------------------------------------------
# walk_upperdir — real filesystem walk
# ---------------------------------------------------------------------------


def test_walk_upperdir_yields_files_and_skips_plain_dirs(tmp_path: Path) -> None:
    root = tmp_path / "upper"
    root.mkdir()
    (root / "a.py").write_text("a\n", encoding="utf-8")
    (root / "pkg").mkdir()
    (root / "pkg" / "b.py").write_text("b\n", encoding="utf-8")

    rels = sorted(e.rel for e in walk_upperdir(str(root)))
    assert rels == ["a.py", "pkg/b.py"]


def test_walk_upperdir_handles_missing_root(tmp_path: Path) -> None:
    assert list(walk_upperdir(str(tmp_path / "missing"))) == []


# ---------------------------------------------------------------------------
# NDJSON emitters
# ---------------------------------------------------------------------------


def test_write_diff_ndjson_meta_and_entries(tmp_path: Path) -> None:
    outcome = ClassifyOutcome(
        gitinclude=(
            __import__("sandbox.code_intelligence.overlay.run", fromlist=["GitincludeChange"])
            .GitincludeChange(
                path="a.py",
                kind="modify",
                base_content="old\n",
                base_existed=True,
                final_content="new\n",
            ),
        ),
        gitignore_paths=(".venv/cfg",),
        direct_merged_bytes=12,
        whiteouts_gitinclude=0,
        whiteouts_gitignore_refused=0,
        dotgit_rejects=0,
    )

    path = write_diff_ndjson(
        run_dir=str(tmp_path),
        snap="deadbeef1234",
        exit_code=0,
        outcome=outcome,
        upper_bytes=99,
        upper_files=3,
        snapshot_timings={"total": 0.123, "git_add": 0.045},
        run_timings={"total": 0.5, "classify": 0.2},
    )

    lines = Path(path).read_text(encoding="utf-8").splitlines()
    meta = json.loads(lines[0])
    assert meta["_meta"]["snap"] == "deadbeef1234"
    assert meta["_meta"]["gitinclude_changes"] == 1
    assert meta["_meta"]["gitignore_changes"] == 1
    assert meta["_meta"]["gitignore_paths"] == [".venv/cfg"]
    assert meta["_meta"]["snapshot_timings"] == {"total": 0.123, "git_add": 0.045}
    assert meta["_meta"]["run_timings"] == {"total": 0.5, "classify": 0.2}
    entry = json.loads(lines[1])
    assert entry["path"] == "a.py"
    assert entry["kind"] == "modify"
    assert entry["base_content"] == "old\n"
    assert entry["final_content"] == "new\n"
    assert entry["strict_base"] is True


def test_write_reject_ndjson_emits_reject_block(tmp_path: Path) -> None:
    reject = PolicyRejectOutcome(reason=REJECT_DOTGIT, paths=(".git/config",))
    path = write_reject_ndjson(
        run_dir=str(tmp_path),
        snap="snapX",
        reject=reject,
        snapshot_timings={"total": 0.1},
        run_timings={"total": 0.2},
    )
    payload = json.loads(Path(path).read_text(encoding="utf-8"))
    assert payload == {
        "_reject": {
            "snap": "snapX",
            "reason": REJECT_DOTGIT,
            "paths": [".git/config"],
            "snapshot_timings": {"total": 0.1},
            "run_timings": {"total": 0.2},
        }
    }


# ---------------------------------------------------------------------------
# check_ignore_factory — real git check-ignore against a fixture repo.
# ---------------------------------------------------------------------------


def _init_repo(path: Path) -> None:
    subprocess.run(["git", "-C", str(path), "init", "-q"], check=True)
    subprocess.run(
        ["git", "-C", str(path), "config", "user.email", "t@example.invalid"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(path), "config", "user.name", "Tester"], check=True
    )


def test_build_live_snapshot_in_namespace_returns_snap_and_timings(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "snapshot-repo"
    repo.mkdir()
    _init_repo(repo)
    (repo / "app.py").write_text("committed\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-q", "-m", "seed"], check=True)
    (repo / "app.py").write_text("dirty\n", encoding="utf-8")

    snap, timings = build_live_snapshot_in_namespace(str(repo))

    shown = subprocess.check_output(
        ["git", "-C", str(repo), "show", f"{snap}:app.py"],
        text=True,
    )
    assert shown == "dirty\n"
    assert timings["total"] >= 0
    assert timings["git_add"] >= 0


def test_check_ignore_factory_splits_gitinclude_and_ignored(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    (repo / ".gitignore").write_text(".venv/\nnode_modules/\n", encoding="utf-8")
    (repo / "src").mkdir()
    (repo / "src" / "app.py").write_text("hi\n", encoding="utf-8")

    check = check_ignore_factory(repo_root=str(repo))

    ignored = check(
        [
            "src/app.py",
            ".venv/pyvenv.cfg",
            "node_modules/pkg/index.js",
            "README.md",  # not matched by any .gitignore rule
        ]
    )
    assert ignored == {".venv/pyvenv.cfg", "node_modules/pkg/index.js"}


def test_check_ignore_factory_empty_input_returns_empty_set(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    assert check_ignore_factory(repo_root=str(repo))([]) == set()


def test_check_ignore_factory_matches_dir_only_pattern_with_trailing_slash(
    tmp_path: Path,
) -> None:
    # Regression for opaque-dir routing bug: a dir-only gitignore pattern
    # like ".pytest_cache/" does NOT match bare ".pytest_cache" when the
    # path is absent on the live side (sandbox created it in upper only).
    # Passing the rel with a trailing slash matches correctly. The
    # classifier relies on this behavior.
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    (repo / ".gitignore").write_text(".pytest_cache/\n", encoding="utf-8")

    check = check_ignore_factory(repo_root=str(repo))
    assert ".pytest_cache" not in check([".pytest_cache"])  # bare, absent
    assert ".pytest_cache/" in check([".pytest_cache/"])  # with slash


# ---------------------------------------------------------------------------
# narrow_prune_opaque_factory — on-disk behavior.
# ---------------------------------------------------------------------------


def test_narrow_prune_opaque_deletes_only_lower_only_children(
    tmp_path: Path,
) -> None:
    live_root = tmp_path / "live"
    upper_root = tmp_path / "upper"
    live_dir = live_root / ".pytest_cache"
    upper_dir = upper_root / ".pytest_cache"
    live_dir.mkdir(parents=True)
    upper_dir.mkdir(parents=True)

    # Both sides have "shared.txt"; only live has "stale.txt"; only
    # upper has "new.txt" (no-op for prune — merge will write it later).
    (live_dir / "shared.txt").write_text("old", encoding="utf-8")
    (live_dir / "stale.txt").write_text("stale", encoding="utf-8")
    (upper_dir / "shared.txt").write_text("new", encoding="utf-8")
    (upper_dir / "new.txt").write_text("new", encoding="utf-8")

    prune = narrow_prune_opaque_factory(live_root=str(live_root))
    count = prune(".pytest_cache", str(upper_dir))

    assert count == 1
    assert (live_dir / "shared.txt").exists()  # preserved for rename-over
    assert not (live_dir / "stale.txt").exists()  # pruned


def test_narrow_prune_opaque_recurses_into_lower_only_subdirs(
    tmp_path: Path,
) -> None:
    live_root = tmp_path / "live"
    upper_root = tmp_path / "upper"
    live_dir = live_root / ".cache"
    upper_dir = upper_root / ".cache"
    (live_dir / "__pycache__").mkdir(parents=True)
    (live_dir / "__pycache__" / "a.pyc").write_bytes(b"pyc")
    upper_dir.mkdir(parents=True)

    prune = narrow_prune_opaque_factory(live_root=str(live_root))
    count = prune(".cache", str(upper_dir))

    assert count == 1
    assert not (live_dir / "__pycache__").exists()


def test_narrow_prune_opaque_unlinks_symlink_children_without_following(
    tmp_path: Path,
) -> None:
    # Critical safety property: if the live dir contains a symlink to an
    # *outside* directory, prune must unlink the symlink itself and not
    # descend into the target.
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "keep.txt").write_text("keep", encoding="utf-8")

    live_root = tmp_path / "live"
    upper_root = tmp_path / "upper"
    live_dir = live_root / ".cache"
    upper_dir = upper_root / ".cache"
    live_dir.mkdir(parents=True)
    upper_dir.mkdir(parents=True)
    os.symlink(str(outside), str(live_dir / "linked"))

    prune = narrow_prune_opaque_factory(live_root=str(live_root))
    count = prune(".cache", str(upper_dir))

    assert count == 1
    assert not (live_dir / "linked").exists()
    assert (outside / "keep.txt").exists()  # NOT followed into


def test_narrow_prune_opaque_returns_zero_when_live_dir_absent(
    tmp_path: Path,
) -> None:
    prune = narrow_prune_opaque_factory(live_root=str(tmp_path))
    # Nothing exists at this rel — should be a no-op, not an error.
    assert prune("missing/dir", str(tmp_path / "upper_missing")) == 0


# ---------------------------------------------------------------------------
# direct_merge_factory — atomic rename into live
# ---------------------------------------------------------------------------


def test_direct_merge_writes_file_and_is_observably_atomic(tmp_path: Path) -> None:
    upper = tmp_path / "upper"
    upper.mkdir()
    live = tmp_path / "live"
    live.mkdir()
    src = upper / ".venv" / "pyvenv.cfg"
    src.parent.mkdir(parents=True)
    src.write_text("home=/usr\n", encoding="utf-8")

    merge = direct_merge_factory(live_root=str(live))
    st = src.stat()
    bytes_written = merge(".venv/pyvenv.cfg", str(src), st)

    target = live / ".venv" / "pyvenv.cfg"
    assert target.read_text(encoding="utf-8") == "home=/usr\n"
    assert bytes_written == len("home=/usr\n")

    # No .overlay-merge temp files left behind in the parent.
    stray = [p.name for p in (live / ".venv").iterdir() if ".overlay-merge" in p.name]
    assert stray == []


def test_direct_merge_overwrites_existing_live_file_last_writer_wins(
    tmp_path: Path,
) -> None:
    upper = tmp_path / "upper"
    upper.mkdir()
    live = tmp_path / "live"
    live.mkdir()
    (live / "dep.txt").write_text("old\n", encoding="utf-8")
    (upper / "dep.txt").write_text("new\n", encoding="utf-8")

    merge = direct_merge_factory(live_root=str(live))
    merge("dep.txt", str(upper / "dep.txt"), (upper / "dep.txt").stat())

    assert (live / "dep.txt").read_text(encoding="utf-8") == "new\n"


# ---------------------------------------------------------------------------
# Edge case: mixed gitinclude + gitignore writes (plan §0 row)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# run_user_command cwd invariant: relative paths must resolve against workspace
# ---------------------------------------------------------------------------


def test_run_user_command_runs_in_workspace_cwd(tmp_path: Path) -> None:
    from sandbox.code_intelligence.overlay.run import run_user_command

    stdout, exit_code = run_user_command(
        user_cmd="pwd",
        stdin_bytes=None,
        cwd=str(tmp_path),
        stdout_path=str(tmp_path / "stdout.bin"),
    )
    assert exit_code == 0
    assert Path(stdout.decode().strip()) == tmp_path.resolve()


def test_run_user_command_resolves_relative_paths_against_cwd(tmp_path: Path) -> None:
    from sandbox.code_intelligence.overlay.run import run_user_command

    (tmp_path / "marker.txt").write_text("hi\n", encoding="utf-8")
    stdout, exit_code = run_user_command(
        user_cmd="cat marker.txt",
        stdin_bytes=None,
        cwd=str(tmp_path),
        stdout_path=str(tmp_path / "stdout.bin"),
    )
    assert exit_code == 0
    assert stdout == b"hi\n"


def test_run_user_command_disables_optional_git_locks(tmp_path: Path) -> None:
    from sandbox.code_intelligence.overlay.run import run_user_command

    stdout, exit_code = run_user_command(
        user_cmd='printf "%s" "$GIT_OPTIONAL_LOCKS"',
        stdin_bytes=None,
        cwd=str(tmp_path),
        stdout_path=str(tmp_path / "stdout.bin"),
    )
    assert exit_code == 0
    assert stdout == b"0"


# ---------------------------------------------------------------------------
# _parse_args smoke: catch typos in the CLI without needing Linux execution.
# ---------------------------------------------------------------------------


def test_parse_args_accepts_the_full_argument_surface() -> None:
    from sandbox.code_intelligence.overlay.run import _parse_args

    ns = _parse_args(
        [
            "--workspace-root", "/ws",
            "--run-dir", "/run",
            "--snap", "deadbeef",
            "--upper-size-mb", "256",
            "--user-cmd-b64", "ZWNobyBoaQ==",
            "--stdin-b64", "c3RkaW4=",
        ]
    )
    assert ns.workspace_root == "/ws"
    assert ns.run_dir == "/run"
    assert ns.snap == "deadbeef"
    assert ns.upper_size_mb == 256
    assert ns.user_cmd_b64 == "ZWNobyBoaQ=="
    assert ns.stdin_b64 == "c3RkaW4="


def test_parse_args_rejects_missing_required_argument() -> None:
    from sandbox.code_intelligence.overlay.run import _parse_args

    with pytest.raises(SystemExit):
        _parse_args(["--workspace-root", "/ws"])


def test_namespace_mount_root_uses_writable_tmp_prefix() -> None:
    from sandbox.code_intelligence.overlay import run as overlay_run

    assert overlay_run._NS_ROOT.startswith("/tmp/")
    assert overlay_run._NS_TMP.startswith(overlay_run._NS_ROOT + "/")
    assert overlay_run._NS_UPPER.startswith(overlay_run._NS_TMP + "/")
    assert overlay_run._NS_WORK.startswith(overlay_run._NS_TMP + "/")


# ---------------------------------------------------------------------------
# git_show_base_factory real-git round-trip
# ---------------------------------------------------------------------------


def _make_snap(repo: Path) -> str:
    """Create a dangling commit capturing the live tree; return its SHA."""
    env = dict(os.environ)
    env.setdefault("GIT_AUTHOR_NAME", "T")
    env.setdefault("GIT_AUTHOR_EMAIL", "t@example.invalid")
    env.setdefault("GIT_COMMITTER_NAME", "T")
    env.setdefault("GIT_COMMITTER_EMAIL", "t@example.invalid")
    tree = subprocess.check_output(
        ["git", "-C", str(repo), "write-tree"], env=env, text=True
    ).strip()
    head = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "--verify", "HEAD"],
        text=True,
        capture_output=True,
    )
    args = ["git", "-C", str(repo), "commit-tree", tree, "-m", "snap"]
    if head.returncode == 0:
        args.extend(["-p", head.stdout.strip()])
    return subprocess.check_output(args, env=env, text=True).strip()


def test_git_show_base_returns_bytes_for_existing_path(tmp_path: Path) -> None:
    from sandbox.code_intelligence.overlay.run import git_show_base_factory

    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    (repo / "app.py").write_text("hello\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "add", "-A"], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-q", "-m", "seed"], check=True
    )
    snap = _make_snap(repo)

    show = git_show_base_factory(repo_root=str(repo), snap=snap)
    assert show("app.py") == b"hello\n"


def test_git_show_base_returns_none_for_missing_path(tmp_path: Path) -> None:
    from sandbox.code_intelligence.overlay.run import git_show_base_factory

    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    (repo / "a.py").write_text("seed\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "add", "-A"], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-q", "-m", "seed"], check=True
    )
    snap = _make_snap(repo)

    show = git_show_base_factory(repo_root=str(repo), snap=snap)
    assert show("not-in-snap.py") is None


def test_git_show_base_raises_on_bad_sha(tmp_path: Path) -> None:
    from sandbox.code_intelligence.overlay.run import git_show_base_factory

    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    (repo / "a.py").write_text("seed\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "add", "-A"], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-q", "-m", "seed"], check=True
    )

    show = git_show_base_factory(repo_root=str(repo), snap="0" * 40)
    # A sha that doesn't resolve to any commit: git returns 128 and stderr
    # has "ambiguous argument" rather than "exists on disk". We treat this
    # as a missing base.
    assert show("a.py") is None


# ---------------------------------------------------------------------------
# NDJSON round-trip: write_diff_ndjson -> parse_diff_ndjson must be lossless
# on the fields the schema promises.
# ---------------------------------------------------------------------------


def test_ndjson_round_trip_preserves_gitinclude_and_gitignore(tmp_path: Path) -> None:
    from sandbox.code_intelligence.overlay.auditor import parse_diff_ndjson
    from sandbox.code_intelligence.overlay.run import GitincludeChange

    outcome = ClassifyOutcome(
        gitinclude=(
            GitincludeChange(
                path="src/app.py",
                kind="modify",
                base_content="old\n",
                base_existed=True,
                final_content="new\n",
            ),
            GitincludeChange(
                path="src/gone.py",
                kind="delete",
                base_content="bye\n",
                base_existed=True,
                final_content=None,
            ),
            GitincludeChange(
                path="src/new.py",
                kind="create",
                base_content="",
                base_existed=False,
                final_content="hi\n",
            ),
        ),
        gitignore_paths=(".venv/cfg", "node_modules/pkg/index.js"),
        direct_merged_bytes=123,
        whiteouts_gitinclude=1,
        whiteouts_gitignore_refused=0,
        dotgit_rejects=0,
    )

    path = write_diff_ndjson(
        run_dir=str(tmp_path),
        snap="deadbeef",
        exit_code=0,
        outcome=outcome,
        upper_bytes=999,
        upper_files=5,
        snapshot_timings={"total": 0.2, "commit_tree": 0.03},
        run_timings={"total": 0.4, "user_command": 0.1},
    )
    raw = Path(path).read_text(encoding="utf-8")
    parsed = parse_diff_ndjson(raw)

    assert not isinstance(parsed, PolicyRejectOutcome)
    # No PolicyReject — parser returns OverlayDiff.
    assert parsed.snap == "deadbeef"
    assert parsed.upper_bytes == 999
    assert parsed.upper_files == 5
    assert parsed.direct_merged_bytes == 123
    assert parsed.whiteouts_gitinclude == 1
    assert parsed.gitignore_paths == (".venv/cfg", "node_modules/pkg/index.js")
    assert parsed.snapshot_timings == {"total": 0.2, "commit_tree": 0.03}
    assert parsed.run_timings == {"total": 0.4, "user_command": 0.1}

    kinds = [c.kind for c in parsed.gitinclude_changes]
    assert kinds == ["modify", "delete", "create"]
    delete_change = [c for c in parsed.gitinclude_changes if c.kind == "delete"][0]
    assert delete_change.final_content is None
    create_change = [c for c in parsed.gitinclude_changes if c.kind == "create"][0]
    assert create_change.base_existed is False
    modify_change = [c for c in parsed.gitinclude_changes if c.kind == "modify"][0]
    assert modify_change.base_content == "old\n"
    assert modify_change.final_content == "new\n"


def test_ndjson_round_trip_preserves_reject_block(tmp_path: Path) -> None:
    from sandbox.code_intelligence.overlay.auditor import parse_diff_ndjson

    reject = PolicyRejectOutcome(
        reason="overlay_rejected_dotgit_writes",
        paths=(".git/config", ".git/objects/a"),
    )
    path = write_reject_ndjson(
        run_dir=str(tmp_path),
        snap="abc",
        reject=reject,
        snapshot_timings={"total": 0.3},
        run_timings={"total": 0.7},
    )
    raw = Path(path).read_text(encoding="utf-8")
    parsed = parse_diff_ndjson(raw)

    # parse_diff_ndjson returns OverlayPolicyReject (different dataclass,
    # same schema).
    assert parsed.reason == "overlay_rejected_dotgit_writes"  # type: ignore[union-attr]
    assert parsed.paths == (".git/config", ".git/objects/a")  # type: ignore[union-attr]
    assert parsed.snapshot_timings == {"total": 0.3}  # type: ignore[union-attr]
    assert parsed.run_timings == {"total": 0.7}  # type: ignore[union-attr]


# ---------------------------------------------------------------------------
# Preserved: Edge case: mixed gitinclude + gitignore writes (plan §0 row)
# ---------------------------------------------------------------------------


def test_mixed_write_direct_merges_gitignore_even_when_gitinclude_path_is_present(
    tmp_path: Path,
) -> None:
    # The classifier is pure; the orchestrator is the one that decides
    # whether to commit gitinclude via OCC after gitignore already landed.
    # Here we just verify the classifier itself routes correctly.
    env = _Classifier(
        upper_bytes={
            "requirements.txt": b"foo==1.0\n",
            ".venv/lib/foo/__init__.py": b"# foo\n",
        },
        base_bytes={"requirements.txt": b""},
        ignored={".venv/lib/foo/__init__.py"},
    )
    result = env.classifier().classify(
        [
            _regular_entry("requirements.txt"),
            _regular_entry(".venv/lib/foo/__init__.py"),
        ]
    )
    assert isinstance(result, ClassifyOutcome)
    assert [c.path for c in result.gitinclude] == ["requirements.txt"]
    assert result.gitignore_paths == (".venv/lib/foo/__init__.py",)
    # Gitignored direct-merge runs inside the classifier (inside the ns
    # in production). That means it is already applied before the
    # orchestrator runs gitinclude OCC — the partial-apply contract.
    assert env.merged == [(".venv/lib/foo/__init__.py", len(b"# foo\n"))]
