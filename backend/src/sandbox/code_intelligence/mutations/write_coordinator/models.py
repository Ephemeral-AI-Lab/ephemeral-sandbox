"""Data models used by the write coordinator pipeline."""

from __future__ import annotations

from dataclasses import dataclass

from sandbox.code_intelligence.core.types import OperationChange


@dataclass(frozen=True)
class CommitOperation:
    """One attributed semantic operation inside a batched commit."""

    changes: tuple[OperationChange, ...]
    agent_id: str = ""
    edit_type: str = "edit"
    description: str = ""


@dataclass(frozen=True)
class ResolvedChange:
    """A planned change resolved against the current locked file state."""

    change: OperationChange
    current_content: str
    final_content: str | None
    current_hash: str
    existed: bool
