"""Overlay runtime policy constants and path gates."""

from __future__ import annotations

from collections.abc import Iterable

from .types import PolicyRejectOutcome, UpperEntry

REJECT_DOTGIT = "overlay_rejected_dotgit_writes"
REJECT_GITIGNORE_WHITEOUT = "overlay_refused_gitignore_whiteout"
REJECT_UNSUPPORTED_SYMLINK = "overlay_unsupported_symlink"
REJECT_UNSUPPORTED_OPAQUE_DIR = "overlay_unsupported_opaque_dir"
REJECT_NON_UTF8_GITINCLUDE = "overlay_non_utf8_gitinclude"
REJECT_UPPER_FULL = "overlay_upper_full"

IGNORABLE_DOTGIT_WRITES = frozenset({".git/index", ".git/index.lock"})

_REJECT_EXIT_BASE = 200
_REJECT_EXIT_CODES: dict[str, int] = {
    REJECT_DOTGIT: _REJECT_EXIT_BASE + 1,
    REJECT_GITIGNORE_WHITEOUT: _REJECT_EXIT_BASE + 2,
    REJECT_UNSUPPORTED_SYMLINK: _REJECT_EXIT_BASE + 4,
    REJECT_UNSUPPORTED_OPAQUE_DIR: _REJECT_EXIT_BASE + 5,
    REJECT_NON_UTF8_GITINCLUDE: _REJECT_EXIT_BASE + 6,
    REJECT_UPPER_FULL: _REJECT_EXIT_BASE + 7,
}


def reject_dotgit_writes(entries: Iterable[UpperEntry]) -> PolicyRejectOutcome | None:
    dotgit = [
        entry
        for entry in entries
        if entry.rel == ".git" or entry.rel.startswith(".git/")
    ]
    dotgit = [entry for entry in dotgit if entry.rel not in IGNORABLE_DOTGIT_WRITES]
    if not dotgit:
        return None
    return PolicyRejectOutcome(
        reason=REJECT_DOTGIT,
        paths=tuple(sorted(entry.rel for entry in dotgit)),
    )


def filter_ignorable_dotgit_writes(entries: Iterable[UpperEntry]) -> list[UpperEntry]:
    return [entry for entry in entries if entry.rel not in IGNORABLE_DOTGIT_WRITES]


def reject_exit_code(reason: str) -> int:
    return _REJECT_EXIT_CODES.get(reason, _REJECT_EXIT_BASE)


__all__ = [
    "IGNORABLE_DOTGIT_WRITES",
    "REJECT_DOTGIT",
    "REJECT_GITIGNORE_WHITEOUT",
    "REJECT_NON_UTF8_GITINCLUDE",
    "REJECT_UNSUPPORTED_OPAQUE_DIR",
    "REJECT_UNSUPPORTED_SYMLINK",
    "REJECT_UPPER_FULL",
    "filter_ignorable_dotgit_writes",
    "reject_dotgit_writes",
    "reject_exit_code",
]
