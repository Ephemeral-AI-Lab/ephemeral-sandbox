"""Shared role-based submission tool visibility policy for team-mode agents."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class RoleToolPolicy:
    """Role-level policy for visible submission tools."""

    allowed_submission_tools: frozenset[str]


_ROLE_TOOL_POLICIES: dict[str, RoleToolPolicy] = {
    "planner": RoleToolPolicy(
        allowed_submission_tools=frozenset({"submit_plan"}),
    ),
    "replanner": RoleToolPolicy(
        allowed_submission_tools=frozenset({"submit_replan"}),
    ),
    "developer": RoleToolPolicy(
        allowed_submission_tools=frozenset({"submit_task_success", "request_replan"}),
    ),
    "parent_summarizer": RoleToolPolicy(
        allowed_submission_tools=frozenset({"submit_task_success"}),
    ),
    "reviewer": RoleToolPolicy(
        allowed_submission_tools=frozenset({"submit_task_success", "request_replan"}),
    ),
    "explorer": RoleToolPolicy(
        allowed_submission_tools=frozenset(),
    ),
    "scout": RoleToolPolicy(
        allowed_submission_tools=frozenset(),
    ),
}


def get_role_tool_policy(role: str | None) -> RoleToolPolicy | None:
    """Return the shared team-mode role policy, if any."""
    role_name = str(role or "").strip()
    if not role_name:
        return None
    return _ROLE_TOOL_POLICIES.get(role_name)


def blocked_submission_tools_for_role(
    role: str | None,
    available_submission_tools: list[str] | set[str] | tuple[str, ...],
) -> set[str]:
    """Return submission tools that should be hidden for this role."""
    policy = get_role_tool_policy(role)
    if policy is None:
        return set()
    available = {
        str(name).strip()
        for name in available_submission_tools
        if str(name).strip()
    }
    return available - set(policy.allowed_submission_tools)
