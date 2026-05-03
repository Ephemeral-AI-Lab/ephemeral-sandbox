"""Slice 5a command-executor tests: overlay captures, OCC owns policy."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from sandbox.occ.changeset import ChangesetResult
from sandbox.runtime.shell_command_executor import AuditedCommandExecutor
from sandbox.overlay.types import (
    OverlayPolicyReject,
    OverlayRunOutcome,
    UpperChange,
)


@pytest.fixture
def workspace(tmp_path: Path) -> Path:
    (tmp_path / "app.py").write_text("old\n", encoding="utf-8")
    return tmp_path


def _outcome_with_upper_change() -> OverlayRunOutcome:
    return OverlayRunOutcome(
        exit_code=0,
        stdout="ran\n",
        upper_changes=(
            UpperChange(
                rel="app.py",
                kind="regular",
                base_bytes=b"old\n",
                upper_bytes=b"new\n",
                base_existed=True,
            ),
        ),
        overlay_rejected=False,
        conflict=None,
        warnings=(),
        overlay_run_timings={},
        overlay_stage_timings={},
        policy_reject=None,
    )


def _make_executor(
    workspace: Path,
    *,
    write_coordinator,
    sandbox_id: str = "slice-5a-test",
) -> AuditedCommandExecutor:
    return AuditedCommandExecutor(
        sandbox_id=sandbox_id,
        workspace_root=str(workspace),
        write_coordinator=write_coordinator,
        rebind_sandbox=lambda _sandbox: None,
        direct_runtime=False,
    )


@pytest.mark.asyncio
async def test_overlay_reject_skips_occ_changeset(workspace: Path) -> None:
    write_coordinator = MagicMock()
    write_coordinator.apply_changeset = MagicMock()

    reject = OverlayPolicyReject(reason="overlay_upper_full", paths=())
    reject_outcome = OverlayRunOutcome(
        exit_code=207,
        stdout="",
        upper_changes=(),
        overlay_rejected=True,
        conflict=None,
        warnings=("overlay_upper_full",),
        overlay_run_timings={},
        overlay_stage_timings={},
        policy_reject=reject,
    )
    fake_overlay = SimpleNamespace(execute=AsyncMock(return_value=reject_outcome))

    executor = _make_executor(workspace, write_coordinator=write_coordinator)
    executor._ensure_overlay_engine = AsyncMock(return_value=fake_overlay)  # type: ignore[method-assign]

    result = await executor.cmd(SimpleNamespace(), "echo big")

    assert write_coordinator.apply_changeset.call_count == 0
    assert result.conflict_reason == "overlay_upper_full"
    assert result.conflict_file is None
    assert result.changed_paths == []


@pytest.mark.asyncio
async def test_overlay_success_then_occ_conflict_surfaces_patch_failed(
    workspace: Path,
) -> None:
    write_coordinator = MagicMock()
    write_coordinator.apply_changeset = MagicMock(
        return_value=ChangesetResult(
            success=False,
            status="aborted_version",
            conflict_reason="patch_failed",
            conflict_file=str(workspace / "app.py"),
        )
    )
    fake_overlay = SimpleNamespace(execute=AsyncMock(return_value=_outcome_with_upper_change()))

    executor = _make_executor(workspace, write_coordinator=write_coordinator)
    executor._ensure_overlay_engine = AsyncMock(return_value=fake_overlay)  # type: ignore[method-assign]

    result = await executor.cmd(SimpleNamespace(), "echo hi")

    assert result.conflict_reason == "patch_failed"
    assert result.conflict_file == str(workspace / "app.py")
    assert result.changed_paths == []
    assert write_coordinator.apply_changeset.call_count == 1


@pytest.mark.asyncio
async def test_argv_overflow_surfaces_as_argv_too_large(workspace: Path) -> None:
    write_coordinator = MagicMock()
    write_coordinator.apply_changeset = MagicMock(
        return_value=ChangesetResult(
            success=False,
            status="failed",
            conflict_reason="argv_too_large",
            conflict_file=str(workspace / "app.py"),
        )
    )
    fake_overlay = SimpleNamespace(execute=AsyncMock(return_value=_outcome_with_upper_change()))

    executor = _make_executor(workspace, write_coordinator=write_coordinator)
    executor._ensure_overlay_engine = AsyncMock(return_value=fake_overlay)  # type: ignore[method-assign]

    result = await executor.cmd(SimpleNamespace(), "echo hi")

    assert result.conflict_reason == "argv_too_large"
    assert result.conflict_file == str(workspace / "app.py")
    assert result.changed_paths == []


@pytest.mark.asyncio
async def test_unrelated_runtime_error_propagates(workspace: Path) -> None:
    write_coordinator = MagicMock()
    write_coordinator.apply_changeset = MagicMock(side_effect=RuntimeError("disk full"))
    fake_overlay = SimpleNamespace(execute=AsyncMock(return_value=_outcome_with_upper_change()))

    executor = _make_executor(workspace, write_coordinator=write_coordinator)
    executor._ensure_overlay_engine = AsyncMock(return_value=fake_overlay)  # type: ignore[method-assign]

    with pytest.raises(RuntimeError, match="disk full"):
        await executor.cmd(SimpleNamespace(), "echo hi")
