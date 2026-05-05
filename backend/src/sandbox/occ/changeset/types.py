"""Typed OCC changeset objects.

Source-tagged mutation intent objects for the layer-stack OCC path.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Literal, Union

ChangeSource = Literal["api_write", "api_edit", "overlay_capture"]


@dataclass(frozen=True)
class SearchReplaceEdit:
    """One exact-match edit anchor."""

    old_text: str
    new_text: str


@dataclass(frozen=True, init=False)
class Change:
    """Base mutation intent entering OCC."""

    path: str
    source: ChangeSource

    def __init__(self, path: str, *, source: ChangeSource) -> None:
        object.__setattr__(self, "path", str(path))
        object.__setattr__(self, "source", source)


@dataclass(frozen=True, init=False)
class WriteChange(Change):
    """Whole-file write intent.

    ``final_content`` is stored as bytes for layer-stack staging.
    """

    final_content: bytes
    base_hash: str | None
    create_only: bool

    def __init__(
        self,
        path: str,
        final_content: bytes | str,
        base_hash: str | None = None,
        create_only: bool = False,
        *,
        source: ChangeSource = "api_write",
    ) -> None:
        Change.__init__(self, path, source=source)
        payload = (
            final_content
            if isinstance(final_content, bytes)
            else final_content.encode("utf-8")
        )
        object.__setattr__(self, "final_content", payload)
        object.__setattr__(self, "base_hash", base_hash)
        object.__setattr__(self, "create_only", bool(create_only))

    def with_base_hash(self, base_hash: str | None) -> "WriteChange":
        return WriteChange(
            path=self.path,
            source=self.source,
            final_content=self.final_content,
            base_hash=base_hash,
            create_only=self.create_only,
        )


@dataclass(frozen=True, init=False)
class EditChange(Change):
    """Search/replace edit intent."""

    old_text: str
    new_text: str
    expected_occurrences: int

    def __init__(
        self,
        path: str,
        old_text: str | None = None,
        new_text: str | None = None,
        expected_occurrences: int = 1,
        *,
        source: ChangeSource = "api_edit",
    ) -> None:
        Change.__init__(self, path, source=source)
        if old_text is None:
            raise ValueError("EditChange requires old_text")
        if new_text is None:
            raise ValueError("EditChange requires new_text")
        object.__setattr__(self, "old_text", str(old_text))
        object.__setattr__(self, "new_text", str(new_text))
        object.__setattr__(self, "expected_occurrences", int(expected_occurrences))

    @property
    def edits(self) -> tuple[SearchReplaceEdit, ...]:
        return (SearchReplaceEdit(old_text=self.old_text, new_text=self.new_text),)


@dataclass(frozen=True, init=False)
class DeleteChange(Change):
    """Delete intent pinned to a base hash when known."""

    base_hash: str | None

    def __init__(
        self,
        path: str,
        base_hash: str | None = None,
        *,
        source: ChangeSource = "api_write",
    ) -> None:
        Change.__init__(self, path, source=source)
        object.__setattr__(self, "base_hash", base_hash)

    def with_base_hash(self, base_hash: str | None) -> "DeleteChange":
        return DeleteChange(path=self.path, source=self.source, base_hash=base_hash)


GatedChange = Union[WriteChange, EditChange, DeleteChange]


@dataclass(frozen=True, init=False)
class SymlinkChange(Change):
    """Replace path with symlink to target."""

    target: str

    def __init__(
        self,
        path: str,
        target: str,
        *,
        source: ChangeSource = "overlay_capture",
    ) -> None:
        Change.__init__(self, path, source=source)
        object.__setattr__(self, "target", str(target))


@dataclass(frozen=True, init=False)
class OpaqueDirChange(Change):
    """Prune children of path not in ``kept_children``."""

    kept_children: frozenset[str]

    def __init__(
        self,
        path: str,
        kept_children: frozenset[str],
        *,
        source: ChangeSource = "overlay_capture",
    ) -> None:
        Change.__init__(self, path, source=source)
        object.__setattr__(self, "kept_children", frozenset(kept_children))


class FileStatus(str, Enum):
    ACCEPTED = "accepted"
    COMMITTED = "committed"
    ABORTED_VERSION = "aborted_version"
    ABORTED_OVERLAP = "aborted_overlap"
    DROPPED = "dropped"
    REJECTED = "rejected"
    FAILED = "failed"


def is_published_status(status: FileStatus) -> bool:
    return status in {FileStatus.ACCEPTED, FileStatus.COMMITTED}


def is_success_status(status: FileStatus) -> bool:
    return status in {
        FileStatus.ACCEPTED,
        FileStatus.COMMITTED,
        FileStatus.DROPPED,
    }


@dataclass(frozen=True)
class FileResult:
    path: str
    status: FileStatus
    message: str = ""
    timings: dict[str, float] = field(default_factory=dict)


@dataclass(frozen=True)
class ChangesetResult:
    files: tuple[FileResult, ...]
    timings: dict[str, float] = field(default_factory=dict)
    published_manifest_version: int | None = None

    @property
    def success(self) -> bool:
        return all(is_success_status(f.status) for f in self.files)


__all__ = [
    "Change",
    "ChangeSource",
    "ChangesetResult",
    "DeleteChange",
    "EditChange",
    "FileResult",
    "FileStatus",
    "GatedChange",
    "OpaqueDirChange",
    "SearchReplaceEdit",
    "SymlinkChange",
    "WriteChange",
    "is_published_status",
    "is_success_status",
]
