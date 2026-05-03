"""Tests for overlay JSON/base64 wire helpers."""

from __future__ import annotations

from sandbox.overlay.types import (
    ConflictInfo,
    OverlayRunOutcome,
    ShellResult,
    UpperChange,
)
from sandbox.overlay.wire import (
    overlay_outcome_from_dict,
    overlay_outcome_to_dict,
    shell_result_from_dict,
    shell_result_to_dict,
    upper_change_from_dict,
    upper_change_to_dict,
)


def test_upper_change_bytes_round_trip_through_json_shape() -> None:
    change = UpperChange(
        rel="bin.dat",
        kind="regular",
        base_bytes=b"\x00old",
        upper_bytes=b"\xffnew",
        base_existed=True,
    )

    decoded = upper_change_from_dict(upper_change_to_dict(change))

    assert decoded == change


def test_overlay_success_outcome_decodes_to_typed_result() -> None:
    outcome = OverlayRunOutcome(
        exit_code=0,
        stdout="ok\n",
        upper_changes=(
            UpperChange(
                rel="a.txt",
                kind="regular",
                base_bytes=None,
                upper_bytes=b"a\n",
                base_existed=False,
            ),
        ),
        overlay_rejected=False,
        conflict=None,
    )

    decoded = overlay_outcome_from_dict(overlay_outcome_to_dict(outcome))

    assert decoded.exit_code == 0
    assert decoded.upper_changes[0].upper_bytes == b"a\n"
    assert decoded.overlay_rejected is False


def test_overlay_reject_outcome_decodes_to_typed_result() -> None:
    outcome = OverlayRunOutcome(
        exit_code=207,
        stdout="",
        upper_changes=(),
        overlay_rejected=True,
        conflict=ConflictInfo(
            reason="overlay_upper_full",
            conflict_file=None,
            message="overlay_upper_full",
        ),
    )

    decoded = overlay_outcome_from_dict(overlay_outcome_to_dict(outcome))

    assert decoded.overlay_rejected is True
    assert decoded.conflict is not None
    assert decoded.conflict.reason == "overlay_upper_full"


def test_shell_result_round_trip() -> None:
    result = ShellResult(
        result="ok",
        exit_code=0,
        changed_paths=("/workspace/a.txt",),
        overlay_stage_timings={"total": 0.1},
    )

    decoded = shell_result_from_dict(shell_result_to_dict(result))

    assert decoded.changed_paths == ("/workspace/a.txt",)
    assert decoded.overlay_stage_timings == {"total": 0.1}
