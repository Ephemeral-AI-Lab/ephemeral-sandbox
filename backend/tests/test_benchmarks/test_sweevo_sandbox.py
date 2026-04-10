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
    uploaded: list[tuple[bytes, str]] = []

    class _FakeFS:
        async def upload_file(self, content, path):  # type: ignore[no-untyped-def]
            uploaded.append((content, path))

    monkeypatch.setattr(
        sweevo_sandbox,
        "_get_sandbox",
        AsyncMock(return_value=SimpleNamespace(fs=_FakeFS())),
    )
    exec_mock = AsyncMock(side_effect=["APPLYABLE", ""])
    monkeypatch.setattr(sweevo_sandbox, "_exec", exec_mock)

    await sweevo_sandbox.ensure_sweevo_test_patch(_instance(), "sbx-1")

    assert uploaded == [(b"diff --git a/foo b/foo\n", "/tmp/sweevo_test.patch")]
    assert not any(
        'base64 -d > /tmp/sweevo_test.patch' in call.args[1]
        for call in exec_mock.await_args_list
    )
