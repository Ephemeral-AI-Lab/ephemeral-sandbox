# ruff: noqa
"""E2E live test for background task progress live-tailing.

Verifies that streaming-capable background tools can push incremental
output via ``on_progress_line`` and that ``check_background_progress``
surfaces a live tail (with ``last_n_lines`` honoured) while the task is
still running. Also guards the negative case: a non-streaming background
task must NOT leak any partial output mid-run.

No API credentials required — exercises the BackgroundTaskManager and
the real CheckBackgroundProgressTool directly with the same context
wiring as ``query.py``.
"""

from __future__ import annotations

import asyncio
import logging
import shlex
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest
from pydantic import BaseModel, Field

from engine.runtime.background_tasks import BackgroundTaskManager
from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import (
    create_eval_agent,
    create_test_sandbox,
    delete_test_sandbox,
)
from tools.builtins.background.check_background_progress import (
    CheckBackgroundProgressInput,
    CheckBackgroundProgressTool,
)
from tools.builtins.background.wait_for_background_task import (
    WaitForBackgroundTaskInput,
    WaitForBackgroundTaskTool,
)
from tools.core.base import BaseTool, ToolExecutionContext, ToolResult

logger = logging.getLogger(__name__)

pytestmark = pytest.mark.e2e


def _daytona_shell_metadata(
    *,
    sandbox_id: str,
    async_sandbox: object,
    progress_callback: object,
    background_task_id: str,
) -> dict[str, Any]:
    """Build direct daytona_shell metadata with the same CI wiring as app runs."""
    from sandbox.service import SandboxService
    from sandbox.workspace import discover_workspace, inject_code_intelligence
    from tools.daytona_toolkit._daytona_utils import _wrap_bash_command

    sync_sandbox = SandboxService().get_sandbox_object(sandbox_id)
    workspace_root = discover_workspace(sync_sandbox) or "/home/daytona"
    quoted_workspace = shlex.quote(workspace_root)
    init_resp = sync_sandbox.process.exec(
        _wrap_bash_command(
            f"mkdir -p {quoted_workspace}\n"
            f"cd {quoted_workspace}\n"
            "git rev-parse --git-dir >/dev/null 2>&1 || git init >/dev/null"
        ),
        timeout=20,
    )
    assert init_resp.exit_code == 0, init_resp.result

    bootstrap_context = MagicMock()
    bootstrap_context.metadata = {}
    inject_code_intelligence(
        bootstrap_context,
        sandbox_id,
        sync_sandbox,
        workspace_root,
    )
    ci_service = bootstrap_context.metadata.get("ci_service")
    assert ci_service is not None, "expected CI service for direct Daytona daytona_shell test"

    return {
        "daytona_sandbox": async_sandbox,
        "daytona_cwd": workspace_root,
        "repo_root": workspace_root,
        "sandbox_id": sandbox_id,
        "ci_service": ci_service,
        "on_progress_line": progress_callback,
        "background_task_id": background_task_id,
    }


async def _wait_for_running_progress(
    *,
    manager: BackgroundTaskManager,
    task_id: str,
    expected_token: str,
    timeout: float = 15.0,
) -> ToolResult:
    check_tool = CheckBackgroundProgressTool()
    check_ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"background_task_manager": manager},
    )

    deadline = asyncio.get_running_loop().time() + timeout
    last_result: ToolResult | None = None
    while asyncio.get_running_loop().time() < deadline:
        result = await check_tool.execute(
            CheckBackgroundProgressInput(task_id=task_id, last_n_lines=20),
            check_ctx,
        )
        last_result = result
        if (
            not result.is_error
            and '"status": "running"' in result.output
            and expected_token in result.output
        ):
            return result
        if not result.is_error and (
            '"status": "completed"' in result.output
            or '"status": "delivered"' in result.output
            or '"status": "failed"' in result.output
        ):
            break
        await asyncio.sleep(0.5)

    pytest.fail(
        "Expected live Daytona progress before task completion. "
        f"Last check_background_progress output:\n"
        f"{last_result.output if last_result is not None else '<no result>'}"
    )


async def _wait_for_background_completion(
    *,
    manager: BackgroundTaskManager,
    task_id: str,
    timeout: float = 30.0,
) -> ToolResult:
    wait_tool = WaitForBackgroundTaskTool()
    wait_ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"background_task_manager": manager},
    )
    result = await wait_tool.execute(
        WaitForBackgroundTaskInput(
            task_id=task_id,
            timeout=timeout,
            last_n_lines=20,
        ),
        wait_ctx,
    )
    assert not result.is_error, result.output
    assert '"status": "completed"' in result.output or '"status": "delivered"' in result.output, (
        result.output
    )
    return result


class _StreamingInput(BaseModel):
    n_lines: int = Field(default=5)
    interval: float = Field(default=0.05)


