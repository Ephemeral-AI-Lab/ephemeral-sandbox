"""Contract test: tool intent labels stay in sync with daemon operation routes.

For every BaseTool whose ``name`` matches a verb served by
``sandbox.daemon.builtin_operations``, the tool's declared ``intent`` MUST equal the
intent the daemon dispatches that verb with. This catches drift where
``@tool(intent=READ_ONLY)`` for a verb the daemon routes as
``WRITE_ALLOWED`` (or vice versa).

Also asserts every @tool decoration produced a BaseTool with an ``intent``
attribute set to an ``Intent`` member; this is the positive complement to
the import-time ``TypeError`` in ``tools._framework.core.decorator.tool``
that fires when the caller forgets ``intent=``.
"""

from __future__ import annotations

import importlib
from collections.abc import Iterator

import pytest

from sandbox._shared.models import Intent
from sandbox.daemon.builtin_operations import WORKSPACE_TOOL_ROUTES
from tools._framework.core.base import BaseTool


# Canonical source: backend/src/sandbox/daemon/builtin_operations.py.
DAEMON_TOOL_ROUTE_INTENTS: dict[str, Intent] = dict(WORKSPACE_TOOL_ROUTES)
DAEMON_VERB_TOOL_NAME_ALIASES = {
    "shell": "exec_command",
}


_TOOL_MODULES = (
    "tools.sandbox.read_file.read_file",
    "tools.sandbox.write_file.write_file",
    "tools.sandbox.edit_file.edit_file",
    "tools.sandbox.exec_command.exec_command",
    "tools.sandbox.grep.grep",
    "tools.sandbox.glob.glob",
    "tools.ask_helper.ask_advisor.ask_advisor",
    "tools.subagent.run_subagent.run_subagent",
    "tools.isolated_workspace.enter_isolated_workspace.definition",
    "tools.isolated_workspace.exit_isolated_workspace.definition",
    "tools.submission.reducer.submit_reducer_outcome.submit_reducer_outcome",
    "tools.submission.advisor.submit_advisor_feedback.submit_advisor_feedback",
    "tools.submission.planner.submit_planner_outcome.submit_planner_outcome",
    "tools.submission.explorer.submit_exploration_result.submit_exploration_result",
    "tools.submission.root.submit_root_outcome.submit_root_outcome",
    "tools.submission.generator.submit_generator_outcome.submit_generator_outcome",
    "tools.workflow.delegate_workflow",
    "tools.workflow.check_workflow_status",
    "tools.workflow.cancel_workflow",
    "plugins.catalog.lsp.tools.hover",
    "plugins.catalog.lsp.tools.find_definitions",
    "plugins.catalog.lsp.tools.find_references",
    "plugins.catalog.lsp.tools.diagnostics",
    "plugins.catalog.lsp.tools.query_symbols",
    "plugins.catalog.lsp.tools.code_actions",
    "plugins.catalog.lsp.tools.apply_workspace_edit",
    "plugins.catalog.lsp.tools.apply_code_action",
    "plugins.catalog.lsp.tools.rename",
    "plugins.catalog.lsp.tools.format",
)


def _iter_decorated_tools() -> Iterator[BaseTool]:
    for module_name in _TOOL_MODULES:
        module = importlib.import_module(module_name)
        for attr_name in dir(module):
            obj = getattr(module, attr_name)
            if isinstance(obj, BaseTool):
                yield obj


def test_every_decorated_tool_has_intent_attribute() -> None:
    """@tool decorations MUST set tool.intent to an Intent member."""
    tools = list(_iter_decorated_tools())
    assert tools, "no @tool callsites discovered — _TOOL_MODULES is stale"
    missing: list[str] = []
    for tool in tools:
        intent = getattr(tool, "intent", None)
        if not isinstance(intent, Intent):
            missing.append(tool.name)
    assert not missing, f"@tool callsites missing intent=: {missing}"


@pytest.mark.parametrize("verb,daemon_intent", sorted(DAEMON_TOOL_ROUTE_INTENTS.items()))
def test_tool_intent_matches_daemon_handlers_table(verb: str, daemon_intent: Intent) -> None:
    """Sibling @tool and daemon handler for the same verb MUST agree on intent."""
    tool_name = DAEMON_VERB_TOOL_NAME_ALIASES.get(verb, verb)
    matching = [t for t in _iter_decorated_tools() if t.name == tool_name]
    assert matching, f"no @tool with name={tool_name!r} for daemon verb={verb!r}"
    tool = matching[0]
    assert tool.intent == daemon_intent, (
        f"@tool {verb!r} declares intent={tool.intent.value} but daemon "
        f"builtin_operations.py dispatches verb={verb!r} with intent={daemon_intent.value}; "
        "edit both or neither"
    )
