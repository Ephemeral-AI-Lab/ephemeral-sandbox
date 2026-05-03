"""OCC-gated operation facade helpers for CodeIntelligenceService."""

from __future__ import annotations

import logging
from collections.abc import Sequence
from typing import Any, cast

from sandbox.occ.patching.patcher import Patcher
from sandbox.occ.commit import CommitOperation, WriteCoordinator
from sandbox.occ.content.hashing import content_hash
from sandbox.occ.content.manager import ContentManager
from sandbox.occ.types import (
    EditResult,
    EditSpec,
    OperationChange,
    OperationResult,
    WriteSpec,
)

logger = logging.getLogger(__name__)

_CommitSpec = WriteSpec | EditSpec


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


def _patch_failed_result(file_path: str, errors: list[str]) -> OperationResult:
    return _error_result(
        file_path,
        "; ".join(errors) if errors else "edit apply failed",
        conflict_reason="patch_failed",
        conflict_file=file_path,
    )


class _CommitSpecRequest:
    """Normalized internal high-level commit request."""

    def __init__(
        self,
        *,
        op: str,
        specs: Sequence[_CommitSpec],
        agent_id: str = "",
        description: str = "",
    ) -> None:
        self.op = op
        self.specs = specs
        self.agent_id = agent_id
        self.description = description


class OCCOperationService:
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
        """Apply search/replace edits to one or more files.

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

    # -- Spec -> OperationChange adapters ------------------------------------

    def _commit_specs_direct(self, req: _CommitSpecRequest) -> OperationResult:
        if req.op == "write":
            write_specs = cast("Sequence[WriteSpec]", req.specs)
            return self.write_file(
                write_specs,
                agent_id=req.agent_id,
                description=req.description,
            )
        if req.op == "edit":
            edit_specs = cast("Sequence[EditSpec]", req.specs)
            return self.edit_file(
                edit_specs,
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
        return list(dict.fromkeys(paths))

    def _commit_spec_changes_from_base(
        self,
        req: _CommitSpecRequest,
        base_by_path: dict[str, tuple[str, bool]],
    ) -> tuple[list[OperationChange], OperationResult | None]:
        if req.op == "write":
            write_specs = cast("Sequence[WriteSpec]", req.specs)
            return self._write_specs_to_changes_from_base(write_specs, base_by_path), None
        if req.op == "edit":
            edit_specs = cast("Sequence[EditSpec]", req.specs)
            return self._edit_specs_to_changes_from_base(edit_specs, base_by_path)
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
        specs: Sequence[WriteSpec],
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
        specs: Sequence[EditSpec],
        base_by_path: dict[str, tuple[str, bool]],
    ) -> tuple[list[OperationChange], OperationResult | None]:
        changes: list[OperationChange] = []
        for spec in specs:
            current, existed = base_by_path.get(str(spec.file_path), ("", False))
            if not existed:
                return [], _not_found_result(spec.file_path)
            patch = self.patcher.apply_many(current, list(spec.edits))
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
