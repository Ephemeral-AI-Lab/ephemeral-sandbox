from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from benchmarks.sweevo import sandbox as sweevo_sandbox
from benchmarks.sweevo.models import SWEEvoInstance


def _instance() -> SWEEvoInstance:
    return SWEEvoInstance(
        instance_id="dask__dask_2023.3.2_2023.4.0",
        repo="dask/dask",
        base_commit="abc123",
        problem_statement="",
        patch="",
        test_patch="diff --git a/foo b/foo\n",
        fail_to_pass=["dask/tests/test_cli.py::test_config_get"],
        pass_to_pass=["dask/tests/test_config.py::test_collect"],
        docker_image="example/image",
        test_cmds="pytest -q",
        environment_setup_commit="",
    )


@pytest.mark.asyncio
async def test_ensure_sweevo_test_patch_uploads_bytes_before_path(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    commands: list[str] = []

    async def fake_exec(_sandbox_id: str, command: str, **_kwargs) -> str:
        commands.append(command)
        if "git apply --check" in command:
            return "APPLYABLE"
        return ""

    monkeypatch.setattr(sweevo_sandbox, "_exec", fake_exec)

    await sweevo_sandbox.ensure_sweevo_test_patch(_instance(), "sbx-1")

    assert commands[:3] == [
        ": > /tmp/sweevo_test.patch.b64",
        "printf %s ZGlmZiAtLWdpdCBhL2ZvbyBiL2Zvbwo= >> /tmp/sweevo_test.patch.b64",
        "base64 -d /tmp/sweevo_test.patch.b64 > /tmp/sweevo_test.patch && rm -f /tmp/sweevo_test.patch.b64",
    ]


@pytest.mark.asyncio
async def test_create_sweevo_test_sandbox_does_not_apply_test_patch_before_agents(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, object] = {}

    def fake_create_sandbox(**kwargs) -> dict[str, str]:
        captured["create_kwargs"] = kwargs
        return {"id": "sbx-1"}

    service = SimpleNamespace(
        create_sandbox=fake_create_sandbox,
        get_sandbox=lambda _sandbox_id: {"id": "sbx-1"},
    )
    monkeypatch.setattr(sweevo_sandbox, "_service", lambda: service)
    monkeypatch.setattr(sweevo_sandbox, "setup_sweevo_sandbox", AsyncMock(return_value="/testbed"))
    ensure_mock = AsyncMock()
    monkeypatch.setattr(sweevo_sandbox, "ensure_sweevo_test_patch", ensure_mock)

    result = await sweevo_sandbox.create_sweevo_test_sandbox(
        _instance(),
        register_snapshot=False,
    )

    assert result["sandbox_id"] == "sbx-1"
    create_kwargs = captured["create_kwargs"]
    assert isinstance(create_kwargs, dict)
    labels = create_kwargs["labels"]
    assert isinstance(labels, dict)
    assert labels["project_dir"] == "/testbed"
    assert labels["sweevo_instance"] == "dask__dask_2023.3.2_2023.4.0"
    ensure_mock.assert_not_awaited()


@pytest.mark.asyncio
async def test_named_sweevo_sandbox_is_configured_before_reuse(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, object] = {}

    def fake_set_labels(_sandbox_id: str, labels: dict[str, str]) -> dict[str, object]:
        captured["labels"] = labels
        return {"id": "sbx-existing", "labels": labels}

    def fake_start_sandbox(sandbox_id: str) -> dict[str, object]:
        captured["started"] = sandbox_id
        return {
            "id": sandbox_id,
            "name": "sweevo-existing",
            "state": "started",
            "labels": captured["labels"],
            "project_dir": "/testbed",
        }

    service = SimpleNamespace(
        list_sandboxes=lambda: [
            {
                "id": "sbx-existing",
                "name": "sweevo-existing",
                "state": "started",
                "labels": {"managed_by": "ephemeralos"},
            }
        ],
        set_sandbox_labels=fake_set_labels,
        start_sandbox=fake_start_sandbox,
        create_sandbox=lambda **_: pytest.fail("existing sandbox should be reused"),
    )
    monkeypatch.setattr(sweevo_sandbox, "_service", lambda: service)
    monkeypatch.setattr(sweevo_sandbox, "setup_sweevo_sandbox", AsyncMock(return_value="/testbed"))

    result = await sweevo_sandbox.create_sweevo_test_sandbox(
        _instance(),
        sandbox_name="sweevo-existing",
        register_snapshot=False,
    )

    assert result["sandbox_id"] == "sbx-existing"
    assert result["reused_existing"] is True
    assert captured["started"] == "sbx-existing"
    labels = captured["labels"]
    assert isinstance(labels, dict)
    assert labels["managed_by"] == "ephemeralos"
    assert labels["project_dir"] == "/testbed"
