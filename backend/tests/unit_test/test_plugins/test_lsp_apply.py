"""Unit tests for LSP WorkspaceEdit application."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from plugins.catalog.lsp.runtime import apply as apply_mod
from plugins.catalog.lsp.runtime.apply import apply_workspace_edit


class _Overlay:
    def __init__(self, workspace_root: str) -> None:
        self.workspace_root = workspace_root
        self.published_paths: tuple[str, ...] = ()
        self.ensure_reasons: list[str] = []

    async def ensure_current(self, *, reason: str = "ensure_current") -> str:
        self.ensure_reasons.append(reason)
        return "hash@1"

    async def publish_workspace_paths(
        self,
        *,
        paths: tuple[str, ...],
        actor_id: str = "",
        description: str = "plugin workspace edit",
    ) -> object:
        del actor_id, description
        self.published_paths = paths
        return SimpleNamespace(
            success=True,
            published_manifest_version=2,
            files=(),
        )


class _OperationOverlay(_Overlay):
    def __init__(self, workspace_root: str, *, scratch_root: Path) -> None:
        super().__init__(workspace_root)
        self.scratch_root = scratch_root
        self.handle: SimpleNamespace | None = None
        self.published_upperdir: Path | None = None

    def acquire_operation_overlay(
        self,
        *,
        request_id: str,
        workspace_root: str | None = None,
        materialize: bool = False,
    ) -> SimpleNamespace:
        del request_id, workspace_root, materialize
        run_dir = self.scratch_root / "run"
        upperdir = run_dir / "upper"
        workdir = run_dir / "work"
        upperdir.mkdir(parents=True)
        workdir.mkdir()
        self.handle = SimpleNamespace(
            manifest_key="hash@1",
            manifest=SimpleNamespace(version=1),
            layer_paths=("/layers/L1",),
            run_dir=run_dir.as_posix(),
            upperdir=upperdir.as_posix(),
            workdir=workdir.as_posix(),
            release=lambda: None,
        )
        return self.handle

    async def publish_cycle(
        self,
        *,
        request: Any,
        upperdir: str,
        snapshot: Any,
        run_maintenance: bool = True,
    ) -> object:
        del request, snapshot, run_maintenance
        self.published_upperdir = Path(upperdir)
        return SimpleNamespace(
            changeset=SimpleNamespace(
                success=True,
                published_manifest_version=3,
                files=(),
            ),
            timings={"lsp.apply.overlay_s": 0.01},
        )


@dataclass(frozen=True)
class _Caller:
    agent_id: str = "agent"


@dataclass(frozen=True)
class _Ctx:
    overlay: _Overlay
    layer_stack_root: str = "/layer-stack"
    caller: _Caller = _Caller()
    metadata: dict[str, str] | None = None


@pytest.mark.asyncio
async def test_apply_workspace_edit_writes_text_edits_and_publishes_path(
    tmp_path: Path,
) -> None:
    workspace = tmp_path / "testbed"
    module = workspace / "pkg" / "mod.py"
    module.parent.mkdir(parents=True)
    module.write_text("value = 1\nprint(value)\n", encoding="utf-8")
    overlay = _Overlay(workspace.as_posix())
    uri = module.as_uri()

    result = await apply_workspace_edit(
        {
            "changes": {
                uri: [
                    {
                        "range": {
                            "start": {"line": 0, "character": 8},
                            "end": {"line": 0, "character": 9},
                        },
                        "newText": "2",
                    }
                ]
            }
        },
        _Ctx(overlay=overlay),
    )

    assert module.read_text(encoding="utf-8") == "value = 2\nprint(value)\n"
    assert overlay.ensure_reasons == ["lsp:apply_workspace_edit:enter"]
    assert overlay.published_paths == ("pkg/mod.py",)
    assert result["success"] is True
    assert result["manifest_version"] == 2


@pytest.mark.asyncio
async def test_apply_workspace_edit_rejects_paths_outside_workspace(
    tmp_path: Path,
) -> None:
    workspace = tmp_path / "testbed"
    workspace.mkdir()
    outside = tmp_path / "outside.py"
    overlay = _Overlay(workspace.as_posix())

    with pytest.raises(ValueError, match="outside workspace"):
        await apply_workspace_edit(
            {
                "changes": {
                    outside.as_uri(): [
                        {
                            "range": {
                                "start": {"line": 0, "character": 0},
                                "end": {"line": 0, "character": 0},
                            },
                            "newText": "x = 1\n",
                        }
                    ]
                }
            },
            _Ctx(overlay=overlay),
        )

    assert not outside.exists()
    assert overlay.published_paths == ()


@pytest.mark.asyncio
async def test_apply_workspace_edit_handles_file_operations(
    tmp_path: Path,
) -> None:
    workspace = tmp_path / "testbed"
    old_path = workspace / "pkg" / "old.py"
    old_path.parent.mkdir(parents=True)
    old_path.write_text("x = 1\n", encoding="utf-8")
    overlay = _Overlay(workspace.as_posix())

    result = await apply_workspace_edit(
        {
            "documentChanges": [
                {"kind": "rename", "oldUri": old_path.as_uri(), "newUri": (workspace / "pkg" / "new.py").as_uri()},
                {"kind": "create", "uri": (workspace / "pkg" / "created.py").as_uri()},
                {"kind": "delete", "uri": (workspace / "pkg" / "created.py").as_uri()},
            ]
        },
        _Ctx(overlay=overlay),
    )

    assert not old_path.exists()
    assert (workspace / "pkg" / "new.py").read_text(encoding="utf-8") == "x = 1\n"
    assert not (workspace / "pkg" / "created.py").exists()
    assert overlay.published_paths == (
        "pkg/created.py",
        "pkg/new.py",
        "pkg/old.py",
    )
    assert result["changed_paths"] == ["pkg/created.py", "pkg/new.py", "pkg/old.py"]


@pytest.mark.asyncio
async def test_apply_workspace_edit_uses_operation_overlay_upperdir(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace = tmp_path / "testbed"
    workspace.mkdir()
    overlay = _OperationOverlay(workspace.as_posix(), scratch_root=tmp_path / "scratch")

    async def fake_run_apply_child(
        edit: dict[str, Any],
        *,
        workspace_root: str,
        handle: Any,
    ) -> list[str]:
        del edit
        assert workspace_root == "/testbed"
        output = Path(handle.upperdir) / "pkg" / "mod.py"
        output.parent.mkdir(parents=True)
        output.write_text("value = 2\n", encoding="utf-8")
        return ["pkg/mod.py"]

    monkeypatch.setattr(apply_mod, "_overlay_namespace_available", lambda: True)
    monkeypatch.setattr(apply_mod, "_run_apply_child", fake_run_apply_child)

    result = await apply_workspace_edit(
        {"changes": {}},
        _Ctx(overlay=overlay),
        workspace_root="/testbed",
        expected_manifest_key="hash@1",
    )

    assert overlay.ensure_reasons == []
    assert overlay.published_upperdir == Path(overlay.handle.upperdir)
    assert (overlay.published_upperdir / "pkg" / "mod.py").read_text(
        encoding="utf-8"
    ) == "value = 2\n"
    assert not (workspace / "pkg" / "mod.py").exists()
    assert result["success"] is True
    assert result["changed_paths"] == ["pkg/mod.py"]
    assert result["manifest_version"] == 3
