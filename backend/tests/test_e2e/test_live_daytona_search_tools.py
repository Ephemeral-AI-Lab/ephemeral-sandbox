"""Live E2E coverage for direct Daytona search tools.

Run with:
    uv run pytest backend/tests/test_e2e/test_live_daytona_search_tools.py -m live -v -s
"""

from __future__ import annotations

import json
import os
import shlex
from pathlib import Path, PurePosixPath
from typing import Any

import pytest
from dotenv import load_dotenv

from sandbox.async_client import get_async_sandbox
from sandbox.lifecycle import shutdown_cached_client_async
from sandbox.testing import create_test_sandbox, delete_test_sandbox
from tools.core.base import ToolExecutionContextService
from sandbox.daytona_utils import (
    _build_write_text_file_command,
    _wrap_bash_command,
)
from tools.daytona_toolkit.glob import glob
from tools.daytona_toolkit.grep import grep

_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")


def _load_settings() -> dict[str, Any]:
    settings_path = Path.home() / ".ephemeralos" / "settings.json"
    if settings_path.exists():
        return json.loads(settings_path.read_text())
    return {}


_SETTINGS = _load_settings()
HAS_DAYTONA = bool(
    (os.environ.get("DAYTONA_API_KEY") or _SETTINGS.get("daytona_api_key", ""))
    and (os.environ.get("DAYTONA_API_URL") or _SETTINGS.get("daytona_api_url", ""))
)

pytestmark = [pytest.mark.e2e, pytest.mark.live]


async def _exec_checked(async_sandbox: Any, command: str, *, timeout: int = 30) -> str:
    response = await async_sandbox.process.exec(command, timeout=timeout)
    if getattr(response, "exit_code", 0) not in (0, None):
        raise AssertionError(getattr(response, "result", "") or f"command failed: {command}")
    return str(getattr(response, "result", "") or "")


@pytest.mark.skipif(not HAS_DAYTONA, reason="Daytona credentials not configured")
@pytest.mark.asyncio
async def test_live_grep_and_glob_direct_tools() -> None:
    info = create_test_sandbox(name="search-tools-live")
    sandbox_id = info["id"]
    try:
        async_sandbox = await get_async_sandbox(sandbox_id)
        cwd = (await _exec_checked(async_sandbox, "pwd", timeout=10)).strip() or "/home/daytona"
        root = str(PurePosixPath(cwd) / "search-tools-live")

        await _exec_checked(
            async_sandbox,
            "mkdir -p "
            + " ".join(
                shlex.quote(path)
                for path in (
                    f"{root}/src",
                    f"{root}/node_modules/skip",
                    f"{root}/.git/skip",
                )
            ),
            timeout=10,
        )

        files = {
            f"{root}/src/app.py": "MARKER_ALPHA = 'native grep works'\nprint(MARKER_ALPHA)\n",
            f"{root}/src/other.txt": "plain text\nMARKER_ALPHA appears here too\n",
            f"{root}/src/component.tsx": "export function Component() { return <div/> }\n",
            f"{root}/node_modules/skip/ignored.py": "MARKER_ALPHA should be pruned\n",
            f"{root}/.git/skip/ignored.py": "MARKER_ALPHA should be pruned\n",
        }
        for file_path, content in files.items():
            await _exec_checked(
                async_sandbox,
                _wrap_bash_command(_build_write_text_file_command(file_path, content)),
                timeout=10,
            )

        ctx = ToolExecutionContextService(
            cwd=Path("/tmp"),
            services={
                "sandbox_id": sandbox_id,
                "daytona_sandbox": async_sandbox,
                "repo_root": cwd,
            },
        )

        grep_result = await grep.execute(
            grep.input_model(pattern="MARKER_ALPHA", path=root),
            ctx,
        )
        assert not grep_result.is_error, grep_result.output
        grep_payload = json.loads(grep_result.output)
        grep_files = {item["file"] for item in grep_payload["matches"]}
        assert f"{root}/src/app.py" in grep_files
        assert f"{root}/src/other.txt" in grep_files
        assert all("node_modules" not in path and "/.git/" not in path for path in grep_files)

        py_glob_result = await glob.execute(
            glob.input_model(pattern="**/*.py", path=root),
            ctx,
        )
        assert not py_glob_result.is_error, py_glob_result.output
        py_glob_payload = json.loads(py_glob_result.output)
        assert py_glob_payload["files"] == [f"{root}/src/app.py"]

        tsx_glob_result = await glob.execute(
            glob.input_model(pattern="**/*.tsx", path=root),
            ctx,
        )
        assert not tsx_glob_result.is_error, tsx_glob_result.output
        tsx_glob_payload = json.loads(tsx_glob_result.output)
        assert tsx_glob_payload["files"] == [f"{root}/src/component.tsx"]
    finally:
        try:
            await shutdown_cached_client_async()
        finally:
            delete_test_sandbox(sandbox_id)
