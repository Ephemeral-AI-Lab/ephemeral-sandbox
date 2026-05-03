"""Apply raw overlay changes through OCC-owned merge policy."""

from __future__ import annotations

import os
import subprocess
from collections.abc import Callable, Sequence
from dataclasses import dataclass

from sandbox.occ.changeset.types import ChangesetResult, UpperChangeLike
from sandbox.occ.content.hashing import content_hash
from sandbox.occ.content.manager import ContentManager
from sandbox.occ.types import OperationChange, OperationResult


CommitFn = Callable[
    [Sequence[OperationChange]],
    OperationResult,
]

_ARGV_OVERFLOW_SIGNALS = (
    "argument list too long",
    "checked batch apply failed",
    "argv_too_large",
)


def apply_changeset(
    upper_changes: Sequence[UpperChangeLike],
    *,
    workspace_root: str,
    content: ContentManager,
    commit: CommitFn,
) -> ChangesetResult:
    """Apply one raw upperdir changeset.

    Overlay emits bytes and overlayfs kinds. This function owns the git policy:
    drop ``.git`` writes, direct-merge gitignored changes, and ledger tracked
    UTF-8 regular/delete changes through strict-base OCC.
    """
    changes = [_normalize_change(change) for change in upper_changes]
    changes = [change for change in changes if not _is_dotgit(change.rel)]
    if not changes:
        return ChangesetResult(success=True, status="noop")

    workspace_changes = [change for change in changes if not _is_external(change.rel)]
    ignored = _check_ignored(workspace_root, workspace_changes)

    direct_changes = [
        change for change in changes if _is_external(change.rel) or change.rel in ignored
    ]
    ledger_changes = [
        change for change in workspace_changes if change.rel not in ignored
    ]

    direct_merged = _apply_direct_changes(content, direct_changes, changes, workspace_root)
    operation_changes, conflict = _to_operation_changes(ledger_changes, workspace_root)
    if conflict is not None:
        conflict_reason, conflict_file = conflict
        return ChangesetResult(
            success=False,
            status="failed",
            direct_merged=tuple(direct_merged),
            conflict_reason=conflict_reason,
            conflict_file=conflict_file,
        )
    if not operation_changes:
        return ChangesetResult(
            success=True,
            status="noop",
            direct_merged=tuple(direct_merged),
        )

    try:
        operation_result = commit(operation_changes)
    except RuntimeError as exc:
        if _looks_like_argv_overflow(str(exc)):
            return _transport_failure(
                str(exc),
                direct_merged=tuple(direct_merged),
                conflict_file=operation_changes[0].file_path if operation_changes else None,
            )
        raise

    if operation_result.success:
        return ChangesetResult(
            success=True,
            status=operation_result.status,
            ledgered=tuple(change.file_path for change in operation_changes),
            direct_merged=tuple(direct_merged),
            timings=dict(operation_result.timings),
        )
    return ChangesetResult(
        success=False,
        status=operation_result.status,
        direct_merged=tuple(direct_merged),
        conflict_reason=(
            "argv_too_large"
            if _looks_like_argv_overflow(operation_result.conflict_reason)
            else "patch_failed"
        ),
        conflict_file=operation_result.conflict_file,
        timings=dict(operation_result.timings),
    )


def _normalize_change(change: UpperChangeLike) -> UpperChangeLike:
    rel = str(change.rel).replace("\\", "/").lstrip("/")
    return _Change(
        rel=rel,
        kind=str(change.kind),
        base_bytes=change.base_bytes,
        upper_bytes=change.upper_bytes,
        base_existed=change.base_existed,
    )


@dataclass(frozen=True)
class _Change:
    rel: str
    kind: str
    base_bytes: bytes | None
    upper_bytes: bytes | None
    base_existed: bool


def _is_dotgit(rel: str) -> bool:
    return rel == ".git" or rel.startswith(".git/")


def _is_external(rel: str) -> bool:
    return os.path.isabs(rel) or rel == ".." or rel.startswith("../")


