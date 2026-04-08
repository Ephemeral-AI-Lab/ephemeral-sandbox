"""In-memory artifact storage with byte-budget enforcement."""

from __future__ import annotations

from typing import Any, Protocol

from team.types import ArtifactTooLarge, BudgetConfig, BudgetState


def _size_of(obj: Any) -> int:
    """Rough byte-size estimate for budget accounting."""
    if isinstance(obj, (bytes, bytearray)):
        return len(obj)
    if isinstance(obj, str):
        return len(obj.encode("utf-8"))
    try:
        import json

        return len(json.dumps(obj, default=str).encode("utf-8"))
    except Exception:
        return len(repr(obj).encode("utf-8"))


class ArtifactStoreProto(Protocol):
    def save(self, work_item_id: str, artifact: Any) -> str: ...
    def load(self, work_item_id: str) -> Any: ...
    def load_many(self, work_item_ids: list[str]) -> dict[str, Any]: ...
    def delete(self, work_item_id: str) -> bool: ...


class InMemoryArtifactStore:
    """Simple dict-backed store with per-artifact and aggregate byte caps."""

    def __init__(self, budgets: BudgetConfig, budget_state: BudgetState) -> None:
        self._budgets = budgets
        self._state = budget_state
        self._data: dict[str, Any] = {}
        self._sizes: dict[str, int] = {}

    def save(self, work_item_id: str, artifact: Any) -> str:
        size = _size_of(artifact)
        if size > self._budgets.max_artifact_bytes:
            raise ArtifactTooLarge(
                f"artifact for {work_item_id} is {size}B, max_artifact_bytes={self._budgets.max_artifact_bytes}"
            )
        projected = self._state.artifact_bytes_used + size - self._sizes.get(work_item_id, 0)
        if projected > self._budgets.max_total_artifact_bytes:
            raise ArtifactTooLarge(
                f"aggregate artifact bytes would reach {projected}, "
                f"max_total_artifact_bytes={self._budgets.max_total_artifact_bytes}"
            )
        # Release old allocation if replacing
        if work_item_id in self._sizes:
            self._state.artifact_bytes_used -= self._sizes[work_item_id]
        self._data[work_item_id] = artifact
        self._sizes[work_item_id] = size
        self._state.artifact_bytes_used += size
        return work_item_id

    def load(self, work_item_id: str) -> Any:
        return self._data.get(work_item_id)

    def load_many(self, work_item_ids: list[str]) -> dict[str, Any]:
        return {wi: self._data[wi] for wi in work_item_ids if wi in self._data}

    def delete(self, work_item_id: str) -> bool:
        if work_item_id not in self._data:
            return False
        self._state.artifact_bytes_used -= self._sizes.pop(work_item_id, 0)
        del self._data[work_item_id]
        return True

    # Checkpoint helpers
    def snapshot(self) -> dict[str, Any]:
        import copy

        return copy.deepcopy(self._data)

    def restore(self, snapshot: dict[str, Any]) -> None:
        import copy

        self._data = copy.deepcopy(snapshot)
        self._sizes = {k: _size_of(v) for k, v in self._data.items()}
        self._state.artifact_bytes_used = sum(self._sizes.values())
