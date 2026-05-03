"""Execution-path tests for ``LocalOverlayEngine``."""

from __future__ import annotations

import base64
import json
import re
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

from sandbox.overlay.engine import (
    RUN_DIR_PREFIX,
    LocalOverlayEngine,
)
from sandbox.overlay.engine.runtime_bundle import overlay_runtime_bundle_bytes
from sandbox.overlay.types import OverlayRunOutcome


def _meta_line(**overrides) -> str:
    base = {
        "exit_code": 0,
        "upper_bytes": 0,
        "upper_files": 0,
        "upper_changes": 0,
        "run_timings": {},
        "warnings": [],
    }
    base.update(overrides)
    return json.dumps({"_meta": base}, separators=(",", ":"))


def _change_line(rel: str, *, base: bytes | None, upper: bytes) -> str:
    return json.dumps(
        {
            "rel": rel,
            "kind": "regular",
            "base_bytes_b64": (
                None if base is None else base64.b64encode(base).decode("ascii")
            ),
            "upper_bytes_b64": base64.b64encode(upper).decode("ascii"),
            "base_existed": base is not None,
        },
        separators=(",", ":"),
    )


def test_overlay_runtime_bundle_contains_capture_runtime_only() -> None:
    raw = overlay_runtime_bundle_bytes()

    import io
    import tarfile

    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as tar:
        names = set(tar.getnames())

    assert "overlay_runtime/cli.py" in names
    assert "overlay_runtime/mounts.py" in names
    assert "overlay_runtime/classifier.py" not in names


@pytest.mark.asyncio
async def test_capture_runner_returns_raw_upper_changes(tmp_path: Path) -> None:
    diff = "\n".join(
        [
            _meta_line(upper_changes=1, upper_files=1, upper_bytes=4),
            _change_line("app.py", base=b"old\n", upper=b"new\n"),
        ]
    )

    class _ScriptedSandbox:
        async def exec(self, command: str, timeout: int | None = None):
            del timeout
            if "unshare -Urm" in command:
                match = re.search(r"--run-dir\s+(\S+)", command)
                if match is None:
                    return SimpleNamespace(result="missing run-dir", exit_code=1)
                run_dir = Path(match.group(1))
                run_dir.mkdir(parents=True, exist_ok=True)
                (run_dir / "diff.ndjson").write_text(diff, encoding="utf-8")
                (run_dir / "stdout.bin").write_text("stdout\n", encoding="utf-8")
                return SimpleNamespace(result="", exit_code=0)
            completed = subprocess.run(
                command,
                shell=True,
                text=True,
                capture_output=True,
                check=False,
            )
            return SimpleNamespace(
                result=completed.stdout + completed.stderr,
                exit_code=completed.returncode,
            )

    async def _exec(sandbox, command: str, *, timeout=None):
        return await sandbox.exec(command, timeout=timeout)

    runner = LocalOverlayEngine(
        sandbox_id=f"capture-{tmp_path.name}",
        workspace_root=str(tmp_path),
        exec_process=_exec,
    )

    outcome = await runner.execute("echo hi", sandbox=_ScriptedSandbox())

    assert isinstance(outcome, OverlayRunOutcome)
    assert outcome.stdout == "stdout\n"
    assert len(outcome.upper_changes) == 1
    assert outcome.upper_changes[0].rel == "app.py"
    assert outcome.upper_changes[0].upper_bytes == b"new\n"


def test_can_use_local_run_dir_requires_no_transport(tmp_path: Path) -> None:
    async def _unused(*_args, **_kwargs):
        raise AssertionError("unused")

    runner = LocalOverlayEngine(
        sandbox_id=f"capture-local-{tmp_path.name}",
        workspace_root=str(tmp_path),
        exec_process=_unused,
        transport=object(),  # type: ignore[arg-type]
    )

    assert runner._can_use_local_run_dir(None) is False
    assert RUN_DIR_PREFIX == "/tmp/eos-shell-overlay"
