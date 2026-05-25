"""Read primitive for namespace-mounted workspaces."""

from __future__ import annotations

import os
from collections.abc import Mapping

from sandbox._shared.models import ReadFileResult
from sandbox._shared.tool_primitives.workspace_filesystem import (
    open_no_follow,
    required_workspace_path,
)

_MAX_READ_BYTES = 16 * 1024 * 1024


def read_file(args: Mapping[str, object]) -> ReadFileResult:
    path = required_workspace_path(args.get("path"))
    try:
        fd = open_no_follow(path, os.O_RDONLY)
    except FileNotFoundError:
        return ReadFileResult(success=True, content="", exists=False)
    with os.fdopen(fd, "rb") as handle:
        size = os.fstat(handle.fileno()).st_size
        if size > _MAX_READ_BYTES:
            raise ValueError(f"file too large: {size} > {_MAX_READ_BYTES} bytes")
        data = handle.read()
    return ReadFileResult(content=data.decode("utf-8", "replace"))


__all__ = ["read_file"]
