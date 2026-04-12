"""Edit history toolkit — query cross-run edit patterns for planner conflict prediction."""

from __future__ import annotations

import json
from typing import Any

from pydantic import BaseModel, Field

from tools.core.base import BaseTool, BaseToolkit, ToolExecutionContext, ToolResult


class QueryEditHistoryInput(BaseModel):
    paths: list[str] = Field(..., description="Scope paths to query edit history for")
    limit: int = Field(default=10, description="Max hotspots to return")


class QueryEditHistoryTool(BaseTool):
    name = "query_edit_history"
    description = (
        "Query cross-run edit patterns to predict scope conflicts. "
        "Returns files edited by multiple agents across previous runs. Planner-only."
    )
    input_model = QueryEditHistoryInput

    async def execute(self, arguments: QueryEditHistoryInput, context: ToolExecutionContext) -> ToolResult:
        store = context.metadata.get("file_change_store")

        if store is not None and getattr(store, "initialized", False):
            # Real PG-backed store available
            hotspots = store.contention_hotspots(
                scope_prefixes=arguments.paths,
                limit=arguments.limit,
            )
            if not hotspots:
                return ToolResult(output=json.dumps({
                    "hotspots": [],
                    "note": "No contention history found for these paths.",
                }))
            return ToolResult(output=json.dumps({
                "hotspots": [
                    {
                        "file": h.file_path,
                        "agents_touched": h.agent_count,
                        "total_edits": h.edit_count,
                    }
                    for h in hotspots
                ],
            }))

        # Fallback: check in-memory Ledger for same-run history
        ledger = context.metadata.get("ledger")
        if ledger is not None:
            hotspots_map: dict[str, set[str]] = {}
            edit_counts: dict[str, int] = {}
            for entry in ledger.changes_since(0):
                if any(entry.file_path.startswith(p.rstrip("/")) for p in arguments.paths):
                    hotspots_map.setdefault(entry.file_path, set()).add(entry.agent_id)
                    edit_counts[entry.file_path] = edit_counts.get(entry.file_path, 0) + 1
            # Only report files touched by multiple agents
            multi_agent = [
                {"file": fp, "agents_touched": len(agents), "total_edits": edit_counts[fp]}
                for fp, agents in hotspots_map.items()
                if len(agents) > 1
            ]
            multi_agent.sort(key=lambda x: (-x["agents_touched"], -x["total_edits"]))
            return ToolResult(output=json.dumps({
                "hotspots": multi_agent[:arguments.limit],
                "note": "In-memory only (same-run history). Connect PostgreSQL for cross-run data.",
            }))

        return ToolResult(output=json.dumps({
            "hotspots": [],
            "note": "No edit history available (no Ledger or FileChangeStore).",
        }))


class EditHistoryToolkit(BaseToolkit):
    def __init__(self) -> None:
        super().__init__(
            name="edit_history",
            description="Query cross-run edit patterns to predict scope conflicts.",
            tools=[QueryEditHistoryTool()],
        )

    @classmethod
    def from_context(cls, ctx: Any) -> EditHistoryToolkit:
        return cls()
