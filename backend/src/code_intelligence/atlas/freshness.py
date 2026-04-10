"""Atlas freshness checks using ledger events, content hashes, and TTL."""

from __future__ import annotations

import hashlib
import logging
from datetime import datetime, timezone
from pathlib import Path
from typing import TYPE_CHECKING

from code_intelligence.atlas.store import AtlasChunk
from team.context.canonicalize import canonicalize_scope

if TYPE_CHECKING:
    from code_intelligence.editing.ledger import Ledger

logger = logging.getLogger(__name__)

DEFAULT_ATLAS_MAX_AGE_SECONDS = 6 * 3600
MIN_COMPLETE_SCOPE_COVERAGE = 0.9


_CACHE_MAX = 4096
_hash_cache: dict[tuple[str, int, int], str] = {}


def content_hash(text: str) -> str:
    """Return the 16-char sha256 prefix for UTF-8 text."""
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


def _raw_hash_file(path: Path) -> str | None:
    """Hash a file as raw bytes, or return ``None`` if unreadable."""
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()[:16]
    except OSError:
        return None


def hash_file(path: str | Path) -> str | None:
    """Return the stat-cached content hash of a path, or ``None`` if missing."""
    p = Path(path)
    try:
        st = p.stat()
    except OSError:
        return None
    key = (str(p), st.st_mtime_ns, st.st_size)
    cached = _hash_cache.get(key)
    if cached is not None:
        return cached
    h = _raw_hash_file(p)
    if h is None:
        return None
    if len(_hash_cache) >= _CACHE_MAX:
        _hash_cache.pop(next(iter(_hash_cache)))
    _hash_cache[key] = h
    return h


def _clear_hash_cache() -> None:
    """Clear the stat cache."""
    _hash_cache.clear()


def hash_paths_under(scope_paths: list[str], repo_root: str | Path) -> dict[str, str]:
    """Hash every regular file under each scope path."""
    out: dict[str, str] = {}
    for target in _iter_scope_files(scope_paths, repo_root):
        h = hash_file(target)
        if h is not None:
            out[str(target)] = h
    return out


def _iter_scope_files(
    scope_paths: list[str], repo_root: str | Path
) -> list[Path]:
    """Resolve the files currently under *scope_paths*."""
    root = Path(repo_root)
    files: list[Path] = []
    for raw in scope_paths:
        rel = raw.strip().rstrip("/")
        if not rel:
            continue
        base = Path(rel) if Path(rel).is_absolute() else root / rel
        try:
            target = base.resolve()
        except OSError:
            target = base
        if target.is_file():
            files.append(target)
            continue
        if not target.is_dir():
            continue
        for p in target.rglob("*"):
            if p.is_file():
                files.append(p)
    return files


def is_subsystem_stale(chunk: AtlasChunk, changed_files: set[str]) -> bool:
    """Return True if any file under the chunk's scope is in *changed_files*."""
    if not changed_files:
        return False
    target_paths = _target_paths(chunk)
    if not target_paths:
        return True
    for path in changed_files:
        path = _normalise_changed_path(path, chunk.repo_root)
        for scope in target_paths:
            if path == scope or path.startswith(scope.rstrip("/") + "/"):
                return True
    return False


def changes_since_chunk(chunk: AtlasChunk, ledger: "Ledger") -> set[str]:
    """Return file paths touched after the chunk's cutoff."""
    cutoff = _ledger_cutoff(chunk)
    if cutoff is None:
        return set()
    entries = ledger.changes_since(cutoff)
    raw_root = (chunk.repo_root or "").rstrip("/")
    resolved_root = _resolve_once(raw_root) if raw_root else ""
    out: set[str] = set()
    for entry in entries:
        out.add(_normalise_ledger_path(entry.file_path, resolved_root, raw_root))
    return out


def _ledger_cutoff(chunk: AtlasChunk) -> float | None:
    if chunk.snapshot_time and chunk.snapshot_time > 0:
        return float(chunk.snapshot_time)
    if chunk.updated_at is not None:
        return _as_utc(chunk.updated_at).timestamp()
    return None


def is_chunk_fresh(
    chunk: AtlasChunk,
    *,
    ledger: "Ledger | None" = None,
    max_age_seconds: float | None = None,
) -> bool:
    """Return True iff *chunk* can be proven fresh."""
    fresh, _ = freshness_status(
        chunk,
        ledger=ledger,
        max_age_seconds=max_age_seconds,
    )
    return fresh


def freshness_status(
    chunk: AtlasChunk,
    *,
    ledger: "Ledger | None" = None,
    max_age_seconds: float | None = None,
) -> tuple[bool, str | None]:
    """Return whether a chunk is fresh plus the first stale reason."""
    if max_age_seconds is not None and chunk.updated_at is not None:
        now = datetime.now(timezone.utc)
        age = (now - _as_utc(chunk.updated_at)).total_seconds()
        if age > max_age_seconds:
            return False, (
                "atlas brief exceeded the max reuse age and must be refreshed"
            )

    cutoff = _ledger_cutoff(chunk)
    if ledger is not None and cutoff is not None:
        touched = changes_since_chunk(chunk, ledger)
        if not is_subsystem_stale(chunk, touched):
            return True, None
        return False, (
            "ledger recorded edits under this scope since the chunk snapshot"
        )

    if chunk.content_hashes:
        for path, stored in chunk.content_hashes.items():
            current = hash_file(path)
            if current is None or current != stored:
                return False, (
                    "content hashes diverged from the working tree under this scope"
                )
        target_paths = _target_paths(chunk)
        if target_paths and chunk.repo_root:
            current_files = {
                str(p) for p in _iter_scope_files(target_paths, chunk.repo_root)
            }
            tracked = set(chunk.content_hashes.keys())
            if current_files - tracked:
                return False, (
                    "new files appeared under this scope since the chunk was written"
                )
        return True, None

    return False, (
        "cannot prove freshness: no ledger visibility and no stored content hashes"
    )


