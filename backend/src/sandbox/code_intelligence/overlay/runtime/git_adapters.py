"""Git adapters used inside the overlay namespace."""

from __future__ import annotations

import os
import subprocess
import tempfile
import time
from collections.abc import Callable, Iterator
from typing import Any


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


def git_show_base_factory(
    *, repo_root: str, snap: str
) -> Callable[[str], bytes | None]:
    """Return a callable that reads ``git show <snap>:<rel>``."""

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
        if proc.returncode == 128 and "exists on disk" not in stderr:
            return None
        raise RuntimeError(
            f"git show {snap}:{rel} failed: rc={proc.returncode} stderr={stderr!r}"
        )

    return _show


def check_ignore_factory(*, repo_root: str) -> Callable[[list[str]], set[str]]:
    """Return a callable that batch-checks gitignore membership."""

    def _check(paths: list[str]) -> set[str]:
        if not paths:
            return set()
        ignored: set[str] = set()
        for chunk in _chunk_paths(paths, byte_limit=1024 * 1024):
            stdin_bytes = b"\0".join(path.encode("utf-8") for path in chunk) + b"\0"
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
            if proc.returncode not in (0, 1):
                stderr = proc.stderr.decode("utf-8", "replace")
                raise RuntimeError(
                    f"git check-ignore failed: rc={proc.returncode} stderr={stderr!r}"
                )
            fields = proc.stdout.split(b"\0")
            if fields and fields[-1] == b"":
                fields = fields[:-1]
            for i in range(0, len(fields), 4):
                record = fields[i : i + 4]
                if len(record) < 4:
                    break
                source, _line, _pattern, path = record
                if source:
                    ignored.add(path.decode("utf-8"))
        return ignored

    return _check


def _chunk_paths(paths: list[str], *, byte_limit: int) -> Iterator[list[str]]:
    chunk: list[str] = []
    size = 0
    for path in paths:
        plen = len(path.encode("utf-8")) + 1
        if chunk and size + plen > byte_limit:
            yield chunk
            chunk = []
            size = 0
        chunk.append(path)
        size += plen
    if chunk:
        yield chunk


__all__ = [
    "_record_timing",
    "build_live_snapshot_in_namespace",
    "check_ignore_factory",
    "git_show_base_factory",
]
