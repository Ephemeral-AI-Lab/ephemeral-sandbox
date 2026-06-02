"""Unit tests for the daemon-managed plugin PPC service bridge."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

from sandbox.ephemeral_workspace.plugin import ppc_service


class _FakePpcStream:
    def __init__(self) -> None:
        self.writes: list[bytes] = []

    def write(self, payload: bytes) -> None:
        self.writes.append(payload)

    def flush(self) -> None:
        return None


def test_ppc_service_context_uses_mounted_workspace_state(
    monkeypatch,
) -> None:
    state = ppc_service._ServiceState()
    asyncio.run(
        state.ack_refresh(
            {
                "manifest_key": "root@7",
                "workspace_root": "/testbed",
            }
        )
    )

    captured: dict[str, Any] = {}

    def handler(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
        captured["args"] = args
        captured["manifest_key"] = ctx.projection.active_manifest_key()
        captured["workspace_root"] = ctx.overlay.workspace_root
        captured["has_acquire_overlay"] = hasattr(ctx.projection, "acquire_overlay")
        return {"success": True, "manifest_key": captured["manifest_key"]}

    monkeypatch.setattr(ppc_service, "_load_handler", lambda _plugin, _op: handler)

    result = asyncio.run(
        ppc_service._dispatch_plugin_op(
            "plugin.demo.run",
            {
                "caller": {"task_id": "task-1"},
                "intent": "read_only",
                "layer_stack_root": "/eos/layer-stack",
            },
            state,
        )
    )

    assert result == {"success": True, "manifest_key": "root@7"}
    assert captured == {
        "args": {
            "caller": {"task_id": "task-1"},
            "intent": "read_only",
            "layer_stack_root": "/eos/layer-stack",
        },
        "manifest_key": "root@7",
        "workspace_root": "/testbed",
        "has_acquire_overlay": False,
    }


def test_ppc_service_publishes_mounted_workspace_changes(
    tmp_path: Path,
) -> None:
    workspace = tmp_path / "testbed"
    module = workspace / "pkg" / "mod.py"
    module.parent.mkdir(parents=True)
    module.write_text("value = 2\n", encoding="utf-8")
    stream = _FakePpcStream()
    state = ppc_service._ServiceState(stream)
    state.layer_stack_root = "/eos/layer-stack"

    async def publish_and_reply() -> dict[str, Any]:
        publish = asyncio.create_task(
            state.publish_mounted_workspace_changes(
                ["pkg/mod.py"],
                workspace_root=workspace.as_posix(),
                parent_message_id="plugin-op-1",
            )
        )
        while not stream.writes:
            await asyncio.sleep(0)
        request = json.loads(stream.writes[0].decode("utf-8"))
        state.resolve_reply(
            json.loads(
                ppc_service._reply_frame(
                    request["invocation_id"],
                    {
                        "success": True,
                        "published_manifest_version": 5,
                        "files": [{"path": "pkg/mod.py", "status": "committed"}],
                    },
                )
            )
        )
        return await publish

    result = asyncio.run(publish_and_reply())

    assert result == {
        "success": True,
        "published_manifest_version": 5,
        "files": [{"path": "pkg/mod.py", "status": "committed"}],
    }
    request = json.loads(stream.writes[0].decode("utf-8"))
    assert request["op"] == "daemon.occ.apply_changeset"
    body = json.loads(request["args"]["body"])
    assert body == {
        "changes": [
            {
                "content_utf8": "value = 2\n",
                "kind": "write",
                "path": "pkg/mod.py",
            }
        ],
        "layer_stack_root": "/eos/layer-stack",
        "parent_message_id": "plugin-op-1",
    }
    assert request["invocation_id"].startswith("plugin-op-1:plugin-occ-apply-")


def test_ppc_service_dispatches_plugin_requests_concurrently(
    monkeypatch,
) -> None:
    stream = _FakePpcStream()
    state = ppc_service._ServiceState(stream)
    first_started = asyncio.Event()
    second_started = asyncio.Event()
    release = asyncio.Event()

    async def handler(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
        assert ctx.overlay.workspace_root == "/testbed"
        if args["request"] == "a":
            first_started.set()
            await second_started.wait()
        else:
            second_started.set()
        await release.wait()
        return {"success": True, "request": args["request"]}

    load_calls: list[tuple[str, str]] = []

    def load_handler(plugin_name: str, op_name: str) -> Any:
        load_calls.append((plugin_name, op_name))
        return handler

    monkeypatch.setattr(ppc_service, "_load_handler", load_handler)

    def request(message_id: str, value: str) -> dict[str, Any]:
        return json.loads(
            ppc_service._request_frame(
                message_id,
                "plugin.demo.run",
                {
                    "request": value,
                    "intent": "read_only",
                    "workspace_root": "/testbed",
                },
            ).decode("utf-8")
        )

    async def run() -> None:
        first = asyncio.create_task(
            ppc_service._handle_request_message(request("plugin-op-a", "a"), state)
        )
        await asyncio.wait_for(first_started.wait(), timeout=1)
        second = asyncio.create_task(
            ppc_service._handle_request_message(request("plugin-op-b", "b"), state)
        )
        await asyncio.wait_for(second_started.wait(), timeout=1)
        release.set()
        await asyncio.gather(first, second)

    asyncio.run(run())

    replies = [json.loads(frame.decode("utf-8")) for frame in stream.writes]
    assert {reply["invocation_id"] for reply in replies} == {
        "plugin-op-a",
        "plugin-op-b",
    }
    bodies = {
        reply["invocation_id"]: json.loads(reply["args"]["body"]) for reply in replies
    }
    assert bodies == {
        "plugin-op-a": {"request": "a", "success": True},
        "plugin-op-b": {"request": "b", "success": True},
    }
    assert load_calls == [("demo", "run")]
