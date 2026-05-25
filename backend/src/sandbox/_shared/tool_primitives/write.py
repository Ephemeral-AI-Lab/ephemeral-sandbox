"""Write primitive for namespace-mounted workspaces."""

from __future__ import annotations

from collections.abc import Mapping

from sandbox._shared.models import WriteFileResult
from sandbox._shared.tool_primitives.workspace_filesystem import (
    required_workspace_path,
    write_bytes_no_follow,
)


def write_file(
    args: Mapping[str, object],
) -> WriteFileResult:
    path = required_workspace_path(args.get("path"))
    content = str(args.get("content") or "")
    overwrite = bool(args.get("overwrite", True))
    write_bytes_no_follow(path, str(content).encode("utf-8"), overwrite=overwrite)
    return WriteFileResult(changed_paths=(path,), status="ok")


__all__ = ["write_file"]
