from __future__ import annotations

import pytest

from benchmarks.sweevo import evaluation as sweevo_evaluation


@pytest.mark.asyncio
async def test_run_test_set_uses_python_subprocess_for_weird_test_ids(monkeypatch):
    captured: dict[str, str] = {}

    async def fake_exec(_sandbox_id: str, cmd: str, *, timeout: int, check: bool = False) -> str:
        captured["cmd"] = cmd
        return "EXIT_CODE=0"

    monkeypatch.setattr(sweevo_evaluation, "_exec", fake_exec)

    passed = await sweevo_evaluation._run_test_set(
        "sbx-1",
        "/testbed",
        ['tests/test_networks.py::test_address_invalid[\n@example.com-None]'],
        "pytest -q",
    )

    assert passed == 1
    assert "subprocess.run(argv" in captured["cmd"]
    assert 'tests/test_networks.py::test_address_invalid[\\n@example.com-None]' in captured["cmd"]


@pytest.mark.asyncio
async def test_run_test_set_counts_passed_tests_from_pytest_summary(monkeypatch):
    async def fake_exec(_sandbox_id: str, cmd: str, *, timeout: int, check: bool = False) -> str:
        return "2 failed, 3 passed\nEXIT_CODE=1"

    monkeypatch.setattr(sweevo_evaluation, "_exec", fake_exec)

    passed = await sweevo_evaluation._run_test_set(
        "sbx-1",
        "/testbed",
        ["a", "b", "c", "d", "e"],
        "pytest -q",
    )

    assert passed == 3
