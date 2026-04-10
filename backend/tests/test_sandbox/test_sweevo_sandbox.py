"""Tests for SWE-EVO sandbox provisioning helpers."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from benchmarks.sweevo.models import _REPO_DIR, SWEEvoInstance, _normalize_sweevo_image_ref


def _instance() -> SWEEvoInstance:
    return SWEEvoInstance(
        instance_id="pydantic__pydantic_v2.6.0b1_v2.6.0",
        repo="pydantic/pydantic",
        base_commit="abc123",
        problem_statement="",
        patch="",
        fail_to_pass=[],
        pass_to_pass=[],
        docker_image="xingyaoww/sweb.eval.x86_64.pydantic_s_pydantic-8583",
        test_cmds="pytest",
        environment_setup_commit="",
    )


def test_default_sweevo_sandbox_name_is_unique():
    from benchmarks.sweevo.sandbox import _default_sweevo_sandbox_name

    instance = _instance()

    first = _default_sweevo_sandbox_name(instance)
    second = _default_sweevo_sandbox_name(instance)

    assert first != second
    assert first.startswith("sweevo-test-pydantic__pydantic")
    assert len(first) <= 63
    assert len(second) <= 63


def test_create_sweevo_test_sandbox_reuses_named_retry(monkeypatch):
    from benchmarks.sweevo import sandbox as sweevo_sandbox

    existing = {
        "id": "sb-existing",
        "name": "retry-sandbox",
        "labels": {"purpose": "sweevo-test"},
    }
    service = SimpleNamespace(
        list_sandboxes=lambda: [existing],
        create_sandbox=lambda **_: pytest.fail("should not create a new sandbox"),
    )
    setup_mock = AsyncMock()
    patch_mock = AsyncMock()

    monkeypatch.setattr(sweevo_sandbox, "_service", lambda: service)
    monkeypatch.setattr(sweevo_sandbox, "setup_sweevo_sandbox", setup_mock)
    monkeypatch.setattr(sweevo_sandbox, "ensure_sweevo_test_patch", patch_mock)

    result = asyncio.run(
        sweevo_sandbox.create_sweevo_test_sandbox(
            _instance(),
            sandbox_name="retry-sandbox",
            register_snapshot=False,
        )
    )

    assert result["sandbox_id"] == "sb-existing"
    assert result["sandbox"] == existing
    assert result["reused_existing"] is True
    setup_mock.assert_awaited_once_with(_instance(), "sb-existing", _REPO_DIR)
    patch_mock.assert_awaited_once_with(_instance(), "sb-existing", _REPO_DIR)


def test_create_sweevo_test_sandbox_truncates_explicit_name_on_create(monkeypatch):
    from benchmarks.sweevo import sandbox as sweevo_sandbox
    from benchmarks.sweevo.models import _truncate_dns_label

    long_name = (
        "retry-sandbox-name-that-is-way-too-long-for-daytona-and-needs-truncation-now"
    )
    expected_name = _truncate_dns_label(long_name)
    created: dict[str, object] = {}

    def create_sandbox(**kwargs):
        created.update(kwargs)
        return {"id": "sb-created"}

    service = SimpleNamespace(
        list_sandboxes=lambda: [],
        create_sandbox=create_sandbox,
        get_sandbox=lambda sandbox_id: {"id": sandbox_id, "name": expected_name},
    )
    setup_mock = AsyncMock()
    patch_mock = AsyncMock()

    monkeypatch.setattr(sweevo_sandbox, "_service", lambda: service)
    monkeypatch.setattr(sweevo_sandbox, "setup_sweevo_sandbox", setup_mock)
    monkeypatch.setattr(sweevo_sandbox, "ensure_sweevo_test_patch", patch_mock)

    instance = _instance()
    result = asyncio.run(
        sweevo_sandbox.create_sweevo_test_sandbox(
            instance,
            sandbox_name=long_name,
            register_snapshot=False,
        )
    )

    assert created["name"] == expected_name
    assert created["image"] == _normalize_sweevo_image_ref(instance.docker_image)
    assert result["sandbox_id"] == "sb-created"
    assert result["sandbox"]["name"] == expected_name
    assert result["reused_existing"] is False
    setup_mock.assert_awaited_once_with(instance, "sb-created", _REPO_DIR)
    patch_mock.assert_awaited_once_with(instance, "sb-created", _REPO_DIR)


def test_create_sweevo_test_sandbox_falls_back_after_pending_build_timeout(monkeypatch):
    from benchmarks.sweevo import sandbox as sweevo_sandbox

    instance = _instance()
    fresh_name = "fresh-sandbox"
    pending = {"id": "sb-pending", "name": fresh_name, "state": "pending_build", "labels": {}}
    started = {
        "id": "sb-started",
        "name": f"sweevo-test-{instance.instance_id}-prev",
        "state": "started",
        "labels": {"project_dir": _REPO_DIR},
    }
    deleted: list[str] = []
    service = SimpleNamespace(
        list_sandboxes=lambda: [pending, started],
        create_sandbox=lambda **_: (_ for _ in ()).throw(RuntimeError("timed out waiting for build")),
        delete_sandbox=lambda sandbox_id: deleted.append(sandbox_id),
    )
    setup_mock = AsyncMock()
    patch_mock = AsyncMock()

    monkeypatch.setattr(sweevo_sandbox, "_service", lambda: service)
    monkeypatch.setattr(sweevo_sandbox, "_default_sweevo_sandbox_name", lambda _instance: fresh_name)
    monkeypatch.setattr(sweevo_sandbox, "setup_sweevo_sandbox", setup_mock)
    monkeypatch.setattr(sweevo_sandbox, "ensure_sweevo_test_patch", patch_mock)

    result = asyncio.run(
        sweevo_sandbox.create_sweevo_test_sandbox(
            instance,
            register_snapshot=False,
        )
    )

    assert deleted == ["sb-pending"]
    assert result["sandbox_id"] == "sb-started"
    assert result["sandbox"] == started
    assert result["reused_existing"] is True
    assert result["fallback_reason"] == "fresh_create_timeout_reused_started_sandbox"
    assert "timed out waiting for build" in result["fresh_create_error"]
    setup_mock.assert_awaited_once_with(instance, "sb-started", _REPO_DIR)
    patch_mock.assert_awaited_once_with(instance, "sb-started", _REPO_DIR)


def test_setup_sweevo_sandbox_preserves_existing_labels(monkeypatch):
    from benchmarks.sweevo import sandbox as sweevo_sandbox

    labels_set: list[dict[str, str]] = []

    class FakeSandbox:
        labels = {"purpose": "sweevo-test", "sweevo_instance": "abc"}

        def set_labels(self, labels: dict[str, str]) -> None:
            labels_set.append(labels)

    exec_mock = AsyncMock(return_value="")
    service = SimpleNamespace(get_sandbox_object=lambda sandbox_id: FakeSandbox())

    monkeypatch.setattr(sweevo_sandbox, "_exec", exec_mock)
    monkeypatch.setattr(sweevo_sandbox, "_service", lambda: service)

    asyncio.run(sweevo_sandbox.setup_sweevo_sandbox(_instance(), "sbx-1"))

    assert labels_set == [
        {
            "purpose": "sweevo-test",
            "sweevo_instance": "abc",
            "project_dir": _REPO_DIR,
        }
    ]


def test_ensure_sweevo_test_patch_uploads_with_content_first_signature(monkeypatch):
    from benchmarks.sweevo import sandbox as sweevo_sandbox

    uploads: list[tuple[bytes, str]] = []

    class FakeFs:
        async def upload_file(self, content: bytes, path: str) -> None:
            uploads.append((content, path))

    fake_sandbox = SimpleNamespace(fs=FakeFs())
    exec_mock = AsyncMock(side_effect=["APPLYABLE", ""])
    instance = _instance()
    instance.test_patch = "diff --git a/foo b/foo\n"

    monkeypatch.setattr(sweevo_sandbox, "_get_sandbox", AsyncMock(return_value=fake_sandbox))
    monkeypatch.setattr(sweevo_sandbox, "_exec", exec_mock)

    asyncio.run(sweevo_sandbox.ensure_sweevo_test_patch(instance, "sbx-1"))

    assert uploads == [(instance.test_patch.encode("utf-8"), "/tmp/sweevo_test.patch")]


def test_ensure_sweevo_test_patch_falls_back_to_chunked_exec(monkeypatch):
    from benchmarks.sweevo import sandbox as sweevo_sandbox

    async def exec_stub(_sandbox_id: str, command: str, *args, **kwargs) -> str:
        if "git apply --check" in command:
            return "APPLYABLE"
        return ""

    exec_mock = AsyncMock(side_effect=exec_stub)
    instance = _instance()
    instance.test_patch = "diff --git a/foo b/foo\n" * 20

    class FakeFs:
        async def upload_file(self, content: bytes, path: str) -> None:
            raise RuntimeError("upload unavailable")

    monkeypatch.setattr(
        sweevo_sandbox,
        "_get_sandbox",
        AsyncMock(return_value=SimpleNamespace(fs=FakeFs())),
    )
    monkeypatch.setattr(sweevo_sandbox, "_exec", exec_mock)

    asyncio.run(sweevo_sandbox.ensure_sweevo_test_patch(instance, "sbx-1"))

    commands = [call.args[1] for call in exec_mock.await_args_list]
    assert commands[0].startswith(": > /tmp/sweevo_test.patch.b64")
    assert any(
        "base64 -d /tmp/sweevo_test.patch.b64 > /tmp/sweevo_test.patch" in command
        for command in commands
    )


def test_ensure_sweevo_test_patch_uses_upload_file_compat(monkeypatch):
    from benchmarks.sweevo import sandbox as sweevo_sandbox

    sandbox = SimpleNamespace(fs=SimpleNamespace())
    upload_mock = AsyncMock()
    exec_mock = AsyncMock(return_value="ALREADY_APPLIED")

    monkeypatch.setattr(sweevo_sandbox, "_get_sandbox", AsyncMock(return_value=sandbox))
    monkeypatch.setattr(sweevo_sandbox, "_upload_file_compat", upload_mock)
    monkeypatch.setattr(sweevo_sandbox, "_exec", exec_mock)

    instance = _instance()
    instance.test_patch = "diff --git a/foo b/foo\n"
    asyncio.run(sweevo_sandbox.ensure_sweevo_test_patch(instance, "sbx-1"))

    upload_mock.assert_awaited_once_with(
        sandbox,
        b"diff --git a/foo b/foo\n",
        "/tmp/sweevo_test.patch",
    )
