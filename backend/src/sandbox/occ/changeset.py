"""Typed OCC changeset objects.

Source-tagged mutation intent objects for the layer-stack OCC path.
"""

from __future__ import annotations

from dataclasses import dataclass, field, replace
from enum import Enum
from pathlib import Path

from sandbox.layer_stack.manifest import Manifest


class ChangeSource(str, Enum):
    API_WRITE = "api_write"
    API_EDIT = "api_edit"
    OVERLAY_CAPTURE = "overlay_capture"

    def __str__(self) -> str:
        return self.value


@dataclass(frozen=True)
class Change:
    """Base mutation intent entering OCC."""

    path: str
    source: ChangeSource = ChangeSource.API_WRITE

    def __post_init__(self) -> None:
        object.__setattr__(self, "path", str(self.path))
        object.__setattr__(self, "source", ChangeSource(self.source))


@dataclass(frozen=True)
class WritePayload:
    """Write payload: eager bytes, an on-disk path, or both.

    At least one of ``content``/``content_path`` must be set; ``read_bytes``
    prefers the in-memory bytes and falls back to a disk read. Callers that
    need to avoid re-reads should cache the bytes themselves.
    """

    content: bytes | None = None
    content_path: str | None = None
    precomputed_hash: str | None = None

    def read_bytes(self) -> bytes:
        if self.content is not None:
            return self.content
        if self.content_path is None:
            raise ValueError("WritePayload requires content or content_path")
        return Path(self.content_path).read_bytes()


@dataclass(frozen=True, kw_only=True)
class WriteChange(Change):
    """Whole-file write intent.

    ``payload`` keeps transport details out of the mutation intent. Source
    adapters translate host/API inputs into in-memory or disk-backed payloads
    before constructing this value object.
    """

    payload: WritePayload
    base_hash: str | None = None

    @property
    def final_content(self) -> bytes:
        return self.payload.read_bytes()

    @property
    def content_path(self) -> str | None:
        return self.payload.content_path

    @property
    def precomputed_hash(self) -> str | None:
        return self.payload.precomputed_hash

    def with_base_hash(self, base_hash: str | None) -> WriteChange:
        return replace(self, base_hash=base_hash)


@dataclass(frozen=True)
class EditChange(Change):
    """Search/replace edit intent."""

    source: ChangeSource = ChangeSource.API_EDIT
    old_text: str | None = None
    new_text: str | None = None
    expected_occurrences: int = 1

    def __post_init__(self) -> None:
        super().__post_init__()
        if self.old_text is None:
            raise ValueError("EditChange requires old_text")
        if self.new_text is None:
            raise ValueError("EditChange requires new_text")
        object.__setattr__(self, "old_text", str(self.old_text))
        object.__setattr__(self, "new_text", str(self.new_text))
        object.__setattr__(self, "expected_occurrences", int(self.expected_occurrences))


@dataclass(frozen=True)
class DeleteChange(Change):
    """Delete intent pinned to a base hash when known."""

    base_hash: str | None = None

    def with_base_hash(self, base_hash: str | None) -> DeleteChange:
        return replace(self, base_hash=base_hash)


@dataclass(frozen=True)
class SymlinkChange(Change):
    """Replace path with symlink to target."""

    source: ChangeSource = ChangeSource.OVERLAY_CAPTURE
    target: str = ""

    def __post_init__(self) -> None:
        super().__post_init__()
        object.__setattr__(self, "target", str(self.target))


@dataclass(frozen=True)
class OpaqueDirChange(Change):
    """Prune lower-layer children of a directory."""

    source: ChangeSource = ChangeSource.OVERLAY_CAPTURE


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


# ---- prepared changesets ---------------------------------------------------


class RouteDecision(str, Enum):
    GATED = "gated"
    DIRECT = "direct"
    DROP = "drop"
    REJECT = "reject"


@dataclass(frozen=True)
class PreparedPathGroup:
    """Ordered changes for one normalized path and route decision."""

    path: str
    route: RouteDecision
    changes: tuple[Change, ...]
    message: str | None = None


@dataclass(frozen=True)
class CommitOptions:
    """Request-level OCC commit options.

    ``atomic`` defaults to ``True``: a multi-path changeset is published only
    if every path validates. If any path fails (ABORTED_OVERLAP,
    ABORTED_VERSION, FAILED, or REJECTED), no path lands. Callers that want
    best-effort partial publish must opt out explicitly with
    ``atomic=False``.
    """

    atomic: bool = True


@dataclass(frozen=True)
class PreparedChangeset:
    """Routed changeset consumed by the commit transaction."""

    snapshot: Manifest | None
    path_groups: tuple[PreparedPathGroup, ...]
    atomic: bool
    timings: dict[str, float] = field(default_factory=dict)


# ---- builders ------


def _eager_payload(content: bytes | str) -> WritePayload:
    if isinstance(content, bytes):
        return WritePayload(content=content)
    return WritePayload(content=content.encode("utf-8"))


def build_api_write_change(
    *,
    path: str,
    final_content: bytes | str,
    base_hash: str | None = None,
) -> WriteChange:
    """Build a source-tagged write change from the host write API."""
    return WriteChange(
        path=path,
        source=ChangeSource.API_WRITE,
        payload=_eager_payload(final_content),
        base_hash=base_hash,
    )


def build_overlay_write_change(
    *,
    path: str,
    final_content: bytes | None = None,
    content_path: str | None = None,
    precomputed_hash: str | None = None,
    source: ChangeSource = ChangeSource.OVERLAY_CAPTURE,
) -> WriteChange:
    """Build an overlay-captured full-file write without a caller base hash.

    When ``content_path`` is provided and ``final_content`` is None, the
    bytes stay on disk and the OCC stager streams them kernel-to-kernel.
    ``final_content`` is the bytes-based fallback for callers that don't
    have a content path on disk.
    """
    if content_path is not None and final_content is None:
        payload = WritePayload(
            content_path=str(content_path),
            precomputed_hash=precomputed_hash,
        )
    elif final_content is not None:
        payload = _eager_payload(final_content)
    else:
        raise ValueError("build_overlay_write_change needs final_content or content_path")
    return WriteChange(
        path=path,
        source=source,
        payload=payload,
        base_hash=None,
    )


def build_overlay_delete_change(
    *,
    path: str,
    base_hash: str | None = None,
    source: ChangeSource = ChangeSource.OVERLAY_CAPTURE,
) -> DeleteChange:
    """Build an overlay-captured delete whose base hash can be inferred later."""
    return DeleteChange(path=path, source=source, base_hash=base_hash)


__all__ = [
    "Change",
    "ChangeSource",
    "ChangesetResult",
    "CommitOptions",
    "DeleteChange",
    "EditChange",
    "FileResult",
    "FileStatus",
    "OpaqueDirChange",
    "PreparedChangeset",
    "PreparedPathGroup",
    "RouteDecision",
    "SymlinkChange",
    "WriteChange",
    "WritePayload",
    "build_api_write_change",
    "build_overlay_delete_change",
    "build_overlay_write_change",
    "is_published_status",
    "is_success_status",
]
