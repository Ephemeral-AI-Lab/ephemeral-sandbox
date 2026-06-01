"""Contract: ``AdvisorApprovalPreHook`` wiring on ``make_submission_tools()``.

Mirrors Case 10 of
``backend/tests/unit_test/test_tools/test_submission/test_advisor_approval_prehook.py``
but at the runner-visible package surface — the runner imports terminals
via ``tools.submission.make_submission_tools()`` and via direct module
imports inside its late-import block. The structural guard here protects
against the runner picking up a divergent terminal that lacks the gate
(e.g. a future helper-style refactor that strips ``pre_hooks``).

Offline check — no Daytona, no Postgres, no event loop.
"""

from __future__ import annotations

from tools.submission import make_submission_tools
from tools._hooks.advisor_approval import AdvisorApprovalPreHook


_MAIN_TERMINAL_NAMES = frozenset(
    {
        "submit_planner_outcome",
        "submit_generator_outcome",
        "submit_reducer_outcome",
    }
)

_HELPER_TERMINAL_NAMES = frozenset(
    {
        "submit_advisor_feedback",
        "submit_exploration_result",
    }
)

_ROOT_TERMINAL_NAMES = frozenset({"submit_root_outcome"})


def test_main_terminals_carry_advisor_approval_prehook() -> None:
    """Every main-role terminal in ``make_submission_tools()`` carries the gate."""
    tools_by_name = {tool.name: tool for tool in make_submission_tools()}
    for name in _MAIN_TERMINAL_NAMES:
        tool = tools_by_name.get(name)
        assert tool is not None, f"{name!r} missing from make_submission_tools()"
        hooks = tuple(getattr(tool, "pre_hooks", ()) or ())
        advisor_hooks = [hook for hook in hooks if isinstance(hook, AdvisorApprovalPreHook)]
        assert len(advisor_hooks) == 1, (
            f"{name!r}: expected exactly one AdvisorApprovalPreHook, got {hooks!r}"
        )
        assert advisor_hooks[0].target_tool == name, (
            f"{name!r}: hook.target_tool={advisor_hooks[0].target_tool!r}"
        )


def test_non_main_terminals_omit_advisor_approval_prehook() -> None:
    """Root/helper/subagent terminals must NOT carry the advisor gate."""
    tools_by_name = {tool.name: tool for tool in make_submission_tools()}
    for name in _HELPER_TERMINAL_NAMES | _ROOT_TERMINAL_NAMES:
        tool = tools_by_name.get(name)
        assert tool is not None, f"{name!r} missing from make_submission_tools()"
        hooks = tuple(getattr(tool, "pre_hooks", ()) or ())
        advisor_hooks = [hook for hook in hooks if isinstance(hook, AdvisorApprovalPreHook)]
        assert not advisor_hooks, (
            f"{name!r}: non-main terminal must not carry AdvisorApprovalPreHook "
            f"(found {advisor_hooks!r})"
        )


def test_make_submission_tools_covers_every_known_terminal() -> None:
    """``make_submission_tools()`` returns the full main + helper set.

    Guards against silently dropping a terminal — if the factory loses one,
    the gate-presence test above can't catch it (it only iterates known
    names). This test catches that by asserting set equality.
    """
    factory_names = {tool.name for tool in make_submission_tools()}
    assert factory_names == _MAIN_TERMINAL_NAMES | _HELPER_TERMINAL_NAMES | _ROOT_TERMINAL_NAMES, (
        f"make_submission_tools() returned {factory_names!r}"
    )
