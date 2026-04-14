# ruff: noqa
"""Live e2e tests — Suite 2: Multiple posthook tools per role.

Roles with >1 posthook tool must select the correct one based on context.
Tests the LLM's tool selection when presented with multiple posthook options:

  1. Developer (post_note + request_replan) → picks post_note for normal completion
  2. Developer (post_note + request_replan) → picks request_replan for mis-scoped task
  3. Replanner (add_tasks + declare_blocker + cancel_and_redraft) → picks add_tasks
  4. Replanner → picks declare_blocker for shared dependency break
  5. Replanner → picks cancel_and_redraft for wrong decomposition

Run with:
    .venv/bin/python -m pytest backend/tests/test_e2e/test_posthook_multi_tool_live.py -v -m live -o "addopts="
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from external_trigger.runner import run as run_trigger
from tests.test_e2e.conftest import create_eval_agent
from tools.context.toolkit import PostNoteTool
from tools.posthook.toolkit import (
    AddTasksTool,
    CancelAndRedraftTool,
    DeclareBlockerTool,
    RequestReplanTool,
)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

HAS_CREDENTIALS = EvalAgent.has_credentials()

# Tool sets matching PosthookTools.from_context role resolution
DEVELOPER_TOOLS = [PostNoteTool(), RequestReplanTool()]
REPLANNER_TOOLS = [AddTasksTool(), DeclareBlockerTool(), CancelAndRedraftTool()]


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def agent():
    if not HAS_CREDENTIALS:
        pytest.skip("No LLM credentials configured")
    return create_eval_agent()


@pytest.fixture(scope="module")
def api_client(agent):
    return agent.api_client


# ---------------------------------------------------------------------------
# Developer role: post_note vs request_replan
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_developer_picks_post_note_on_success(api_client):
    """Developer who completed work successfully should pick post_note, not request_replan."""
    messages = [
        {"role": "user", "content": (
            "Fix the broken import in pkg/io.py. The file imports load_defaults "
            "from pkg._compat but the function was renamed to get_defaults."
        )},
        {"role": "assistant", "content": (
            "I've fixed pkg/io.py line 3: changed `from pkg._compat import load_defaults` "
            "to `from pkg._compat import get_defaults`. All tests pass. "
            "The fix was straightforward — single import rename."
        )},
    ]

    result = await run_trigger(
        messages=messages,
        system_prompt=(
            "You are a developer agent. You have two options:\n"
            "  post_note — report your completed work.\n"
            "  request_replan — request a replan if the task is mis-scoped.\n"
            "Choose based on whether you succeeded or need restructuring."
        ),
        prompt=(
            "Your main work is complete. You must now submit your results "
            "by calling one of: post_note (report success), request_replan (task is mis-scoped). "
            "Summarize what you accomplished and call the appropriate tool."
        ),
        tools=DEVELOPER_TOOLS,
        api_client=api_client,
        max_tokens_per_turn=500,
        max_turns=5,
    )

    assert result.tool_name == "post_note", (
        f"Developer who succeeded should post_note, got {result.tool_name}"
    )
    content = result.tool_input.get("content", "")
    assert len(content) > 10, f"Note should be meaningful: {content}"


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_developer_picks_request_replan_on_misscope(api_client):
    """Developer who discovers task is mis-scoped should pick request_replan."""
    messages = [
        {"role": "user", "content": (
            "Fix the authentication validation in src/auth/helpers.py."
        )},
        {"role": "assistant", "content": (
            "I've read src/auth/helpers.py. This file contains only re-exports — "
            "it re-exports validate_token and check_permissions from src/auth/middleware.py. "
            "There is no validation logic here at all. The actual validation code "
            "lives in src/auth/middleware.py lines 45-120. This task is scoped to "
            "the wrong file. I cannot fix auth validation by editing helpers.py."
        )},
    ]

    result = await run_trigger(
        messages=messages,
        system_prompt=(
            "You are a developer agent. You have two options:\n"
            "  post_note — report your completed work.\n"
            "  request_replan — request a replan if the task is mis-scoped.\n"
            "Choose based on whether you succeeded or need restructuring."
        ),
        prompt=(
            "Your main work is complete. You must now submit your results "
            "by calling one of: post_note (report success), request_replan (task is mis-scoped). "
            "Summarize what you accomplished and call the appropriate tool."
        ),
        tools=DEVELOPER_TOOLS,
        api_client=api_client,
        max_tokens_per_turn=500,
        max_turns=5,
    )

    assert result.tool_name == "request_replan", (
        f"Developer with mis-scoped task should request_replan, got {result.tool_name}"
    )
    reason = result.tool_input.get("reason", "")
    assert len(reason) > 10, f"Replan reason should be meaningful: {reason}"


# ---------------------------------------------------------------------------
# Replanner role: add_tasks vs declare_blocker vs cancel_and_redraft
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_replanner_picks_add_tasks_for_transient_failure(api_client):
    """Replanner facing a transient timeout should pick add_tasks (retry)."""
    prompt = (
        "## Failed task\n"
        "**Task ID:** fix-io\n"
        "**Scope:** pkg/io.py\n"
        "**Failure reason:** sandbox timeout after 30s during pytest execution\n\n"
        "## Sibling statuses\n"
        "- **fix-parser** [DONE]: Fix parser module (scope: pkg/parser.py)\n"
        "- **fix-cli** [DONE]: Fix CLI entry points (scope: pkg/cli.py)\n"
        "- **fix-utils** [RUNNING]: Fix utility helpers (scope: pkg/utils.py)\n\n"
        "## Notes\n"
        "**[system on fix-io]:** sandbox timeout after 30s. pytest did not complete. "
        "No code changes were made.\n"
        "**[developer on fix-parser]:** Fixed 3 import sites. All tests pass.\n\n"
        "## Available agents\n"
        "- developer, team_planner, validator\n\n"
        "Draft corrective tasks to recover. Call exactly ONE tool."
    )

    result = await run_trigger(
        messages=[],
        system_prompt=(
            "You are a replanner agent. A task has failed. Call exactly ONE action:\n"
            "  add_tasks — isolated failure, just needs a retry or follow-up.\n"
            "  declare_blocker — shared dependency is broken, pause siblings.\n"
            "  cancel_and_redraft — tasks are stale, cancel and replace.\n"
            "Choose based on the evidence."
        ),
        prompt=prompt,
        tools=REPLANNER_TOOLS,
        api_client=api_client,
        max_tokens_per_turn=1500,
        max_turns=5,
    )

    assert result.tool_name == "add_tasks", (
        f"Transient timeout should produce add_tasks, got {result.tool_name}"
    )
    add_tasks = result.tool_input.get("add_tasks", [])
    assert len(add_tasks) >= 1, "Should add at least one corrective task"


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_replanner_picks_declare_blocker_for_shared_break(api_client):
    """Replanner facing a shared import break should pick declare_blocker."""
    prompt = (
        "## Failed task\n"
        "**Task ID:** fix-io\n"
        "**Scope:** pkg/io.py\n"
        "**Failure reason:** ImportError: cannot import name 'load_defaults' from 'pkg._compat'\n\n"
        "## Sibling statuses\n"
        "- **fix-compat** [DONE]: Refactor compat module (scope: pkg/_compat.py)\n"
        "- **fix-io** [FAILED]: Fix IO module (scope: pkg/io.py)\n"
        "- **fix-parser** [RUNNING]: Fix parser — imports load_defaults from pkg._compat (scope: pkg/parser.py)\n"
        "- **fix-cli** [RUNNING]: Fix CLI — imports load_defaults from pkg._compat (scope: pkg/cli.py)\n\n"
        "## Notes\n"
        "**[developer on fix-compat]:** Renamed load_defaults() → get_defaults() in pkg/_compat.py.\n"
        "**[developer on fix-io]:** FAILED: ImportError. pkg/io.py line 3 imports load_defaults. "
        "This symbol was renamed by fix-compat.\n"
        "**[developer on fix-parser]:** pkg/parser.py line 7 uses 'from pkg._compat import load_defaults'. "
        "Will hit the same error.\n"
        "**[developer on fix-cli]:** cli.py line 2 imports load_defaults from pkg._compat.\n\n"
        "## Available agents\n"
        "- developer, team_planner, validator\n\n"
        "Draft corrective tasks to recover. Call exactly ONE tool."
    )

    result = await run_trigger(
        messages=[],
        system_prompt=(
            "You are a replanner agent. A task has failed. Read the failure context, "
            "sibling statuses, and notes, then call exactly ONE action:\n\n"
            "  add_tasks — the failure is ISOLATED to one task. Other siblings are "
            "healthy and unaffected. Just retry or add follow-up work.\n\n"
            "  declare_blocker — a SHARED dependency is broken and RUNNING siblings "
            "will hit the SAME error. The root cause file must be fixed before "
            "siblings can proceed. Use this when multiple tasks import from or "
            "depend on the same broken file.\n\n"
            "  cancel_and_redraft — tasks are stale or mis-scoped (wrong files, "
            "wrong approach). Cancel them and replace with corrected work.\n\n"
            "CRITICAL: If the notes show that RUNNING siblings import the same "
            "broken symbol/file that caused the failure, that is a SHARED blocker — "
            "call declare_blocker, NOT add_tasks."
        ),
        prompt=prompt,
        tools=REPLANNER_TOOLS,
        api_client=api_client,
        max_tokens_per_turn=1500,
        max_turns=5,
    )

    assert result.tool_name == "declare_blocker", (
        f"Shared import break should produce declare_blocker, got {result.tool_name}"
    )
    root_paths = result.tool_input.get("root_cause_paths", [])
    assert len(root_paths) >= 1, "Blocker must specify root cause paths"
    reason = result.tool_input.get("reason", "")
    assert len(reason) > 10, f"Blocker reason too short: {reason}"


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_replanner_picks_cancel_and_redraft_for_wrong_decomposition(api_client):
    """Replanner facing wrong decomposition should pick cancel_and_redraft."""
    prompt = (
        "## Failed task\n"
        "**Task ID:** fix-io-compat\n"
        "**Scope:** pkg/io.py\n"
        "**Failure reason:** Cannot fix from consumer side. pkg/_compat.py missing compat_shim export.\n\n"
        "## Sibling statuses\n"
        "- **fix-io-compat** [FAILED]: Fix compat usage in io (scope: pkg/io.py)\n"
        "- **fix-parser-compat** [FAILED]: Fix compat usage in parser (scope: pkg/parser.py)\n"
        "- **fix-cli-compat** [FAILED]: Fix compat usage in CLI (scope: pkg/cli.py)\n"
        "- **fix-api-compat** [FAILED]: Fix compat usage in API (scope: pkg/api.py)\n\n"
        "## Notes\n"
        "**[developer on fix-io-compat]:** Cannot fix from consumer side. "
        "pkg/_compat.py never exported compat_shim. The fix must be in pkg/_compat.py itself. "
        "No task owns that file.\n"
        "**[developer on fix-parser-compat]:** Same root cause. Plan decomposed by consumer "
        "but the fix is in the shared source.\n"
        "**[developer on fix-cli-compat]:** Same. pkg/_compat.py needs the export. "
        "All 4 consumer tasks are mis-scoped.\n"
        "**[developer on fix-api-compat]:** Same root cause.\n\n"
        "## Available agents\n"
        "- developer, team_planner, validator\n\n"
        "Draft corrective tasks to recover. Call exactly ONE tool."
    )

    result = await run_trigger(
        messages=[],
        system_prompt=(
            "You are a replanner agent. A task has failed. Read the failure context, "
            "sibling statuses, and notes, then call exactly ONE action:\n\n"
            "  add_tasks — the failure is ISOLATED to one task. Other siblings are "
            "healthy and unaffected. Just retry or add follow-up work.\n\n"
            "  declare_blocker — a shared dependency is broken and RUNNING siblings "
            "will hit the same error. Use when the root cause file must be fixed.\n\n"
            "  cancel_and_redraft — the plan DECOMPOSITION is wrong. Tasks are scoped "
            "to the wrong files, or all tasks share the same structural flaw. "
            "Cancel the mis-scoped tasks and replace with corrected work that "
            "targets the right files.\n\n"
            "CRITICAL: When ALL sibling tasks failed with the SAME root cause "
            "because the plan split work by consumer instead of by source, "
            "and no task owns the file that actually needs fixing — that is a "
            "WRONG DECOMPOSITION. Call cancel_and_redraft with the failed task IDs "
            "in cancel_ids and provide replacement tasks that target the correct file."
        ),
        prompt=prompt,
        tools=REPLANNER_TOOLS,
        api_client=api_client,
        max_tokens_per_turn=2000,
        max_turns=5,
    )

    assert result.tool_name == "cancel_and_redraft", (
        f"Wrong decomposition should produce cancel_and_redraft, got {result.tool_name}"
    )
    cancel_ids = set(result.tool_input.get("cancel_ids", []))
    assert len(cancel_ids) >= 3, f"Should cancel most mis-scoped tasks, got {cancel_ids}"
    add_tasks = result.tool_input.get("add_tasks", [])
    assert len(add_tasks) >= 1, "Should provide replacement tasks"
