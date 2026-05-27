from __future__ import annotations

from types import ModuleType
import sys

from task_center_runner.benchmarks.sweevo import setup as sweevo_dataset


def test_load_sweevo_rows_prefers_cached_arrow(monkeypatch):
    sweevo_dataset._load_sweevo_rows.cache_clear()
    cached_rows = ({"instance_id": "cached"},)

    monkeypatch.setattr(sweevo_dataset, "_load_cached_arrow_rows", lambda *_args: cached_rows)

    assert sweevo_dataset._load_sweevo_rows("Fsoft-AIC/SWE-EVO", "test") == cached_rows


def test_load_sweevo_rows_falls_back_to_cached_arrow_after_remote_failure(monkeypatch):
    sweevo_dataset._load_sweevo_rows.cache_clear()
    cached_rows = ({"instance_id": "cached"},)
    calls = {"count": 0}

    def fake_cached(*_args):
        calls["count"] += 1
        return None if calls["count"] == 1 else cached_rows

    fake_datasets = ModuleType("datasets")

    def _boom(*_args, **_kwargs):
        raise RuntimeError("hf unavailable")

    fake_datasets.load_dataset = _boom

    monkeypatch.setattr(sweevo_dataset, "_load_cached_arrow_rows", fake_cached)
    monkeypatch.setitem(sys.modules, "datasets", fake_datasets)

    assert sweevo_dataset._load_sweevo_rows("Fsoft-AIC/SWE-EVO", "test") == cached_rows
