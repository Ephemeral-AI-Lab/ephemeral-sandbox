"""Tests for shell pipeline overlay/OCC composition."""

from __future__ import annotations

from sandbox.occ.changeset import ChangesetResult
from sandbox.overlay.types import ConflictInfo, OverlayRunOutcome, UpperChange
from sandbox.runtime.pipelines import shell_pipeline


class _Overlay:
    def __init__(self, outcome: OverlayRunOutcome) -> None:
        self.outcome = outcome
        self.calls: list[str] = []

    async def execute(self, command: str, **kwargs):
        del kwargs
        self.calls.append(command)
        return self.outcome


class _OCC:
    def __init__(self, result: ChangesetResult) -> None:
        self.result = result
        self.calls: list[tuple[tuple[UpperChange, ...], dict[str, object]]] = []

    def apply_changeset(self, upper_changes, **kwargs):
        self.calls.append((tuple(upper_changes), dict(kwargs)))
        return self.result


def _success_outcome() -> OverlayRunOutcome:
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
    )


async def test_shell_pipeline_overlay_reject_skips_occ() -> None:
    overlay = _Overlay(
        OverlayRunOutcome(
            exit_code=207,
            stdout="",
            upper_changes=(),
            overlay_rejected=True,
            conflict=ConflictInfo(
                reason="overlay_upper_full",
                conflict_file=None,
                message="overlay_upper_full",
            ),
            warnings=("overlay_upper_full",),
        )
    )
    occ = _OCC(ChangesetResult(success=True, status="noop"))

    result = await shell_pipeline(
        command="echo hi",
        overlay_engine=overlay,
        occ_engine=occ,
    )

    assert overlay.calls == ["echo hi"]
    assert occ.calls == []
    assert result.conflict is not None
    assert result.conflict.reason == "overlay_upper_full"


async def test_shell_pipeline_projects_occ_verdict_without_classifying() -> None:
    overlay = _Overlay(_success_outcome())
    occ = _OCC(
        ChangesetResult(
            success=True,
            status="committed",
            ledgered=("/workspace/app.py",),
            direct_merged=("/workspace/.cache/a",),
        )
    )

    result = await shell_pipeline(
        command="printf ok",
        overlay_engine=overlay,
        occ_engine=occ,
        agent_id="agent-a",
    )

    assert len(occ.calls) == 1
    assert occ.calls[0][0][0].rel == "app.py"
    assert result.changed_paths == ("/workspace/.cache/a", "/workspace/app.py")
    assert result.conflict is None


async def test_shell_pipeline_occ_conflict_keeps_direct_merge_projection() -> None:
    overlay = _Overlay(_success_outcome())
    occ = _OCC(
        ChangesetResult(
            success=False,
            status="aborted_version",
            direct_merged=("/workspace/.cache/a",),
            conflict_reason="patch_failed",
            conflict_file="/workspace/app.py",
        )
    )

    result = await shell_pipeline(
        command="printf ok",
        overlay_engine=overlay,
        occ_engine=occ,
    )

    assert result.changed_paths == ("/workspace/.cache/a",)
    assert result.conflict is not None
    assert result.conflict.reason == "patch_failed"
    assert result.conflict.conflict_file == "/workspace/app.py"
