"""Grep primitive for namespace-mounted workspaces."""

from __future__ import annotations

import fnmatch
import re
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Literal, cast

from sandbox._shared.models import GrepResult
from sandbox._shared.tool_primitives.workspace_filesystem import (
    display_workspace_path,
    is_regular_file_no_follow,
    read_bytes_no_follow,
    search_root_path,
    walk_dirs_no_follow,
)

_MAX_FILE_BYTES = 2 * 1024 * 1024
_GrepOutputMode = Literal["content", "files_with_matches", "count"]


@dataclass(frozen=True)
class _GrepOptions:
    root: Path
    pattern: str
    case_insensitive: bool = False
    glob_filter: str | None = None
    output_mode: _GrepOutputMode = "files_with_matches"
    line_numbers: bool = False
    multiline: bool = False


def grep_files(
    args: Mapping[str, object],
) -> GrepResult:
    opts = _options(args)
    if not opts.pattern:
        raise ValueError("pattern is required")
    flags = re.MULTILINE
    if opts.case_insensitive:
        flags |= re.IGNORECASE
    if opts.multiline:
        flags |= re.DOTALL
    regex = re.compile(opts.pattern, flags)
    filenames: list[str] = []
    content_lines: list[str] = []
    num_matches = 0
    for path in sorted(_candidate_files(opts.root)):
        rel = display_workspace_path(path)
        if opts.glob_filter and not fnmatch.fnmatch(rel, opts.glob_filter):
            continue
        try:
            data = read_bytes_no_follow(path)
            if len(data) > _MAX_FILE_BYTES:
                continue
            text = data.decode("utf-8")
        except (OSError, UnicodeDecodeError, ValueError):
            continue
        matches = list(regex.finditer(text))
        if not matches:
            continue
        filenames.append(rel)
        num_matches += len(matches)
        if opts.output_mode in {"content", "count"}:
            if opts.output_mode == "count":
                content_lines.append(f"{rel}:{len(matches)}")
            else:
                content_lines.extend(_matching_lines(rel, text, regex, opts.line_numbers))
    filenames_tuple = tuple(filenames)
    content = "\n".join(content_lines)
    if content:
        content += "\n"
    return GrepResult(
        output_mode=opts.output_mode,
        filenames=filenames_tuple,
        content=content,
        num_files=len(filenames_tuple),
        num_lines=len(content_lines) if opts.output_mode == "content" else 0,
        num_matches=num_matches,
        applied_limit=None,
        applied_offset=0,
        truncated=False,
    )


def _options(args: Mapping[str, object]) -> _GrepOptions:
    glob_filter = args.get("glob_filter")
    return _GrepOptions(
        root=Path(search_root_path(args.get("path") or ".")),
        pattern=str(args.get("pattern") or ""),
        case_insensitive=bool(args.get("case_insensitive", False)),
        glob_filter=str(glob_filter) if glob_filter else None,
        output_mode=cast(
            _GrepOutputMode,
            str(args.get("output_mode") or "files_with_matches"),
        ),
        line_numbers=bool(args.get("line_numbers", False)),
        multiline=bool(args.get("multiline", False)),
    )


def _matching_lines(
    rel: str,
    text: str,
    regex: re.Pattern[str],
    line_numbers: bool,
) -> list[str]:
    lines: list[str] = []
    for lineno, line in enumerate(text.splitlines(), start=1):
        if regex.search(line):
            prefix = f"{rel}:{lineno}:" if line_numbers else f"{rel}:"
            lines.append(prefix + line)
    return lines


def _candidate_files(root: Path) -> tuple[Path, ...]:
    if is_regular_file_no_follow(root):
        return (root,)
    return tuple(walk_dirs_no_follow(root))


__all__ = ["grep_files"]
