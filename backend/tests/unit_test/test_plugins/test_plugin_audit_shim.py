"""Phase 2.5 slice 5 — generic plugin shim emits ``plugin.*`` events.

Asserts the shim installed by ``plugins.core.loader._install_plugin_audit_shim``:
- Fires ``plugin.tool_invoked`` + ``plugin.tool_completed`` on success.
- Fires ``plugin.error`` with ``error_kind`` on exception.
- Never embeds LSP-named keys in the payload (Principle 2 — generic by
  construction).
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

import pytest
from pydantic import BaseModel

from plugins.core.loader import _install_plugin_audit_shim
from plugins.core.manifest import PluginManifest, ToolEntry
from sandbox._shared.models import Intent
from sandbox.daemon.audit_buffer import get_audit_buffer
from tools._framework.core.base import BaseTool, ToolExecutionContextService
from tools._framework.core.results import ToolResult


_AUDIT_CURSOR = {"seq": -1}


class _Args(BaseModel):
    value: int = 0


class _Out(BaseModel):
    echoed: int = 0


class _FakeTool(BaseTool):
    name = "indexer.echo"
    description = "fake indexer tool"
    input_model = _Args
    output_model = _Out
    intent = Intent.READ_ONLY

    def __init__(self, *, raise_with: Exception | None = None) -> None:
        super().__init__()
        self._raise_with = raise_with

    async def execute(
        self,
        arguments: _Args,
        context: ToolExecutionContextService,
    ) -> ToolResult:
        if self._raise_with is not None:
            raise self._raise_with
        return ToolResult(output=str(arguments.value))


def _fake_manifest(name: str, *, kind: str | None = None) -> PluginManifest:
    return PluginManifest(
        name=name,
        description="fake",
        tools=(),
        setup=None,
        runtime=None,
        source_dir=Path("/tmp"),
        body="",
        kind=kind,
    )


def _drain_plugin_events() -> list[dict[str, Any]]:
    buf = get_audit_buffer()
    snap = buf.pull(after_seq=_AUDIT_CURSOR["seq"], limit=10_000)
    events = snap.get("events", [])
    if events:
        _AUDIT_CURSOR["seq"] = int(events[-1]["seq"])
    return [evt for evt in events if str(evt.get("type", "")).startswith("plugin.")]


@pytest.fixture(autouse=True)
def _reset_audit_cursor() -> None:
    buf = get_audit_buffer()
    cursor = -1
    while True:
        snap = buf.pull(after_seq=cursor, limit=10_000)
        events = snap.get("events", [])
        if not events:
            break
        cursor = int(events[-1]["seq"])
    _AUDIT_CURSOR["seq"] = cursor
    yield


def test_plugin_events_are_kind_generic_no_lsp_keys() -> None:
    tool = _FakeTool()
    manifest = _fake_manifest("indexer_demo")
    entry = ToolEntry(name="indexer.echo", module=Path("/tmp/x.py"))
    _install_plugin_audit_shim(tool, manifest=manifest, entry=entry)

    asyncio.run(tool.execute(_Args(value=42), None))  # type: ignore[arg-type]

    events = _drain_plugin_events()
    types = [e["type"] for e in events]
    assert "plugin.tool_invoked" in types
    assert "plugin.tool_completed" in types

    raw = json.dumps(events)
    # Generic-by-construction: NO LSP / vendor identifier as a JSON key.
    for forbidden_key in ('"lsp"', '"pyright"', '"language_server"'):
        # ``"language_server"`` is allowed as a VALUE for plugin_kind, but
        # never as a key. We assert it only appears in value position
        # (post-colon) — never as a bare key (colon-followed).
        assert '"language_server":' not in raw, (
            f"forbidden key {forbidden_key} present in plugin event payload"
        )
    assert '"lsp":' not in raw
    assert '"pyright":' not in raw


def test_plugin_error_carries_error_kind() -> None:
    tool = _FakeTool(raise_with=ValueError("nope"))
    manifest = _fake_manifest("indexer_demo")
    entry = ToolEntry(name="indexer.echo", module=Path("/tmp/x.py"))
    _install_plugin_audit_shim(tool, manifest=manifest, entry=entry)

    with pytest.raises(ValueError):
        asyncio.run(tool.execute(_Args(value=1), None))  # type: ignore[arg-type]

    events = _drain_plugin_events()
    error = next(e for e in events if e["type"] == "plugin.error")
    section = error["payload"]["plugin"]
    assert section["error_kind"] == "ValueError"
    assert section["plugin_kind"] == "custom"
    assert section["plugin_id"] == "indexer_demo"


def test_plugin_shim_stamps_manifest_kind_when_present() -> None:
    """Closer D: when ``manifest.kind`` is set, the shim uses it instead of ``"custom"``."""
    tool = _FakeTool()
    manifest = _fake_manifest("lsp", kind="language_server")
    entry = ToolEntry(name="indexer.echo", module=Path("/tmp/x.py"))
    _install_plugin_audit_shim(tool, manifest=manifest, entry=entry)
    asyncio.run(tool.execute(_Args(value=1), None))  # type: ignore[arg-type]

    events = _drain_plugin_events()
    invoked = next(e for e in events if e["type"] == "plugin.tool_invoked")
    completed = next(e for e in events if e["type"] == "plugin.tool_completed")
    assert invoked["payload"]["plugin"]["plugin_kind"] == "language_server"
    assert completed["payload"]["plugin"]["plugin_kind"] == "language_server"
