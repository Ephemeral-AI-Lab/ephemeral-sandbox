"""Plan §A11 — ``coding_plan_mode_active`` audit field in run.json.

Three cases per S1.1 acceptance criteria:

* (a) coding_plan_mode_active=True explicit → run.json carries True
* (b) default (no kwarg) → run.json carries False
* (c) no-model-registered fallback at engine.py:117 yields False without error

Uses ``is True`` / ``is False`` identity comparison to catch type-drift (a
truthy str slipping through would otherwise pass ``==`` checks).
"""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import patch

from task_center_runner.audit.recorder import AuditRecorder


def _read_run_json(run_dir: Path) -> dict:
    return json.loads((run_dir / "run.json").read_text(encoding="utf-8"))


def test_coding_plan_mode_active_true_recorded_in_run_json(tmp_path: Path) -> None:
    recorder = AuditRecorder(
        tmp_path,
        task_center_run_id="test-run",
        scenario_name="s",
        instance_id="i",
        sandbox_id="sb",
        coding_plan_mode_active=True,
    )
    recorder.start()
    recorder.dispose()

    payload = _read_run_json(tmp_path)
    assert payload["coding_plan_mode_active"] is True


def test_coding_plan_mode_active_defaults_false_in_run_json(tmp_path: Path) -> None:
    recorder = AuditRecorder(
        tmp_path,
        task_center_run_id="test-run",
        scenario_name="s",
        instance_id="i",
        sandbox_id="sb",
    )
    recorder.start()
    recorder.dispose()

    payload = _read_run_json(tmp_path)
    assert payload["coding_plan_mode_active"] is False


def test_no_model_registered_resolves_to_false(tmp_path: Path) -> None:
    """Engine.py:117 fallback: try_get_active_model_kwargs() returns None
    (mock-runner / uninit-store path) → coding_plan_mode_active resolves to False
    via ``(... or {})`` and the recorder is built without error."""
    with patch(
        "config.model_config.try_get_active_model_kwargs", return_value=None
    ):
        # Mirror the engine.py:117 expression exactly.
        from config.model_config import try_get_active_model_kwargs

        class_path = (try_get_active_model_kwargs() or {}).get("class_path", "") or ""
        coding_plan_mode_active = class_path.startswith("providers.clients.coding_plan.")

    assert coding_plan_mode_active is False

    recorder = AuditRecorder(
        tmp_path,
        task_center_run_id="test-run",
        scenario_name="s",
        instance_id="i",
        sandbox_id="sb",
        coding_plan_mode_active=coding_plan_mode_active,
    )
    recorder.start()
    recorder.dispose()

    payload = _read_run_json(tmp_path)
    assert payload["coding_plan_mode_active"] is False