def chunk_reuse_status(
    chunk: AtlasChunk,
    *,
    ledger: "Ledger | None" = None,
    max_age_seconds: float | None = DEFAULT_ATLAS_MAX_AGE_SECONDS,
    min_scope_coverage: float = MIN_COMPLETE_SCOPE_COVERAGE,
) -> tuple[bool, str | None]:
    """Return whether the planner should reuse a cached atlas chunk.

    Planner reuse is stricter than freshness alone: an atlas entry can
    be fresh on disk and still be too incomplete to trust as structural
    context. In that case callers should refresh it, not propagate a
    partial brief.
    """
    fresh, reason = freshness_status(
        chunk,
        ledger=ledger,
        max_age_seconds=max_age_seconds,
    )
    if not fresh:
        return False, reason
    return brief_reuse_status(
        chunk.brief,
        min_scope_coverage=min_scope_coverage,
    )


def _target_paths(chunk: AtlasChunk) -> list[str]:
    raw = chunk.brief.get("target_paths") if isinstance(chunk.brief, dict) else None
    if not isinstance(raw, list):
        return []
    out: list[str] = []
    for p in raw:
        if isinstance(p, str) and p.strip():
            out.append(p.strip().replace("\\", "/").removeprefix("./").rstrip("/"))
    return out


_resolved_root_cache: dict[str, str] = {}


def _resolve_once(root: str) -> str:
    """Resolve ``root`` once per process."""
    if not root:
        return ""
    cached = _resolved_root_cache.get(root)
    if cached is not None:
        return cached
    try:
        resolved = str(Path(root).resolve())
    except OSError:
        resolved = root
    _resolved_root_cache[root] = resolved
    return resolved


def _normalise_changed_path(path: str, repo_root: str) -> str:
    """Best-effort repo-relative normalization for caller-provided paths."""
    cleaned = path.strip().replace("\\", "/").removeprefix("./").rstrip("/")
    if not cleaned:
        return cleaned
    candidate = Path(cleaned)
    if repo_root and candidate.is_absolute():
        raw_root = repo_root.rstrip("/")
        resolved_root = _resolve_once(raw_root)
        as_posix = candidate.as_posix()
        for root in (resolved_root, raw_root):
            if root and as_posix.startswith(root + "/"):
                return as_posix[len(root) + 1 :]
        return as_posix
    return candidate.as_posix()


def _normalise_ledger_path(
    path: str, resolved_root: str, raw_root: str
) -> str:
    """Strip a matching root prefix from a ledger entry path."""
    for root in (resolved_root, raw_root):
        if root and path.startswith(root + "/"):
            return path[len(root) + 1 :]
    return path


def brief_reuse_status(
    brief: dict[str, object] | None,
    *,
    min_scope_coverage: float,
) -> tuple[bool, str | None]:
    brief = brief if isinstance(brief, dict) else {}
    if _is_explicit_empty_area_brief(brief):
        return True, None

    coverage = brief.get("scope_coverage")
    if not isinstance(coverage, (int, float)):
        return False, "atlas brief is missing scope_coverage and cannot be trusted"
    if float(coverage) < min_scope_coverage:
        return False, (
            f"atlas brief coverage {float(coverage):.2f} is below the reuse threshold"
        )

    if _normalised_subdivisions(brief.get("suggested_subdivisions")):
        return False, (
            "atlas brief requested further subdivision and should be refreshed"
        )

    gaps = brief.get("gaps")
    if isinstance(gaps, str) and gaps.strip():
        return False, "atlas brief contains unresolved gaps and should be refreshed"
    return True, None


def _is_explicit_empty_area_brief(brief: dict[str, object]) -> bool:
    coverage = brief.get("scope_coverage")
    if not isinstance(coverage, (int, float)) or float(coverage) != 0.0:
        return False
    files = brief.get("files")
    if not isinstance(files, list) or files:
        return False
    return not _normalised_subdivisions(brief.get("suggested_subdivisions"))


def _normalised_subdivisions(raw: object) -> list[str]:
    if not isinstance(raw, list):
        return []
    return [p.strip() for p in raw if isinstance(p, str) and p.strip()]


def _as_utc(dt: datetime) -> datetime:
    if dt.tzinfo is None:
        return dt.replace(tzinfo=timezone.utc)
    return dt.astimezone(timezone.utc)


def canonical_subsystem_key(paths: list[str]) -> str:
    """Compute the subsystem key the atlas uses for a list of paths."""
    return canonicalize_scope(paths)
