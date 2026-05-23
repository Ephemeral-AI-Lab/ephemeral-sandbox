"""``api.grep`` dispatch entry (read-only).

Acquires a snapshot lease (MVCC read isolation, mirrors ``read.py``) and walks
paths through ``services.layer_stack`` against ``lease.manifest`` so each scan
is consistent with the leased snapshot. The handler does not import
``occ_client`` or touch the OCC mutation gate — the search surface is
read-only by construction.

The companion ``glob.py`` shares the directory-walk helpers exported below.
"""

from __future__ import annotations

import fnmatch
import re
from typing import Any, Literal
from uuid import uuid4

from sandbox.layer_stack.workspace_binding import require_workspace_binding
from sandbox.daemon.async_bridge import run_sync_in_executor
from sandbox.daemon.occ_backend import build_occ_backend
from sandbox.daemon.request_context import (
    classify_path,
    require_layer_stack_root,
)
from sandbox._shared.clock import monotonic_now


VCS_EXCLUDED = frozenset({".git", ".svn", ".hg", ".bzr", ".jj", ".sl"})
DEFAULT_GREP_HEAD_LIMIT = 250
MAX_GREP_CONTENT_BYTES = 20 * 1024
MAX_GREP_FILE_BYTES = 10 * 1024 * 1024

_GrepMode = Literal["content", "files_with_matches", "count"]
_VALID_MODES: frozenset[str] = frozenset({"content", "files_with_matches", "count"})


def is_vcs_excluded(path: str) -> bool:
    return any(part in VCS_EXCLUDED for part in path.split("/"))


def layer_subpath(args: dict[str, Any], workspace_root: str) -> str:
    raw = str(args.get("path") or "").strip()
    if not raw:
        return ""
    classified = classify_path(raw, workspace_root)
    if classified.classification != "in_workspace":
        raise ValueError(f"search path must be inside the workspace: {raw}")
    return classified.layer_path


def under(prefix: str, path: str) -> bool:
    if not prefix:
        return True
    return path == prefix or path.startswith(prefix + "/")


def _compile_regex(
    pattern: str, *, case_insensitive: bool, multiline: bool
) -> re.Pattern[str]:
    flags = 0
    if case_insensitive:
        flags |= re.IGNORECASE
    if multiline:
        flags |= re.MULTILINE | re.DOTALL
    try:
        return re.compile(pattern, flags)
    except re.error as exc:
        raise ValueError(f"invalid regex pattern: {exc}") from exc


