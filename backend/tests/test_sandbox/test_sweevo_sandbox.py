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
    setup_mock.assert_not_awaited()
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
