"""Tests for the cached ``git check-ignore`` oracle (Step 1 of the gate simplification)."""

from __future__ import annotations

import pytest

from sandbox.occ.content.gitignore_oracle import GitignoreOracle, RunOutcome


def _make_run(ignored_paths: set[str], counter: dict[str, int]):
    """Return a fake ``run`` that emits the verbose ``git check-ignore`` format."""

    def run(argv: list[str], stdin_bytes: bytes) -> RunOutcome:
        counter["calls"] = counter.get("calls", 0) + 1
        # Stdin is a NUL-separated list of paths; emit four-field records:
        # source, line, pattern, path. ``source`` empty == not ignored.
        paths = [p for p in stdin_bytes.split(b"\0") if p]
        records: list[bytes] = []
        for raw_path in paths:
            decoded = raw_path.decode("utf-8")
            if decoded.rstrip("/") in ignored_paths:
                records.extend([b".gitignore", b"1", b"pat", raw_path])
            else:
                records.extend([b"", b"", b"", raw_path])
        stdout = b"\0".join(records) + b"\0" if records else b""
        return RunOutcome(returncode=0 if records else 1, stdout=stdout, stderr=b"")

    return run


def test_is_ignored_caches_result_second_call_subprocess_free() -> None:
    counter: dict[str, int] = {}
    oracle = GitignoreOracle(
        "/repo",
        run=_make_run({"build/out.o"}, counter),
    )

    assert oracle.is_ignored("build/out.o") is True
    assert counter["calls"] == 1
    # Same path queried again — must hit cache and not invoke ``run``.
    assert oracle.is_ignored("build/out.o") is True
    assert counter["calls"] == 1


def test_filter_ignored_batches_uncached_paths() -> None:
    counter: dict[str, int] = {}
    oracle = GitignoreOracle(
        "/repo",
        run=_make_run({"build/a.o", "dist/b.so"}, counter),
    )

    result = oracle.filter_ignored(
        ["src/main.py", "build/a.o", "dist/b.so", "src/lib.py"],
    )
    assert result == {"build/a.o", "dist/b.so"}
    assert counter["calls"] == 1

    # All four are now cached: a second batch should not shell out.
    result_again = oracle.filter_ignored(
        ["src/main.py", "build/a.o", "dist/b.so", "src/lib.py"],
    )
    assert result_again == {"build/a.o", "dist/b.so"}
    assert counter["calls"] == 1


def test_filter_ignored_only_queries_uncached() -> None:
    counter: dict[str, int] = {}
    oracle = GitignoreOracle(
        "/repo",
        run=_make_run({"build/a.o"}, counter),
    )

    oracle.is_ignored("build/a.o")
    assert counter["calls"] == 1

    oracle.filter_ignored(["build/a.o", "src/lib.py"])  # only src/lib.py is new
    assert counter["calls"] == 2


def test_run_failure_raises() -> None:
    def run(argv: list[str], stdin_bytes: bytes) -> RunOutcome:
        return RunOutcome(returncode=128, stdout=b"", stderr=b"fatal: not a git repository")

    oracle = GitignoreOracle("/not-a-repo", run=run)
    with pytest.raises(RuntimeError, match="git check-ignore failed"):
        oracle.is_ignored("any/path")


def test_unknown_path_caches_negative_result() -> None:
    counter: dict[str, int] = {}
    oracle = GitignoreOracle("/repo", run=_make_run(set(), counter))

    assert oracle.is_ignored("src/main.py") is False
    assert counter["calls"] == 1
    # A negative answer must also be cached.
    assert oracle.is_ignored("src/main.py") is False
    assert counter["calls"] == 1