def _grep_sync(args: dict[str, Any]) -> dict[str, Any]:
    total_start = monotonic_now()
    layer_stack_root = require_layer_stack_root(args)
    binding = require_workspace_binding(layer_stack_root)
    pattern_raw = args.get("pattern")
    pattern = "" if pattern_raw is None else str(pattern_raw)
    if not pattern:
        raise ValueError("pattern is required")
    sub_path = layer_subpath(args, binding.workspace_root)
    glob_filter = str(args.get("glob_filter") or "").strip() or None
    raw_mode = str(args.get("output_mode") or "files_with_matches").strip() or "files_with_matches"
    if raw_mode not in _VALID_MODES:
        raise ValueError(
            f"output_mode must be one of {sorted(_VALID_MODES)}: {raw_mode}"
        )
    mode: _GrepMode = raw_mode  # type: ignore[assignment]
    raw_head_limit = args.get("head_limit", DEFAULT_GREP_HEAD_LIMIT)
    head_limit = (
        int(raw_head_limit) if raw_head_limit is not None else DEFAULT_GREP_HEAD_LIMIT
    )
    if head_limit < 0:
        raise ValueError("head_limit must be >= 0")
    offset = int(args.get("offset", 0) or 0)
    if offset < 0:
        raise ValueError("offset must be >= 0")
    case_insensitive = bool(args.get("case_insensitive"))
    line_numbers = bool(args.get("line_numbers"))
    multiline = bool(args.get("multiline"))

    regex = _compile_regex(
        pattern, case_insensitive=case_insensitive, multiline=multiline
    )

    services = build_occ_backend(layer_stack_root)
    request_id = uuid4().hex
    lease_start = monotonic_now()
    lease = services.manager.acquire_snapshot_lease(request_id)
    lease_acquired_s = monotonic_now() - lease_start
    skipped_large = 0
    skipped_binary = 0
    try:
        scan_start = monotonic_now()
        candidate_paths: list[str] = []
        for layer_path in services.layer_stack.iter_paths(lease.manifest):
            if is_vcs_excluded(layer_path):
                continue
            if not under(sub_path, layer_path):
                continue
            if glob_filter is not None and not fnmatch.fnmatchcase(
                layer_path, glob_filter
            ):
                continue
            candidate_paths.append(layer_path)
        candidate_paths.sort()

        matched_filenames: list[str] = []
        match_counts: list[tuple[str, int]] = []
        content_segments: list[str] = []
        content_size = 0
        truncated = False
        lines_emitted = 0
        total_matches = 0
        observed_files = 0

        for layer_path in candidate_paths:
            data, exists = services.layer_stack.read_bytes(layer_path, lease.manifest)
            if not exists or data is None:
                continue
            if len(data) > MAX_GREP_FILE_BYTES:
                skipped_large += 1
                continue
            try:
                text = data.decode("utf-8")
            except UnicodeDecodeError:
                skipped_binary += 1
                continue

            if mode == "files_with_matches":
                if regex.search(text) is None:
                    continue
                observed_files += 1
                if observed_files <= offset:
                    continue
                if head_limit and len(matched_filenames) >= head_limit:
                    truncated = True
                    break
                matched_filenames.append(layer_path)
                total_matches += 1
            elif mode == "count":
                count = len(regex.findall(text))
                if count <= 0:
                    continue
                observed_files += 1
                if observed_files <= offset:
                    continue
                if head_limit and len(match_counts) >= head_limit:
                    truncated = True
                    break
                match_counts.append((layer_path, count))
                total_matches += count
            else:
                file_lines = text.splitlines()
                file_matched = False
                stop = False
                for index, line in enumerate(file_lines, start=1):
                    if regex.search(line) is None:
                        continue
                    total_matches += 1
                    if total_matches <= offset:
                        continue
                    prefix = (
                        f"{layer_path}:{index}:" if line_numbers else f"{layer_path}:"
                    )
                    rendered = prefix + line + "\n"
                    rendered_size = len(rendered.encode("utf-8"))
                    if content_size + rendered_size > MAX_GREP_CONTENT_BYTES:
                        truncated = True
                        stop = True
                        break
                    if head_limit and lines_emitted >= head_limit:
                        truncated = True
                        stop = True
                        break
                    content_segments.append(rendered)
                    content_size += rendered_size
                    lines_emitted += 1
                    file_matched = True
                if file_matched:
                    matched_filenames.append(layer_path)
                if stop:
                    break

        scan_elapsed = monotonic_now() - scan_start

        if mode == "files_with_matches":
            num_files = len(matched_filenames)
            filenames_out = list(matched_filenames)
            content_out = ""
        elif mode == "count":
            filenames_out = [item[0] for item in match_counts]
            content_out = "\n".join(
                f"{path}:{count}" for path, count in match_counts
            )
            num_files = len(match_counts)
        else:
            num_files = len(matched_filenames)
            filenames_out = list(matched_filenames)
            content_out = "".join(content_segments)

        return {
            "success": True,
            "output_mode": mode,
            "filenames": filenames_out,
            "content": content_out,
            "num_files": num_files,
            "num_lines": lines_emitted,
            "num_matches": total_matches,
            "applied_limit": head_limit if head_limit else None,
            "applied_offset": offset,
            "truncated": truncated,
            "timings": {
                "api.grep.lease_acquire_s": lease_acquired_s,
                "api.grep.scan_s": scan_elapsed,
                "api.grep.skipped_large": float(skipped_large),
                "api.grep.skipped_binary": float(skipped_binary),
                "api.grep.total_s": monotonic_now() - total_start,
            },
        }
    finally:
        services.manager.release_lease(lease.lease_id)


async def grep(args: dict[str, Any]) -> dict[str, Any]:
    """``api.grep``: regex-scan snapshot file contents."""
    return await run_sync_in_executor(_grep_sync, args)


__all__ = [
    "DEFAULT_GREP_HEAD_LIMIT",
    "MAX_GREP_CONTENT_BYTES",
    "MAX_GREP_FILE_BYTES",
    "VCS_EXCLUDED",
    "grep",
    "is_vcs_excluded",
    "layer_subpath",
    "under",
]
