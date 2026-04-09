"""Unit tests for tools.posthook.submit_replan.SubmitReplanInput."""

from __future__ import annotations

import json

from tools.posthook import SubmitReplanInput


def test_submit_replan_input_accepts_json_string_lists() -> None:
    args = SubmitReplanInput.model_validate(
        {
            "add_items": json.dumps([{"agent_name": "developer", "local_id": "w1"}]),
            "cancel_ids": json.dumps(["w2"]),
        }
    )
    assert len(args.add_items) == 1
    assert args.add_items[0].agent_name == "developer"
    assert args.cancel_ids == ["w2"]
