"""Tests for shared sandbox mutation result formatting."""

from __future__ import annotations

from sandbox._shared.timing_keys import TimingKey
from tools.sandbox._lib.mutation_result import mutation_tool_result


def test_mutation_tool_result_normalizes_timing_key_enum_metadata() -> None:
    result = mutation_tool_result(
        success=True,
        success_status="ok",
        paths=["/ws/a.py"],
        timings={
            TimingKey.PREPARE_TOTAL: 0.1,
            TimingKey.APPLY_TOTAL: 0.2,
        },
    )

    assert result.metadata["timings"] == {
        "occ.prepare.total_s": 0.1,
        "occ.apply.total_s": 0.2,
    }
