"""Tests for the host-side typed OCC client."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.occ.changeset.intent import PreparedChangeset
from sandbox.occ.changeset.types import WriteChange
from sandbox.occ.client import (
    OCCClient,
    OCCClientError,
    dispose_occ_service,
    register_occ_service,
)


class _Service:
    def __init__(self) -> None:
        self.calls: list[tuple[tuple[object, ...], object, object]] = []

    async def apply_changeset(self, changes, *, snapshot=None, options=None):
        self.calls.append((tuple(changes), snapshot, options))
        return PreparedChangeset(snapshot=snapshot, path_groups=(), atomic=False)


@pytest.mark.asyncio
async def test_occ_client_resolves_registered_service_by_sandbox_id() -> None:
    service = _Service()
    change = WriteChange(path="/workspace/a.txt", final_content=b"a\n")
    register_occ_service("sb-occ", service)
    try:
        result = await OCCClient("sb-occ").apply_changeset([change])
    finally:
        dispose_occ_service("sb-occ")

    assert isinstance(result, PreparedChangeset)
    assert service.calls[0][0] == (change,)


def test_occ_client_requires_service_binding() -> None:
    with pytest.raises(OCCClientError) as exc:
        OCCClient("missing-sandbox")

    assert exc.value.kind == "MissingOccService"


def test_occ_client_does_not_import_handlers_or_overlay() -> None:
    import sandbox.occ.client as client_module

    source = Path(client_module.__file__).read_text(encoding="utf-8")

    assert "sandbox.occ.handlers" not in source
    assert "sandbox.occ.wire" not in source
    assert "sandbox.control.daemon.command" not in source
    assert "sandbox.overlay" not in source


@pytest.mark.asyncio
async def test_occ_client_can_call_phase03_service_directly() -> None:
    service = _Service()
    change = WriteChange(path="a.txt", source="api_write", final_content=b"x")

    result = await OCCClient(service=service).apply_changeset(
        [change],
        agent_id="agent-a",
        description="write a",
        snapshot="manifest",
    )

    assert isinstance(result, PreparedChangeset)
    assert service.calls[0][0] == (change,)
    assert service.calls[0][1] == "manifest"
    assert service.calls[0][2].caller_id == "agent-a"
