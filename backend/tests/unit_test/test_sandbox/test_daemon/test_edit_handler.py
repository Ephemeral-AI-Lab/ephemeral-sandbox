from __future__ import annotations

import time

from sandbox.runtime.daemon.handler.tools.edit import _edit_out_of_workspace


def test_edit_out_of_workspace_anchor_miss_returns_conflict(tmp_path) -> None:
    target = tmp_path / "probe.txt"
    target.write_text("alpha\n", encoding="utf-8")

    result = _edit_out_of_workspace(
        abs_path=str(target),
        edits=(("missing\n", "replacement\n", 1),),
        total_start=time.perf_counter(),
    )

    assert result["success"] is False
    assert result["applied_edits"] == 0
    assert result["status"] == "aborted_overlap"
    assert result["changed_paths"] == [str(target)]
    assert result["conflict_reason"]
    conflict = result["conflict"]
    assert isinstance(conflict, dict)
    assert conflict["reason"] == "aborted_overlap"
    assert conflict["conflict_file"] == str(target)
