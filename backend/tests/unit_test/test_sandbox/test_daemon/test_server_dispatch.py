"""Tests for the generic sandbox daemon dispatcher."""

from __future__ import annotations

from typing import Any

import pytest

from sandbox.runtime.daemon.rpc import dispatcher as server


@pytest.fixture(autouse=True)
def restore_op_table() -> None:
    saved = dict(server.OP_TABLE)
    server.OP_TABLE.clear()
    try:
        yield
    finally:
        server.OP_TABLE.clear()
        server.OP_TABLE.update(saved)


async def test_empty_op_table_returns_unknown_op() -> None:
    response = await server.dispatch_envelope_async({"op": "occ.missing", "args": {}})

    assert response["success"] is False
    assert response["error"]["kind"] == "unknown_op"
    assert response["error"]["details"] == {"op": "occ.missing"}


async def test_registered_handler_dispatches_through_op_table() -> None:
    calls: list[dict[str, Any]] = []

    def handler(args: dict[str, Any]) -> dict[str, Any]:
        calls.append(args)
        return {"success": True, "value": args["value"]}

    server.register_op("test.echo", handler)

    response = await server.dispatch_envelope_async(
        {"op": "test.echo", "args": {"value": 3}}
    )

    assert response["success"] is True
    assert response["value"] == 3
    timings = response.get("timings", {})
    assert "runtime.boot_to_dispatch_s" in timings
    assert "runtime.dispatch_s" in timings
    assert calls == [{"value": 3}]
