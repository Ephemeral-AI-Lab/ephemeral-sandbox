"""Adapt overlay NDJSON gitinclude changes to the OCC write coordinator.

See ``docs/architecture/overlay-sandbox-plan.md`` §4.1. The committer is
deliberately decoupled from the auditor / sandbox transport: given a
sequence of :class:`OverlayChange` items (every upperdir entry that
``git check-ignore`` did *not* flag — first-writer-wins under
concurrency), it builds strict-base ``OperationChange`` values and
delegates to ``WriteCoordinator``. Gitignored writes were already
direct-merged inside the namespace and do not pass through here.
"""

from __future__ import annotations

import logging
from collections.abc import Sequence
from typing import Any

from code_intelligence.core.async_bridge import run_sync_in_executor, use_sandbox_io_loop
from code_intelligence.core.hashing import content_hash
from code_intelligence.overlay.types import OverlayChange
from code_intelligence.core.types import OperationChange, OperationResult

logger = logging.getLogger(__name__)


class OverlayCommandCommitter:
    """Commit the gitinclude slice of one overlay op through OCC.

    Every change that ``git check-ignore`` did *not* flag is committed
    here; index membership is irrelevant to routing. The strict-base
    contract (``strict_base=True``) matches the plan's invariant that
    ``base_content`` always comes from ``git show $SNAP:path`` — peer
    edits between SNAP and commit abort the op with ``aborted_version``,
    never produce a silent merge. Concurrent writers to the same path
    therefore resolve as first-writer-wins.
    """

    def __init__(self, write_coordinator: Any, *, workspace_root: str) -> None:
        self._write_coordinator = write_coordinator
        self._workspace_root = workspace_root.rstrip("/")

    async def commit(
        self,
        changes: Sequence[OverlayChange],
        *,
        agent_id: str = "",
        edit_type: str = "svc_cmd_overlay",
        description: str = "shell overlay",
    ) -> OperationResult:
        op_changes = self.to_operation_changes(changes)
        if not op_changes:
            return OperationResult(
                success=True,
                status="committed",
                files=(),
                conflict_file=None,
                conflict_reason="",
                timings={"total": 0.0},
            )
        with use_sandbox_io_loop():
            result: OperationResult = await run_sync_in_executor(
                self._write_coordinator.commit_operation_against_base,
                op_changes,
                agent_id=agent_id,
                edit_type=edit_type,
                description=description,
            )
        if not result.success:
            logger.warning(
                "overlay commit aborted: status=%s reason=%s file=%s",
                result.status,
                result.conflict_reason,
                result.conflict_file,
            )
        return result

    def to_operation_changes(
        self, changes: Sequence[OverlayChange]
    ) -> list[OperationChange]:
        """Convert NDJSON-parsed ``OverlayChange`` into strict-base OCC values."""
        op_changes: list[OperationChange] = []
        for change in changes:
            op_changes.append(
                OperationChange(
                    file_path=self._live_path(change.path),
                    base_content=change.base_content,
                    base_hash=content_hash(change.base_content) if change.base_existed else "",
                    final_content=change.final_content,
                    base_existed=change.base_existed,
                    strict_base=True,
                )
            )
        return op_changes

    def _live_path(self, rel: str) -> str:
        rel = rel.replace("\\", "/").lstrip("/")
        return f"{self._workspace_root}/{rel}"


__all__ = ["OverlayCommandCommitter"]
