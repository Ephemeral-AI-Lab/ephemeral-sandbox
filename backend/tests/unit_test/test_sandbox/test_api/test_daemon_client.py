"""Tests for host-owned sandbox daemon client helpers."""

from __future__ import annotations

from typing import Any

import pytest

from sandbox.host import daemon_client as daemon_client_mod


class _Adapter:
    async def exec(self, *_args: object, **_kwargs: object) -> Any:
        raise AssertionError("daemon dispatch is mocked in this test")


def test_versioned_payload_attaches_daemon_protocol_version() -> None:
    assert daemon_client_mod.versioned_payload({"path": "a.py"}) == {
        daemon_client_mod.DAEMON_PROTOCOL_FIELD: daemon_client_mod.DAEMON_PROTOCOL_VERSION,
        "path": "a.py",
    }


@pytest.mark.asyncio
async def test_call_daemon_api_dispatches_without_bundle_probe(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    dispatch_calls: list[tuple[str, str, dict[str, object], int]] = []

    async def fake_call_daemon(
        *, exec_fn, sandbox_id, op, args, timeout, tcp_endpoint
    ):
        del exec_fn, tcp_endpoint
        dispatch_calls.append((sandbox_id, op, args, timeout))
        return {"success": True, "timings": {}}

    monkeypatch.setattr(daemon_client_mod, "get_adapter", lambda _sandbox_id: _Adapter())
    monkeypatch.setattr(daemon_client_mod, "_call_daemon", fake_call_daemon)

    await daemon_client_mod.call_daemon_api(
        "sb-1",
        "api.first",
        {"path": "a.txt"},
        timeout=10,
        layer_stack_root="/runtime/layers",
    )
    await daemon_client_mod.call_daemon_api(
        "sb-1",
        "api.second",
        {"path": "b.txt"},
        timeout=20,
        layer_stack_root="/runtime/layers",
    )

    assert dispatch_calls == [
        (
            "sb-1",
            "api.first",
            {"layer_stack_root": "/runtime/layers", "path": "a.txt"},
            10,
        ),
        (
            "sb-1",
            "api.second",
            {"layer_stack_root": "/runtime/layers", "path": "b.txt"},
            20,
        ),
    ]


@pytest.mark.asyncio
async def test_resolve_daemon_tcp_endpoint_caches_per_sandbox(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Second resolution for the same sandbox must not re-call the resolver."""
    call_count = 0

    class _CachingAdapter:
        def get_daemon_tcp_endpoint(self, sandbox_id: str) -> dict[str, Any]:
            nonlocal call_count
            call_count += 1
            return {"host": "127.0.0.1", "port": 9000 + call_count, "internal_port": 4000}

    monkeypatch.setattr(daemon_client_mod, "_tcp_endpoint_cache", {})
    monkeypatch.setattr(daemon_client_mod, "_tcp_endpoint_cache_locks", {})

    adapter = _CachingAdapter()
    a = await daemon_client_mod._resolve_daemon_tcp_endpoint(adapter, "sb-cache")
    b = await daemon_client_mod._resolve_daemon_tcp_endpoint(adapter, "sb-cache")
    assert a is not None and b is not None
    assert a == b
    assert a.port == 9001
    assert call_count == 1, "cached endpoint must skip resolver on second call"

    daemon_client_mod.invalidate_daemon_tcp_endpoint("sb-cache")
    c = await daemon_client_mod._resolve_daemon_tcp_endpoint(adapter, "sb-cache")
    assert c is not None
    assert call_count == 2, "invalidation must force resolver re-call"
    assert c.port == 9002, "invalidated entry must be replaced by fresh resolution"
