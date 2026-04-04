"""OCC-coordinated file editing tool for Daytona sandboxes.

Coordinates edits via the Arbiter for conflict detection. Falls back
to direct write if no CI service is configured.
"""

from __future__ import annotations

import hashlib
import logging
from typing import Any

from pydantic import BaseModel, Field

from ephemeralos.tools.base import BaseTool, ToolExecutionContext, ToolResult
from ephemeralos.tools.daytona_toolkit.ci_integration import (
    get_ci_gateway,
    prime_cache_after_write,
    record_edit_in_ledger,
)

logger = logging.getLogger(__name__)

_OUTPUT_MAX_CHARS = 8000


def _content_hash(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]


class DaytonaEditInput(BaseModel):
    """Arguments for editing a file in the sandbox."""

    file_path: str = Field(description="Path to the file to edit")
    old_text: str = Field(description="Text to find and replace (first occurrence)")
    new_text: str = Field(description="Replacement text")
    description: str = Field(default="", description="Optional description of the edit")
    dry_run: bool = Field(default=False, description="Preview the edit without applying")


class DaytonaEditTool(BaseTool):
    """Edit a file in the Daytona sandbox with OCC conflict detection."""

    name = "daytona_edit_file"
    description = (
        "Edit a file in the remote Daytona sandbox using search-and-replace. "
        "Coordinates with the code intelligence service for conflict detection. "
        "Use dry_run=true to preview changes without applying."
    )
    input_model = DaytonaEditInput

    async def execute(
        self, arguments: DaytonaEditInput, context: ToolExecutionContext,
    ) -> ToolResult:
        sandbox = context.metadata.get("daytona_sandbox")
        if sandbox is None:
            return ToolResult(
                output="No Daytona sandbox in context.",
                is_error=True,
            )

        file_path = arguments.file_path

        # Read current content
        try:
            raw = sandbox.fs.download_file(file_path)
            current = raw.decode("utf-8") if isinstance(raw, bytes) else str(raw)
        except Exception as exc:
            return ToolResult(output=f"Cannot read file: {exc}", is_error=True)

        # Check that old_text exists
        if arguments.old_text not in current:
            return ToolResult(
                output=f"Search text not found in {file_path}",
                is_error=True,
            )

        # Apply edit
        new_content = current.replace(arguments.old_text, arguments.new_text, 1)

        if arguments.dry_run:
            # Show preview
            import difflib
            diff = difflib.unified_diff(
                current.splitlines(keepends=True),
                new_content.splitlines(keepends=True),
                fromfile=f"a/{file_path}",
                tofile=f"b/{file_path}",
                lineterm="",
            )
            diff_text = "".join(diff)
            if len(diff_text) > _OUTPUT_MAX_CHARS:
                diff_text = diff_text[:_OUTPUT_MAX_CHARS] + "\n... (truncated)"
            return ToolResult(
                output=f"[DRY RUN] Preview of changes to {file_path}:\n\n{diff_text}",
                metadata={"dry_run": True},
            )

        # Try OCC-coordinated edit via CI gateway
        gw = get_ci_gateway(context)
        if gw and hasattr(gw, "arbiter") and gw.arbiter:
            arbiter = gw.arbiter
            old_hash = _content_hash(current)

            if not arbiter.acquire_file_lock(file_path, timeout=15.0):
                return ToolResult(
                    output=f"Could not acquire edit lock for {file_path} (conflict)",
                    is_error=True,
                    metadata={"conflict": True},
                )

            try:
                # Save snapshot for undo
                tm = gw.time_machine
                if tm:
                    tm.save(file_path, current)

                # Write
                sandbox.fs.upload_file(file_path, new_content.encode("utf-8"))

                # Record
                new_hash = _content_hash(new_content)
                arbiter.record_edit(file_path, "")
                record_edit_in_ledger(
                    context, file_path,
                    edit_type="edit",
                    old_hash=old_hash,
                    new_hash=new_hash,
                    description=arguments.description,
                )
                prime_cache_after_write(context, file_path, new_content)

                return ToolResult(
                    output=f"Edited {file_path} (OCC-coordinated)",
                    metadata={"file_path": file_path, "occ": True},
                )
            finally:
                arbiter.release_file_lock(file_path)
        else:
            # Direct write (no CI)
            try:
                sandbox.fs.upload_file(file_path, new_content.encode("utf-8"))
                return ToolResult(
                    output=f"Edited {file_path}",
                    metadata={"file_path": file_path, "occ": False},
                )
            except Exception as exc:
                return ToolResult(output=f"Write failed: {exc}", is_error=True)
