"""Contract: isolated-workspace gate wiring (plan G1/G2).

Mirrors ``test_advisor_gate_wiring.py`` but for the new gates:
- ``RequireNoInflightBackgroundTasks`` is on ``enter``/``exit`` and all nine
  main terminals, ordered BEFORE ``AdvisorApprovalPreHook`` on terminals, and
  absent from the helper terminals;
- each gate instance's ``target_tool`` equals its host tool's own name
  (guards the 11-site copy-paste against a wrong ``<own_name>`` that
  ``validate_hook_targets`` would otherwise reject at construction);
- ``ask_advisor`` carries ``BlockInIsolatedMode``.

Offline check — no Daytona, no Postgres, no event loop.
"""

from __future__ import annotations

from tools.ask_helper.ask_advisor import ask_advisor
from tools.isolated_workspace.enter_isolated_workspace import enter_isolated_workspace
from tools.isolated_workspace.exit_isolated_workspace import exit_isolated_workspace
from tools.submission import make_submission_tools
from tools._hooks.advisor_approval import AdvisorApprovalPreHook
from tools._hooks.block_in_isolated_mode import BlockInIsolatedMode
from tools._hooks.require_no_inflight_background_tasks import (
    RequireNoInflightBackgroundTasks,
)


_MAIN_TERMINAL_NAMES = frozenset(
    {
        "submit_plan_closes_goal",
        "submit_plan_defers_goal",
        "submit_execution_success",
        "submit_execution_blocker",
        "submit_execution_handoff",
        "submit_evaluation_success",
        "submit_evaluation_failure",
        "submit_verification_success",
        "submit_verification_failure",
    }
)

_HELPER_TERMINAL_NAMES = frozenset(
    {
        "submit_advisor_feedback",
        "submit_exploration_result",
    }
)


def _pre_hooks(tool) -> tuple:
    return tuple(getattr(tool, "pre_hooks", ()) or ())


def _only(hooks, hook_type) -> list:
    return [h for h in hooks if isinstance(h, hook_type)]


def test_enter_exit_carry_bg_gate_targeting_self() -> None:
    for tool in (enter_isolated_workspace, exit_isolated_workspace):
        bg = _only(_pre_hooks(tool), RequireNoInflightBackgroundTasks)
        assert len(bg) == 1, f"{tool.name!r}: expected one bg gate, got {_pre_hooks(tool)!r}"
        assert bg[0].target_tool == tool.name


def test_main_terminals_carry_bg_gate_before_advisor() -> None:
    tools_by_name = {tool.name: tool for tool in make_submission_tools()}
    for name in _MAIN_TERMINAL_NAMES:
        tool = tools_by_name.get(name)
        assert tool is not None, f"{name!r} missing from make_submission_tools()"
        hooks = _pre_hooks(tool)
        bg = _only(hooks, RequireNoInflightBackgroundTasks)
        advisor = _only(hooks, AdvisorApprovalPreHook)
        assert len(bg) == 1, f"{name!r}: expected one bg gate, got {hooks!r}"
        assert len(advisor) == 1, f"{name!r}: expected one advisor gate, got {hooks!r}"
        assert bg[0].target_tool == name, f"{name!r}: bg target_tool={bg[0].target_tool!r}"
        # Ordering: the bg rejection must surface first (plan D6).
        assert hooks.index(bg[0]) < hooks.index(advisor[0]), (
            f"{name!r}: bg gate must precede advisor gate, got {hooks!r}"
        )


def test_helper_terminals_omit_bg_gate() -> None:
    tools_by_name = {tool.name: tool for tool in make_submission_tools()}
    for name in _HELPER_TERMINAL_NAMES:
        tool = tools_by_name.get(name)
        assert tool is not None, f"{name!r} missing from make_submission_tools()"
        assert not _only(_pre_hooks(tool), RequireNoInflightBackgroundTasks), (
            f"{name!r}: helper terminal must not carry the bg gate"
        )


def test_ask_advisor_carries_block_in_isolated_mode() -> None:
    hooks = _pre_hooks(ask_advisor)
    blockers = _only(hooks, BlockInIsolatedMode)
    assert len(blockers) == 1, f"ask_advisor: expected one BlockInIsolatedMode, got {hooks!r}"
    assert blockers[0].target_tool == "ask_advisor"
