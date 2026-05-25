"""All typed workspace verbs operate inside one isolated session.

This is the Phase 3.3 functional-upgrade proof: iws file verbs use the same
typed primitive result shapes as the unified ephemeral path while still
discarding every upperdir write on exit.
"""

from __future__ import annotations

import uuid

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(300)
async def test_iws_all_typed_verbs_same_session(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    agent_id = "agent-behavior-upgrade"
    token = uuid.uuid4().hex[:12]
    base_dir = f"/testbed/iws-behavior-{token}"
    app_path = f"{base_dir}/app.py"
    generated_path = f"{base_dir}/generated.txt"

    opened = await _iws_rpc.enter(
        sandbox_id,
        agent_id,
        layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert opened.get("success") is True, opened
    try:
        wrote = await _iws_rpc.write_file(
            sandbox_id,
            agent_id,
            app_path,
            "def label():\n    return 'AlphaValue'\n",
        )
        assert wrote.get("success") is True, wrote
        assert wrote.get("workspace") == "isolated", wrote
        assert any(path.endswith("app.py") for path in wrote.get("changed_paths", ())), wrote

        read = await _iws_rpc.read_file(sandbox_id, agent_id, app_path)
        assert read.get("success") is True, read
        assert read.get("exists") is True, read
        assert "AlphaValue" in (read.get("content") or ""), read

        edited = await _iws_rpc.edit_file(
            sandbox_id,
            agent_id,
            app_path,
            [
                {
                    "old_text": "AlphaValue",
                    "new_text": "BetaValue",
                    "expected_occurrences": 1,
                }
            ],
        )
        assert edited.get("success") is True, edited
        assert edited.get("workspace") == "isolated", edited
        assert edited.get("applied_edits") == 1, edited

        after_edit = await _iws_rpc.read_file(sandbox_id, agent_id, app_path)
        assert after_edit.get("success") is True, after_edit
        assert "BetaValue" in (after_edit.get("content") or ""), after_edit
        assert "AlphaValue" not in (after_edit.get("content") or ""), after_edit

        grep = await _iws_rpc.grep(
            sandbox_id,
            agent_id,
            "betavalue",
            path=base_dir,
            glob_filter="*.py",
            output_mode="content",
            case_insensitive=True,
            line_numbers=True,
        )
        assert grep.get("success") is True, grep
        assert grep.get("output_mode") == "content", grep
        assert grep.get("num_matches") == 1, grep
        assert "app.py:2:" in (grep.get("content") or ""), grep
        assert "BetaValue" in (grep.get("content") or ""), grep

        glob = await _iws_rpc.glob(
            sandbox_id,
            agent_id,
            "*.py",
            path=base_dir,
        )
        assert glob.get("success") is True, glob
        assert any(name.endswith("app.py") for name in glob.get("filenames", ())), glob

        shell = await _iws_rpc.shell(
            sandbox_id,
            agent_id,
            f"printf 'from-shell-{token}\\n' > {generated_path}",
        )
        assert shell.get("success") is True, shell
        assert shell.get("workspace") == "isolated", shell
        assert any(path.endswith("generated.txt") for path in shell.get("changed_paths", ())), shell
    finally:
        await _iws_rpc.exit_(sandbox_id, agent_id)

    for path in (app_path, generated_path):
        default_read = await _iws_rpc.read_file(sandbox_id, agent_id, path)
        assert default_read.get("success") is True, default_read
        assert default_read.get("exists") is False, (
            "iws upperdir writes must be discarded on exit",
            path,
            default_read,
        )