class _StreamingTool(BaseTool):
    """Background-capable tool that emits progress lines via on_progress_line."""

    name: str = "fake_streaming"
    description: str = "Emit n_lines progress lines, sleeping interval between each."
    input_model: type[BaseModel] = _StreamingInput
    background: str = "optional"

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, _StreamingInput)
        on_line = context.metadata.get("on_progress_line")
        for i in range(arguments.n_lines):
            if on_line is not None:
                on_line(f"line {i + 1}")
            await asyncio.sleep(arguments.interval)
        return ToolResult(output="\n".join(f"line {i + 1}" for i in range(arguments.n_lines)))


@pytest.mark.asyncio
async def test_live_tail_visible_while_running() -> None:
    """While the streaming tool is mid-flight, check_background_progress
    must return the lines already emitted via on_progress_line, with
    last_n_lines honoured. After completion, the final output is available."""
    mgr = BackgroundTaskManager()
    tool = _StreamingTool()

    n_lines = 6
    interval = 0.08
    alias = mgr.next_alias()

    async def _coro() -> ToolResult:
        ctx = ToolExecutionContext(
            cwd=Path("/tmp"),
            metadata={"on_progress_line": mgr.make_progress_callback(alias)},
        )
        return await tool.execute(_StreamingInput(n_lines=n_lines, interval=interval), ctx)

    mgr.launch(alias, "fake_streaming", {}, _coro())

    # Wait long enough for ~3 lines to have been emitted, but not all 6.
    await asyncio.sleep(interval * 3 + interval / 2)

    check_tool = CheckBackgroundProgressTool()
    check_ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"background_task_manager": mgr},
    )

    mid_result = await check_tool.execute(
        CheckBackgroundProgressInput(task_id=alias, last_n_lines=2),
        check_ctx,
    )
    assert not mid_result.is_error, mid_result.output
    assert '"status": "running"' in mid_result.output, mid_result.output
    assert '"output"' in mid_result.output, (
        f"Expected live tail in mid-flight check, got:\n{mid_result.output}"
    )
    # last_n_lines=2 → only the most recent two streamed lines should
    # appear, and earlier ones should NOT.
    assert "line 1" not in mid_result.output, mid_result.output
    assert any(f"line {i}" in mid_result.output for i in (2, 3, 4)), mid_result.output

    # Now wait for completion through the public wait_for_background_task tool.
    final_result = await _wait_for_background_completion(
        manager=mgr,
        task_id=alias,
        timeout=5.0,
    )
    assert f"line {n_lines}" in final_result.output


@pytest.mark.asyncio
async def test_no_streaming_means_no_output_field_while_running() -> None:
    """A background task that does NOT use on_progress_line should not
    surface any partial output until it finishes."""
    mgr = BackgroundTaskManager()

    async def _coro() -> ToolResult:
        await asyncio.sleep(0.3)
        return ToolResult(output="final only")

    alias = mgr.next_alias()
    mgr.launch(alias, "noop", {}, _coro())

    await asyncio.sleep(0.05)
    snap = mgr.get_status(alias)
    assert snap and snap[0]["status"] == "running"
    assert snap[0].get("output") == "[started: noop]"

    await mgr.wait_for(alias, timeout=2.0)
    snap = mgr.get_status(alias)
    assert snap[0]["status"] in ("completed", "delivered")
    assert snap[0]["output"] == "final only"


# ===========================================================================
# Real Daytona: daytona_shell streams stdout via on_progress_line while running
# ===========================================================================


