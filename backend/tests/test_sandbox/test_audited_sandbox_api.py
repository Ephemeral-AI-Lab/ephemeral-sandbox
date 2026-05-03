"""Contract tests for :class:`sandbox.api.audited_sandbox_api.AuditedSandboxApi`.

Each test wires a fake :class:`SandboxTransport` and a fake svc so we can
verify that ``AuditedSandboxApi`` dispatches the right operation with the
right shape. Audit semantics themselves are covered by lifecycle/commit
tests — these tests stop at the dispatch boundary.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass, field
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest

from sandbox.api import audit
from sandbox.api.audited_sandbox_api import AuditedSandboxApi
from sandbox.api.models import (
    EditFileRequest,
    ReadFileRequest,
    RequestActor,
    SearchReplaceEdit,
    ShellRequest,
    WriteFileRequest,
)


@dataclass
class _FakeTransport:
    """Fake SandboxTransport recording calls for assertion."""

    name: str = "fake"
    read_bytes_mock: AsyncMock = field(default_factory=AsyncMock)

    async def exec(self, *args: Any, **kwargs: Any) -> Any:
        raise AssertionError("AuditedSandboxApi must not call transport.exec")

    async def read_bytes(self, sandbox_id: str, path: str) -> bytes:
        return await self.read_bytes_mock(sandbox_id, path)

    async def read_bytes_batch(
        self,
        sandbox_id: str,
        paths: Sequence[str],
    ) -> dict[str, bytes | None]:
        raise AssertionError(
            "AuditedSandboxApi does not call transport.read_bytes_batch"
        )

    async def write_bytes(self, sandbox_id: str, path: str, content: bytes) -> None:
        raise AssertionError("AuditedSandboxApi writes go through audit, not transport")

    async def apply_diff_batch_checked(self, *args: Any, **kwargs: Any) -> Any:
        raise AssertionError("AuditedSandboxApi writes go through audit, not transport")


@pytest.fixture
def actor() -> RequestActor:
    return RequestActor(agent_id="alice", run_id="r-1", task_id="t-1")


@pytest.fixture
def transport() -> _FakeTransport:
    return _FakeTransport()


@pytest.fixture
def fake_svc() -> SimpleNamespace:
    return SimpleNamespace(name="fake-svc")


@pytest.fixture
def fake_sandbox() -> SimpleNamespace:
    return SimpleNamespace(id="sb-1")


@pytest.fixture
def api(
    transport: _FakeTransport,
    fake_svc: SimpleNamespace,
    fake_sandbox: SimpleNamespace,
) -> AuditedSandboxApi:
    return AuditedSandboxApi(
        transport=transport,
        svc=fake_svc,
        sandbox=fake_sandbox,
    )


# -- read / search ----------------------------------------------------------


async def test_read_file_decodes_transport_payload(
    api: AuditedSandboxApi,
    transport: _FakeTransport,
    actor: RequestActor,
) -> None:
    transport.read_bytes_mock.return_value = "héllo".encode("utf-8")

    result = await api.read_file("sb-1", ReadFileRequest(path="/x", actor=actor))

    assert result.exists is True
    assert result.content == "héllo"
    transport.read_bytes_mock.assert_awaited_once_with("sb-1", "/x")


async def test_read_file_returns_missing_on_file_not_found(
    api: AuditedSandboxApi,
    transport: _FakeTransport,
    actor: RequestActor,
) -> None:
    transport.read_bytes_mock.side_effect = FileNotFoundError("/missing")

    result = await api.read_file(
        "sb-1", ReadFileRequest(path="/missing", actor=actor),
    )

    assert result.exists is False
    assert result.content == ""


# -- mutation ----------------------------------------------------------------


async def test_write_file_dispatches_through_audit(
    api: AuditedSandboxApi,
    fake_svc: SimpleNamespace,
    fake_sandbox: SimpleNamespace,
    actor: RequestActor,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, Any] = {}

    async def fake_write_request(svc: Any, *, request: WriteFileRequest, sandbox: Any) -> Any:
        captured["svc"] = svc
        captured["request"] = request
        captured["sandbox"] = sandbox
        return SimpleNamespace(
            success=True,
            changed_paths=("/x",),
            conflict_reason=None,
            raw=None,
        )

    monkeypatch.setattr(audit, "submit_write_request", fake_write_request)

    request = WriteFileRequest(path="/x", content="abc", actor=actor)
    result = await api.write_file("sb-1", request)

    assert result.success is True
    assert result.changed_paths == ("/x",)
    assert captured["svc"] is fake_svc
    assert captured["sandbox"] is fake_sandbox
    assert captured["request"] is request


async def test_write_file_surfaces_conflict_reason(
    api: AuditedSandboxApi,
    actor: RequestActor,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def fake_write_request(svc: Any, *, request: WriteFileRequest, sandbox: Any) -> Any:
        return SimpleNamespace(
            success=False,
            changed_paths=(),
            conflict_reason="aborted_version",
            raw=None,
        )

    monkeypatch.setattr(audit, "submit_write_request", fake_write_request)

    result = await api.write_file(
        "sb-1", WriteFileRequest(path="/x", content="", actor=actor),
    )

    assert result.success is False
    assert result.conflict_reason == "aborted_version"


async def test_edit_file_dispatches_through_audit(
    api: AuditedSandboxApi,
    actor: RequestActor,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, Any] = {}

    async def fake_edit_request(svc: Any, *, request: EditFileRequest, sandbox: Any) -> Any:
        captured["request"] = request
        return SimpleNamespace(
            success=True,
            changed_paths=("/x",),
            conflict_reason=None,
            raw=None,
        )

    monkeypatch.setattr(audit, "submit_edit_request", fake_edit_request)

    request = EditFileRequest(
        path="/x",
        edits=(SearchReplaceEdit(old_text="a", new_text="b"),),
        actor=actor,
    )
    result = await api.edit_file("sb-1", request)

    assert result.success is True
    assert result.applied_edits == 1
    assert captured["request"] is request


# -- shell -------------------------------------------------------------------


async def test_shell_unpacks_audit_response(
    api: AuditedSandboxApi,
    actor: RequestActor,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    raw = SimpleNamespace(
        result="hello",
        exit_code=0,
        warnings=["w1"],
    )

    async def fake_shell_request(
        svc: Any, *, sandbox: Any, request: ShellRequest, on_progress_line: Any = None,
    ) -> Any:
        return SimpleNamespace(
            success=True,
            changed_paths=("/foo",),
            conflict_reason=None,
            raw=raw,
        )

    monkeypatch.setattr(audit, "submit_shell_request", fake_shell_request)

    result = await api.shell(
        "sb-1", ShellRequest(command="echo hi", actor=actor, timeout=10),
    )

    assert result.exit_code == 0
    assert result.stdout == "hello"
    assert result.success is True
    assert result.changed_paths == ("/foo",)
    assert result.conflict_reason is None
    assert result.warnings == ("w1",)


async def test_shell_surfaces_audit_conflict(
    api: AuditedSandboxApi,
    actor: RequestActor,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def fake_shell_request(
        svc: Any, *, sandbox: Any, request: ShellRequest, on_progress_line: Any = None,
    ) -> Any:
        return SimpleNamespace(
            success=False,
            changed_paths=(),
            conflict_reason="aborted_version",
            raw=SimpleNamespace(result="", exit_code=0),
        )

    monkeypatch.setattr(audit, "submit_shell_request", fake_shell_request)

    result = await api.shell(
        "sb-1", ShellRequest(command="touch x", actor=actor),
    )

    assert result.success is False
    assert result.conflict_reason == "aborted_version"


def test_audited_sandbox_api_satisfies_protocol_method_set() -> None:
    """AuditedSandboxApi declares every method named on SandboxApi."""
    from sandbox.api.sandbox_api import SandboxApi

    expected = {
        name
        for name in dir(SandboxApi)
        if not name.startswith("_") and callable(getattr(SandboxApi, name))
    }
    declared = {
        name
        for name in dir(AuditedSandboxApi)
        if not name.startswith("_") and callable(getattr(AuditedSandboxApi, name))
    }
    missing = expected - declared
    assert not missing, f"AuditedSandboxApi missing methods: {sorted(missing)}"