def _check_ignored(workspace_root: str, changes: Sequence[UpperChangeLike]) -> set[str]:
    paths = [
        f"{change.rel}/" if change.kind == "opaque_dir" else change.rel
        for change in changes
    ]
    if not paths:
        return set()
    ignored: set[str] = set()
    for chunk in _chunk_paths(paths, byte_limit=1024 * 1024):
        stdin_bytes = b"\0".join(path.encode("utf-8") for path in chunk) + b"\0"
        proc = subprocess.run(
            [
                "git",
                "-C",
                workspace_root,
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
                ignored.add(path.decode("utf-8").rstrip("/"))
    return ignored


def _chunk_paths(paths: list[str], *, byte_limit: int):
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


def _apply_direct_changes(
    content: ContentManager,
    direct_changes: Sequence[UpperChangeLike],
    all_changes: Sequence[UpperChangeLike],
    workspace_root: str,
) -> list[str]:
    merged: list[str] = []
    for change in direct_changes:
        live_path = _live_path(workspace_root, change.rel)
        if change.kind == "regular":
            content.write_bytes(live_path, change.upper_bytes or b"")
        elif change.kind == "whiteout":
            content.delete_path(live_path)
        elif change.kind == "symlink":
            content.make_symlink(live_path, (change.upper_bytes or b"").decode("utf-8"))
        elif change.kind == "opaque_dir":
            _narrow_prune_opaque(content, change, all_changes, live_path)
        else:
            continue
        merged.append(live_path)
    return merged


def _narrow_prune_opaque(
    content: ContentManager,
    change: UpperChangeLike,
    all_changes: Sequence[UpperChangeLike],
    live_path: str,
) -> None:
    prefix = f"{change.rel}/"
    keep = {
        rest.split("/", 1)[0]
        for item in all_changes
        if item.rel.startswith(prefix)
        for rest in [item.rel[len(prefix) :]]
        if rest
    }
    for child in content.list_child_names(live_path):
        if child not in keep:
            content.delete_path(f"{live_path}/{child}")


def _to_operation_changes(
    changes: Sequence[UpperChangeLike],
    workspace_root: str,
) -> tuple[list[OperationChange], tuple[str, str | None] | None]:
    op_changes: list[OperationChange] = []
    for change in changes:
        live_path = _live_path(workspace_root, change.rel)
        if change.kind in {"symlink", "opaque_dir"}:
            return [], ("patch_failed", live_path)
        if change.kind == "whiteout":
            if not change.base_existed:
                continue
            try:
                base_text = (change.base_bytes or b"").decode("utf-8")
            except UnicodeDecodeError:
                return [], ("patch_failed", live_path)
            op_changes.append(
                OperationChange(
                    file_path=live_path,
                    base_content=base_text,
                    base_hash=content_hash(base_text),
                    final_content=None,
                    base_existed=True,
                    strict_base=True,
                )
            )
            continue
        if change.kind != "regular":
            continue
        upper_bytes = change.upper_bytes or b""
        if change.base_existed and change.base_bytes == upper_bytes:
            continue
        try:
            final_text = upper_bytes.decode("utf-8")
            base_text = (change.base_bytes or b"").decode("utf-8")
        except UnicodeDecodeError:
            return [], ("patch_failed", live_path)
        op_changes.append(
            OperationChange(
                file_path=live_path,
                base_content=base_text,
                base_hash=content_hash(base_text) if change.base_existed else "",
                final_content=final_text,
                base_existed=change.base_existed,
                strict_base=True,
            )
        )
    return op_changes, None


def _live_path(workspace_root: str, rel: str) -> str:
    if os.path.isabs(rel):
        return rel
    return f"{workspace_root.rstrip('/')}/{rel.lstrip('/')}"


def _transport_failure(
    message: str,
    *,
    direct_merged: tuple[str, ...],
    conflict_file: str | None,
) -> ChangesetResult:
    del message
    return ChangesetResult(
        success=False,
        status="failed",
        direct_merged=direct_merged,
        conflict_reason="argv_too_large",
        conflict_file=conflict_file,
    )


def _looks_like_argv_overflow(message: str) -> bool:
    if not message:
        return False
    lowered = message.lower()
    return any(signal in lowered for signal in _ARGV_OVERFLOW_SIGNALS)


__all__ = ["ChangesetResult", "UpperChangeLike", "apply_changeset"]
