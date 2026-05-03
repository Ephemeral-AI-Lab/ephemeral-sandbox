"""Runtime handler for OCC changeset requests.

This handler accepts two wire formats during the OCC simplification cut-over:

1. **New typed format** (``args.changes``): a list of typed ``Change``
   records that route through :class:`ChangesetOrchestrator` (search/replace
   gate). Used by the host-side ``OCCClient.apply_changeset`` and the
   typed ``write_file`` / ``edit_file`` API verbs.
2. **Legacy overlay format** (``args.upper_changes``): the original raw
   overlay capture that drives ``LocalOCCEngine.apply_changeset``. Step 4
   removes this branch when the legacy gate is deleted.
"""

from __future__ import annotations

from typing import Any

from sandbox.occ.content.gitignore_oracle import GitignoreOracle
from sandbox.occ.content.manager import ContentManager
from sandbox.occ.direct.direct_merge_coordinator import DirectMergeCoordinator
from sandbox.occ.engine import LocalOCCEngine
from sandbox.occ.gated.gated_coordinator import OCCGatedCoordinator
from sandbox.occ.orchestrator import ChangesetOrchestrator
from sandbox.occ.wire import (
    change_from_dict,
    changeset_result_to_dict,
    upper_change_from_dict,
)

from sandbox.client.async_bridge import run_sync


def handle(args: dict[str, Any]) -> Any:
    workspace_root = str(args.get("workspace_root") or "/workspace")
    if "changes" in args:
        return _handle_typed(args, workspace_root)
    return _handle_legacy(args, workspace_root)


def _handle_typed(args: dict[str, Any], workspace_root: str) -> dict[str, Any]:
    changes = [change_from_dict(record) for record in args.get("changes", ())]
    content = ContentManager(workspace_root)
    orchestrator = ChangesetOrchestrator(
        gitignore=GitignoreOracle(workspace_root),
        direct=DirectMergeCoordinator(content),
        gated=OCCGatedCoordinator(content),
    )
    result = run_sync(orchestrator.apply(changes))
    return changeset_result_to_dict(result)


def _handle_legacy(args: dict[str, Any], workspace_root: str) -> Any:
    engine = LocalOCCEngine(workspace_root=workspace_root)
    try:
        return engine.apply_changeset(
            [upper_change_from_dict(change) for change in args.get("upper_changes", ())],
            agent_id=str(args.get("agent_id") or ""),
            edit_type=str(args.get("edit_type") or "apply_changeset"),
            description=str(args.get("description") or ""),
        )
    finally:
        engine.dispose()


__all__ = ["handle"]