@pytest.mark.e2e
@pytest.mark.live
@pytest.mark.skipif(
    not EvalAgent.has_daytona(), reason="Daytona credentials required for live streaming test"
)
class TestDaytonaBashLiveStreaming:
    """Verify daytona_shell uses session-based streaming when launched as a
    background task, so check_background_progress sees partial output mid-run."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("nova-livestream")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_streaming_visible_via_background_manager(self, sandbox) -> None:
        """Drive daytona_shell directly through BackgroundTaskManager (no LLM):
        a slow loop must surface lines through check_background_progress
        BEFORE the command finishes."""
        from sandbox.async_client import get_async_daytona_client
        from tools.daytona_toolkit.shell_tool import daytona_shell

        client = get_async_daytona_client()
        sb = await client.get(sandbox["id"])

        mgr = BackgroundTaskManager()
        alias = mgr.next_alias()

        async def _coro() -> ToolResult:
            ctx = ToolExecutionContext(
                cwd=Path("/tmp"),
                metadata=_daytona_shell_metadata(
                    sandbox_id=sandbox["id"],
                    async_sandbox=sb,
                    progress_callback=mgr.make_progress_callback(alias),
                    background_task_id=alias,
                ),
            )
            args = daytona_shell.input_model(
                command='for i in $(seq 1 5); do echo "step_$i"; sleep 2; done',
                timeout=60,
            )
            return await daytona_shell.execute(args, ctx)

        mgr.launch(alias, "daytona_shell", {}, _coro())

        mid_result = await _wait_for_running_progress(
            manager=mgr,
            task_id=alias,
            expected_token="step_",
        )
        logger.info("[livestream] mid-flight check:\n%s", mid_result.output)
        # At least one of the early steps should have streamed through.
        assert any(f"step_{i}" in mid_result.output for i in (1, 2, 3)), mid_result.output
        assert "step_5" not in mid_result.output, mid_result.output

        final_result = await _wait_for_background_completion(
            manager=mgr,
            task_id=alias,
        )
        logger.info("[livestream] wait_for_background_task:\n%s", final_result.output)
        assert "step_5" in final_result.output, final_result.output

    @pytest.mark.asyncio
    async def test_python_script_streaming_visible_via_background_manager(self, sandbox) -> None:
        """A Python process with explicit flushing must stream lines mid-run."""
        from sandbox.async_client import get_async_daytona_client
        from tools.daytona_toolkit.shell_tool import daytona_shell

        client = get_async_daytona_client()
        sb = await client.get(sandbox["id"])

        mgr = BackgroundTaskManager()
        alias = mgr.next_alias()

        async def _coro() -> ToolResult:
            ctx = ToolExecutionContext(
                cwd=Path("/tmp"),
                metadata=_daytona_shell_metadata(
                    sandbox_id=sandbox["id"],
                    async_sandbox=sb,
                    progress_callback=mgr.make_progress_callback(alias),
                    background_task_id=alias,
                ),
            )
            args = daytona_shell.input_model(
                command=(
                    "python3 -u - <<'PY'\n"
                    "import time\n"
                    "for i in range(1, 6):\n"
                    "    print(f\"step_{i}\", flush=True)\n"
                    "    time.sleep(2)\n"
                    "PY"
                ),
                timeout=60,
            )
            return await daytona_shell.execute(args, ctx)

        mgr.launch(alias, "daytona_shell", {}, _coro())

        mid_result = await _wait_for_running_progress(
            manager=mgr,
            task_id=alias,
            expected_token="step_",
        )
        logger.info("[livestream-python] mid-flight check:\n%s", mid_result.output)
        assert any(f"step_{i}" in mid_result.output for i in (1, 2, 3)), mid_result.output
        assert "step_5" not in mid_result.output, mid_result.output

        final_result = await _wait_for_background_completion(
            manager=mgr,
            task_id=alias,
        )
        logger.info("[livestream-python] wait_for_background_task:\n%s", final_result.output)
        assert "step_5" in final_result.output, final_result.output


# ===========================================================================
# Real LLM: agent observes live tail mid-run via check_background_progress
# ===========================================================================


_LIVE_AGENT_PROMPT = """\
You are a senior developer with a remote Daytona sandbox.

You MUST use tools for every action. Never describe what you'd do — execute it.
For long-running shell commands, run them in background with "background": true,
then use check_background_progress (non-blocking) to peek at partial output,
and wait_for_background_task to block until they finish.
"""


@pytest.mark.e2e
@pytest.mark.live
@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSupernovaLiveTail:
    """An LLM-driven check that the agent can observe streaming output from a
    long-running daytona_shell task while it is still running."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("nova-livetail")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_agent_sees_partial_output_mid_run(self, sandbox) -> None:
        agent = create_eval_agent(
            system_prompt=_LIVE_AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Run this exact bash command in BACKGROUND (set background=true):\n\n"
            '  for i in $(seq 1 10); do echo "step_$i"; sleep 3; done\n\n'
            "(Total runtime ~30 seconds.)\n\n"
            "Then:\n"
            "1. Sleep ~8 seconds in the FOREGROUND (use daytona_shell with `sleep 8`,\n"
            "   background=false).\n"
            "2. Call check_background_progress(task_id='bg_1', last_n_lines=20)\n"
            "   and read the partial output. The background task should still be running.\n"
            "3. Report which step_N lines you see at that moment.\n"
            "4. Then wait_for_background_task until it completes.\n"
            "5. Report the final lines.\n"
        )

        # Verify the agent actually exercised the live-tail path.
        assert result.has_tool("check_background_progress"), (
            f"Agent never called check_background_progress; tools used: {result.tool_names}"
        )

        # Inspect every check_background_progress completion event — at least
        # one must have surfaced partial step_ output while the bg task was
        # still running, and must NOT contain the final step.
        check_completions = [
            e for e in result.tools_completed() if e.tool_name == "check_background_progress"
        ]
        saw_live_tail = False
        for evt in check_completions:
            out = evt.output or ""
            logger.info("[livetail] check_background_progress output:\n%s", out)
            if (
                '"status": "running"' in out
                and any(f"step_{i}" in out for i in (1, 2, 3, 4, 5))
                and "step_10" not in out
            ):
                saw_live_tail = True
                break
        assert saw_live_tail, (
            "No mid-flight check_background_progress call surfaced partial step_ lines. "
            f"Outputs: {[(e.output or '')[:300] for e in check_completions]}"
        )
