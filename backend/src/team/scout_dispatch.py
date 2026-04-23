"""Scout-specific ``run_subagent`` payload validation.

The scout subagent has one policy knob that must be enforced at dispatch
time rather than inside the agent's playbook: planner-tier callers
(``root_planner`` / ``team_planner`` / ``team_replanner``) are allowed
exactly one live production owner path per ``run_subagent`` call. All
other callers may pass multi-path scout bundles.

Registration is a module-import side effect so any code path that imports
``team.builtins`` (directly or transitively) picks up the validator. This
keeps ``tools.subagent.run_subagent_tool`` generic.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from team._path_utils import is_test_scope_path
from tools.core.base import ToolExecutionContext, ToolResult
from tools.subagent.dispatch_validators import register_dispatch_validator

_SINGLE_TARGET_CALLERS: frozenset[str] = frozenset(
    {"root_planner", "team_planner", "team_replanner"}
)
_REPO_PATH_RE = re.compile(r"(?:[A-Za-z0-9_.-]+/)+[A-Za-z0-9_.-]+(?:\.py)?")


def _normalize_target_paths(value: Any) -> list[str]:
    if isinstance(value, str):
        stripped = value.strip()
        return [stripped] if stripped else []
    if not isinstance(value, list):
        return []
    out: list[str] = []
    for item in value:
        if isinstance(item, str):
            stripped = item.strip()
            if stripped:
                out.append(stripped)
    return out


def _candidate_exists_in_repo(candidate: str, repo_root: Path) -> bool:
    try:
        root = repo_root.resolve()
        resolved = (root / candidate).resolve()
    except Exception:
        return False
    try:
        resolved.relative_to(root)
    except ValueError:
        return False
    return resolved.exists()


def _extract_repoish_paths(value: Any, *, repo_root: Path) -> list[str]:
    if not isinstance(value, str) or not value.strip():
        return []
    seen: set[str] = set()
    out: list[str] = []
    for match in _REPO_PATH_RE.findall(value):
        candidate = match.strip("`'\".,;:()[]{}<>")
        if not candidate or candidate in seen:
            continue
        if not _candidate_exists_in_repo(candidate, repo_root):
            continue
        seen.add(candidate)
        out.append(candidate)
    return out


def _path_related_to_target(path: str, target_path: str) -> bool:
    path_norm = path.strip("/")
    target_norm = target_path.strip("/")
    if path_norm == target_norm:
        return True
    return path_norm.startswith(target_norm + "/") or target_norm.startswith(path_norm + "/")


def validate_scout_dispatch(
    prompt: str | None,
    input: dict[str, Any] | None,
    context: ToolExecutionContext,
) -> ToolResult | None:
    """Reject planner-tier scout calls that widen beyond one production owner.

    Returns ``None`` for accepted requests; returns a ``ToolResult`` with
    ``is_error=True`` carrying planner-facing guidance when the call
    violates the single-target contract.
    """
    if not isinstance(input, dict):
        return None
    caller_agent = str((context.metadata or {}).get("agent_name") or "").strip()
    if caller_agent not in _SINGLE_TARGET_CALLERS:
        return None

    target_paths = _normalize_target_paths(input.get("target_paths"))
    if len(target_paths) != 1:
        return ToolResult(
            output=(
                "run_subagent: planner/replanner scout calls must pass exactly "
                "one production owner path in `target_paths`. Split fan-out "
                "across multiple `run_subagent(...)` calls and keep tests, "
                "missing test-derived paths, and verification evidence in "
                "`context`."
            ),
            is_error=True,
        )

    target_path = target_paths[0]
    if is_test_scope_path(target_path):
        return ToolResult(
            output=(
                "run_subagent: planner/replanner scout `target_paths` must "
                "name one live production owner path, not a test or "
                "verification path. Move benchmark tests, `*/tests/*`, "
                "and `test_*.py` targets into `context` and use the "
                "production owner path instead."
            ),
            is_error=True,
        )
    if not _candidate_exists_in_repo(target_path, context.cwd):
        return ToolResult(
            output=(
                "run_subagent: planner/replanner scout `target_paths` must "
                "name one live production owner path in the repo. "
                f"Missing path: {target_path}. Use a stable existing "
                "production boundary in `target_paths` and keep missing "
                "or test-derived paths in `context`."
            ),
            is_error=True,
        )

    context_paths = _extract_repoish_paths(input.get("context"), repo_root=context.cwd)
    extras = [
        path
        for path in context_paths
        if not is_test_scope_path(path) and not _path_related_to_target(path, target_path)
    ]
    if extras:
        extra_preview = ", ".join(extras[:3])
        return ToolResult(
            output=(
                "run_subagent: planner/replanner scout context may not name "
                "other production owner paths outside the single declared "
                f"`target_paths` entry. Extra paths: {extra_preview}. "
                "Launch separate scout calls for those owner families and "
                "keep benchmark tests or failing ids in `context`."
            ),
            is_error=True,
        )
    return None


register_dispatch_validator("scout", validate_scout_dispatch)
