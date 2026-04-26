"""OCC-gated mutation facade helpers for CodeIntelligenceService."""

from __future__ import annotations

import logging
from collections.abc import Sequence
from typing import Any

from code_intelligence.mutations.patcher import Patcher
from code_intelligence.mutations.write_coordinator import CommitOperation, WriteCoordinator
from code_intelligence.core.hashing import content_hash
from code_intelligence.mutations.content_manager import ContentManager
from code_intelligence.core.types import (
    DeleteSpec,
    EditRequest,
    EditResult,
    EditSpec,
    MoveSpec,
    OperationChange,
    OperationResult,
    WriteSpec,
)

logger = logging.getLogger(__name__)


class _CommitSpecRequest:
    """Normalized internal high-level commit request."""

    def __init__(
        self,
        *,
        op: str,
        specs: Sequence[Any],
        agent_id: str = "",
        description: str = "",
    ) -> None:
        self.op = op
        self.specs = specs
        self.agent_id = agent_id
        self.description = description


class MutationService:
    """Plans and commits file mutations through WriteCoordinator."""

    def __init__(
        self,
        *,
        content: ContentManager,
        write_coordinator: WriteCoordinator,
        patcher: Patcher,
    ) -> None:
        self._content = content
        self._write_coordinator = write_coordinator
        self.patcher = patcher

    def apply_edit(self, request: EditRequest) -> EditResult:
        """Apply a single search/replace edit through the service helper path."""
        current, existed = self._content.read(request.file_path, allow_missing=True)
        if not existed:
            return EditResult(
                success=False,
                file_path=request.file_path,
                message=f"Path does not exist: {request.file_path}",
            )
        if request.old_text not in current:
            return EditResult(
                success=False,
                file_path=request.file_path,
                message="Search text not found",
            )
        new_content = current.replace(request.old_text, request.new_text, 1)
        operation = self._write_coordinator.commit_operation_against_base(
            [
                OperationChange(
                    file_path=request.file_path,
                    base_content=current,
                    base_hash=content_hash(current),
                    final_content=new_content,
                    base_existed=True,
                )
            ],
            agent_id=request.agent_id,
            edit_type="edit",
            description=request.description,
        )
        if operation.files:
            return operation.files[0]
        return EditResult(
            success=operation.success,
            file_path=request.file_path,
            message=operation.conflict_reason,
            conflict=bool(operation.conflict_file),
            conflict_reason=operation.status if operation.conflict_file else "",
            timings=dict(operation.timings),
        )

    def commit_operation_against_base(
        self,
        changes: Sequence[OperationChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> OperationResult:
        return self._write_coordinator.commit_operation_against_base(
            changes,
            agent_id=agent_id,
            edit_type=edit_type,
            description=description,
        )

    def commit_specs_many(
        self,
        requests: Sequence[dict[str, Any]],
    ) -> list[OperationResult]:
        """Plan and commit many typed file mutations with batched sandbox I/O."""
        normalized = [
            _CommitSpecRequest(
                op=str(req.get("op") or ""),
                specs=tuple(req.get("specs") or ()),
                agent_id=str(req.get("agent_id") or ""),
                description=str(req.get("description") or ""),
            )
            for req in requests
        ]
        if not normalized:
            return []
        if any(_request_has_folder_spec(req) for req in normalized):
            return [self._commit_specs_direct(req) for req in normalized]
        if len(normalized) == 1:
            req = normalized[0]
            return [self._commit_specs_direct(req)]

        read_paths = self._commit_spec_read_paths(normalized)
        try:
            base_by_path = self._content.read_many(read_paths, allow_missing=True)
        except Exception:
            logger.debug("batched commit planning read failed", exc_info=True)
            return [self._commit_specs_direct(req) for req in normalized]

        operations: list[CommitOperation | None] = []
        results: list[OperationResult | None] = [None] * len(normalized)
        for idx, req in enumerate(normalized):
            changes, early = self._commit_spec_changes_from_base(req, base_by_path)
            if early is not None:
                results[idx] = early
                operations.append(None)
                continue
            operations.append(
                CommitOperation(
                    changes=tuple(changes),
                    agent_id=req.agent_id,
                    edit_type=f"{req.op}_file",
                    description=req.description,
                )
            )

        commit_ops = [op for op in operations if op is not None]
        committed = self._write_coordinator.commit_many_operations_against_base(commit_ops)
        committed_iter = iter(committed)
        for idx, op in enumerate(operations):
            if op is None:
                continue
            results[idx] = next(committed_iter)
        return [r for r in results if r is not None]

    # -- Typed mutation APIs (OCC-gated, batch-capable) ----------------------

    def write_file(
        self,
        specs: Sequence[WriteSpec] | WriteSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        """Write one or more files through a single OCC-gated batch commit.

        ``WriteSpec(overwrite=True)`` replaces an existing file via a
        strict-base rewrite; ``overwrite=False`` requires the path to be
        absent at commit time. All specs in the batch land atomically or
        none land — any slot's ``aborted_version`` aborts the whole batch.
        """
        normalized = [specs] if isinstance(specs, WriteSpec) else list(specs)
        base_by_path = self._content.read_many(
            [str(spec.file_path) for spec in normalized],
            allow_missing=True,
        )
        changes = self._write_specs_to_changes_from_base(normalized, base_by_path)
        return self._write_coordinator.commit_operation_against_base(
            changes,
            agent_id=agent_id,
            edit_type="write_file",
            description=description,
        )

    def edit_file(
        self,
        specs: Sequence[EditSpec] | EditSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        """Apply search/replace (or line-range) edits to one or more files.

        Each :class:`EditSpec` is resolved against a plan-time base read
        from :class:`ContentManager`; edits are applied host-side via
        :class:`Patcher`. Any spec whose edits cannot be applied is
        surfaced as a failure in the returned :class:`OperationResult`
        without touching disk.
        """
        normalized = [specs] if isinstance(specs, EditSpec) else list(specs)
        base_by_path = self._content.read_many(
            [str(spec.file_path) for spec in normalized],
            allow_missing=True,
        )
        changes, early_failure = self._edit_specs_to_changes_from_base(
            normalized,
            base_by_path,
        )
        if early_failure is not None:
            return early_failure
        return self._write_coordinator.commit_operation_against_base(
            changes,
            agent_id=agent_id,
            edit_type="edit_file",
            description=description,
        )

    def delete_file(
        self,
        paths: Sequence[str | DeleteSpec],
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        """Delete one or more files through the OCC-gated commit path.

        Reads each current content through :class:`ContentManager`, builds
        one ``OperationChange(final_content=None)`` per path, and submits
        the whole list as one batch. The coordinator's delete branch
        requires ``current_hash == base_hash`` exactly — any drift aborts
        with ``aborted_version`` (no merge fallback for deletes).
        """
        resolved_paths: list[str] = []
        for item in paths:
            if isinstance(item, DeleteSpec):
                if item.is_folder:
                    try:
                        resolved_paths.extend(self._content.list_folder_files(item.path))
                    except FileNotFoundError:
                        return _not_found_result(item.path)
                    except NotADirectoryError:
                        return _not_a_directory_result(item.path)
                    continue
                resolved_paths.append(item.path)
            else:
                resolved_paths.append(str(item))

        base_by_path = self._content.read_many(resolved_paths, allow_missing=True)
        changes, early_failure = self._delete_paths_to_changes_from_base(
            resolved_paths,
            base_by_path,
        )
        if early_failure is not None:
            return early_failure
        return self._write_coordinator.commit_operation_against_base(
            changes,
            agent_id=agent_id,
            edit_type="delete_file",
            description=description,
        )

    def move_file(
        self,
        specs: Sequence[MoveSpec],
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        """Atomically move one or more files through the OCC-gated path.

        Each :class:`MoveSpec` expands to a delete-src + create-dst pair
        of :class:`OperationChange` entries; the whole list is submitted
        as one batch so sorted-path locks, two-pass resolve-then-apply,
        and TimeMachine rollback make the moves atomic across every
        spec in the batch.
        """
        normalized: list[MoveSpec] = []
        for spec in specs:
            if not spec.is_folder:
                normalized.append(spec)
                continue
            try:
                members = self._content.list_folder_files(spec.src_path)
            except FileNotFoundError:
                return _not_found_result(spec.src_path)
            except NotADirectoryError:
                return _not_a_directory_result(spec.src_path)
            src_prefix_len = len(spec.src_path)
            normalized.extend(
                MoveSpec(
                    src_path=member,
                    dst_path=spec.dst_path + member[src_prefix_len:],
                    overwrite=spec.overwrite,
                )
                for member in members
            )
        read_paths: list[str] = []
        for spec in normalized:
            if spec.src_path == spec.dst_path:
                return _identical_paths_result(spec.src_path)
            read_paths.extend((str(spec.src_path), str(spec.dst_path)))
        base_by_path = self._content.read_many(read_paths, allow_missing=True)
        changes, early_failure = self._move_specs_to_changes_from_base(
            normalized,
            base_by_path,
        )
        if early_failure is not None:
            return early_failure
        return self._write_coordinator.commit_operation_against_base(
            changes,
            agent_id=agent_id,
            edit_type="move_file",
            description=description,
        )

    # -- Spec -> OperationChange adapters ------------------------------------

    def _commit_specs_direct(self, req: _CommitSpecRequest) -> OperationResult:
        if req.op == "write":
            return self.write_file(
                req.specs,
                agent_id=req.agent_id,
                description=req.description,
            )
        if req.op == "edit":
            return self.edit_file(
                req.specs,
                agent_id=req.agent_id,
                description=req.description,
            )
        if req.op == "delete":
            return self.delete_file(
                list(req.specs),
                agent_id=req.agent_id,
                description=req.description,
            )
        if req.op == "move":
            return self.move_file(
                req.specs,
                agent_id=req.agent_id,
                description=req.description,
            )
        return OperationResult(
            success=False,
            status="failed",
            files=(),
            conflict_file=None,
            conflict_reason=f"unsupported commit op: {req.op}",
            timings={},
        )

    def _commit_spec_read_paths(self, requests: Sequence[_CommitSpecRequest]) -> list[str]:
        paths: list[str] = []
        for req in requests:
            if req.op == "write":
                paths.extend(str(spec.file_path) for spec in req.specs)
            elif req.op == "edit":
                paths.extend(str(spec.file_path) for spec in req.specs)
            elif req.op == "delete":
                paths.extend(_delete_spec_path(path) for path in req.specs)
            elif req.op == "move":
                for spec in req.specs:
                    paths.append(str(spec.src_path))
                    paths.append(str(spec.dst_path))
        return list(dict.fromkeys(paths))

    def _commit_spec_changes_from_base(
        self,
        req: _CommitSpecRequest,
        base_by_path: dict[str, tuple[str, bool]],
    ) -> tuple[list[OperationChange], OperationResult | None]:
        if req.op == "write":
            return self._write_specs_to_changes_from_base(req.specs, base_by_path), None
        if req.op == "edit":
            return self._edit_specs_to_changes_from_base(req.specs, base_by_path)
        if req.op == "delete":
            return self._delete_paths_to_changes_from_base(
                [_delete_spec_path(path) for path in req.specs],
                base_by_path,
            )
        if req.op == "move":
            return self._move_specs_to_changes_from_base(req.specs, base_by_path)
        return [], OperationResult(
            success=False,
            status="failed",
            files=(),
            conflict_file=None,
            conflict_reason=f"unsupported commit op: {req.op}",
            timings={},
        )

    def _write_specs_to_changes_from_base(
        self,
        specs: Sequence[Any],
        base_by_path: dict[str, tuple[str, bool]],
    ) -> list[OperationChange]:
        changes: list[OperationChange] = []
        for spec in specs:
            current, existed = base_by_path.get(str(spec.file_path), ("", False))
            if spec.overwrite:
                changes.append(
                    OperationChange(
                        file_path=spec.file_path,
                        base_content=current,
                        base_hash=content_hash(current) if existed else "",
                        final_content=spec.content,
                        base_existed=existed,
                        strict_base=True,
                    )
                )
            else:
                changes.append(
                    OperationChange(
                        file_path=spec.file_path,
                        base_content="",
                        base_hash="",
                        final_content=spec.content,
                        base_existed=False,
                    )
                )
        return changes

    def _edit_specs_to_changes_from_base(
        self,
        specs: Sequence[Any],
        base_by_path: dict[str, tuple[str, bool]],
    ) -> tuple[list[OperationChange], OperationResult | None]:
        changes: list[OperationChange] = []
        for spec in specs:
            current, existed = base_by_path.get(str(spec.file_path), ("", False))
            if not existed:
                return [], _not_found_result(spec.file_path)
            patch = self.patcher.apply_edits(current, list(spec.edits))
            if not patch.success:
                return [], _patch_failed_result(spec.file_path, patch.errors)
            changes.append(
                OperationChange(
                    file_path=spec.file_path,
                    base_content=current,
                    base_hash=content_hash(current),
                    final_content=patch.content,
                    base_existed=True,
                )
            )
        return changes, None

    def _delete_paths_to_changes_from_base(
        self,
        paths: Sequence[str],
        base_by_path: dict[str, tuple[str, bool]],
    ) -> tuple[list[OperationChange], OperationResult | None]:
        changes: list[OperationChange] = []
        for path in paths:
            current, existed = base_by_path.get(path, ("", False))
            if not existed:
                return [], _not_found_result(path)
            changes.append(
                OperationChange(
                    file_path=path,
                    base_content=current,
                    base_hash=content_hash(current),
                    final_content=None,
                    base_existed=True,
                )
            )
        return changes, None

    def _move_specs_to_changes_from_base(
        self,
        specs: Sequence[Any],
        base_by_path: dict[str, tuple[str, bool]],
    ) -> tuple[list[OperationChange], OperationResult | None]:
        changes: list[OperationChange] = []
        for spec in specs:
            if spec.src_path == spec.dst_path:
                return [], _identical_paths_result(spec.src_path)
            src_content, src_existed = base_by_path.get(str(spec.src_path), ("", False))
            if not src_existed:
                return [], _not_found_result(spec.src_path)
            dst_content, dst_existed = base_by_path.get(str(spec.dst_path), ("", False))
            if dst_existed and not spec.overwrite:
                return [], _dst_exists_result(spec.dst_path)
            changes.append(
                OperationChange(
                    file_path=spec.src_path,
                    base_content=src_content,
                    base_hash=content_hash(src_content),
                    final_content=None,
                    base_existed=True,
                )
            )
            if dst_existed:
                changes.append(
                    OperationChange(
                        file_path=spec.dst_path,
                        base_content=dst_content,
                        base_hash=content_hash(dst_content),
                        final_content=src_content,
                        base_existed=True,
                        strict_base=True,
                    )
                )
            else:
                changes.append(
                    OperationChange(
                        file_path=spec.dst_path,
                        base_content="",
                        base_hash="",
                        final_content=src_content,
                        base_existed=False,
                    )
                )
        return changes, None

    def undo_last_edit(self, file_path: str) -> EditResult:
        return self._write_coordinator.undo_last_edit(file_path)


def _delete_spec_path(spec: Any) -> str:
    return str(spec.path) if isinstance(spec, DeleteSpec) else str(spec)


def _request_has_folder_spec(req: _CommitSpecRequest) -> bool:
    if req.op == "delete":
        return any(
            isinstance(spec, DeleteSpec) and spec.is_folder
            for spec in req.specs
        )
    if req.op == "move":
        return any(
            isinstance(spec, MoveSpec) and spec.is_folder
            for spec in req.specs
        )
    return False


def _error_result(
    file_path: str,
    message: str,
    *,
    conflict_reason: str,
    conflict_file: str | None = None,
) -> OperationResult:
    return OperationResult(
        success=False,
        status="failed",
        files=(EditResult(success=False, file_path=file_path, message=message),),
        conflict_file=conflict_file,
        conflict_reason=conflict_reason,
        timings={},
    )


def _not_found_result(file_path: str) -> OperationResult:
    return _error_result(
        file_path,
        f"Path does not exist: {file_path}",
        conflict_reason="not_found",
    )


def _not_a_directory_result(file_path: str) -> OperationResult:
    return _error_result(
        file_path,
        f"Path is not a directory: {file_path}",
        conflict_reason="not_a_directory",
        conflict_file=file_path,
    )


def _identical_paths_result(file_path: str) -> OperationResult:
    return _error_result(
        file_path,
        "src_path and dst_path are identical",
        conflict_reason="identical_paths",
    )


def _dst_exists_result(dst_path: str) -> OperationResult:
    return _error_result(
        dst_path,
        f"Destination exists: {dst_path} (pass overwrite=True to replace)",
        conflict_reason="dst_exists",
        conflict_file=dst_path,
    )


def _patch_failed_result(file_path: str, errors: list[str]) -> OperationResult:
    return _error_result(
        file_path,
        "; ".join(errors) if errors else "edit apply failed",
        conflict_reason="patch_failed",
        conflict_file=file_path,
    )
