"""Crash-safety tests: every "atomic" write path must fsync data + parent dir.

These tests monkeypatch ``os.fsync`` with a recorder and assert that each
write path under test calls ``os.fsync`` at least the expected number of
times. We do not validate which specific fds are fsynced (too brittle on
different platforms) -- only that the discipline is applied.
"""

from __future__ import annotations

import hashlib
import os
from pathlib import Path

import pytest

import sandbox.layer_stack.layer.publisher as publisher_mod
import sandbox.layer_stack.manifest.store as manifest_store_mod
import sandbox.layer_stack.workspace.base as workspace_base_mod
import sandbox.layer_stack.workspace.binding as binding_mod
from sandbox.layer_stack import WriteLayerChange, LayerStackManager
from sandbox.layer_stack.manifest import (
    LayerRef,
    Manifest,
    write_manifest_atomic,
)
from sandbox.layer_stack.workspace.base import build_workspace_base
from sandbox.layer_stack.workspace.binding import (
    WorkspaceBinding,
    write_workspace_binding_atomic,
)


class _FsyncRecorder:
    """Wraps os.fsync; records each fd and forwards the call."""

    def __init__(self) -> None:
        self.fds: list[int] = []

    def install(self, monkeypatch: pytest.MonkeyPatch, module: object) -> None:
        original = os.fsync

        def recorded(fd: int) -> None:
            self.fds.append(fd)
            original(fd)

        monkeypatch.setattr(module.os, "fsync", recorded)


def test_write_manifest_atomic_fsyncs_file_and_parent_dir(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    recorder = _FsyncRecorder()
    recorder.install(monkeypatch, manifest_store_mod)

    manifest_file = tmp_path / "manifest.json"
    write_manifest_atomic(
        manifest_file,
        Manifest(version=1, layers=(LayerRef(layer_id="L1", path="layers/L1"),)),
    )

    # Expect at least: tmp-file fd fsync + parent-dir fd fsync.
    assert len(recorder.fds) >= 2, (
        f"write_manifest_atomic must fsync tmp file and parent dir; "
        f"recorded {recorder.fds}"
    )
    # Sanity: file is durable on return.
    assert manifest_file.exists()


def test_workspace_binding_write_atomic_fsyncs_file_and_parent_dir(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    recorder = _FsyncRecorder()
    recorder.install(monkeypatch, binding_mod)

    workspace = tmp_path / "workspace"
    workspace.mkdir()
    stack = tmp_path / "stack"
    stack.mkdir()
    binding = WorkspaceBinding(
        workspace_root=workspace.as_posix(),
        layer_stack_root=stack.as_posix(),
        active_manifest_version=1,
        active_root_hash="a" * 64,
        base_manifest_version=1,
        base_root_hash="a" * 64,
    )

    write_workspace_binding_atomic(binding)

    assert len(recorder.fds) >= 2, (
        f"write_workspace_binding_atomic must fsync tmp file and parent dir; "
        f"recorded {recorder.fds}"
    )


def test_publish_layer_fsyncs_staged_files_and_parent_dir(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    recorder = _FsyncRecorder()
    recorder.install(monkeypatch, publisher_mod)

    storage_root = tmp_path / "stack"
    manager = LayerStackManager(storage_root)
    source = tmp_path / "source.txt"
    source.write_bytes(b"hello\n")

    manager.publish_changes(
        [
            WriteLayerChange(
                path="pkg/a.txt",
                content_hash=hashlib.sha256(b"hello\n").hexdigest(),
                source_path=str(source),
            )
        ]
    )

    # Expect at least: one file fsync + staging-dir fsync + layers-parent
    # fsync + digest-file fsync + metadata-dir fsync.
    assert len(recorder.fds) >= 3, (
        f"publish_layer must fsync staged files and parent dirs; "
        f"recorded {recorder.fds}"
    )


def test_workspace_base_writer_fsyncs_files_parent_and_digest(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    recorder = _FsyncRecorder()
    recorder.install(monkeypatch, workspace_base_mod)

    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "a.txt").write_text("base\n", encoding="utf-8")
    stack = tmp_path / "stack"

    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    # File fsync + staging-dir fsync + layers-parent fsync at minimum.
    assert len(recorder.fds) >= 3, (
        f"_write_base_layer must fsync staged files, staging dir, parent dir; "
        f"recorded {recorder.fds}"
    )


def test_workspace_base_writes_digest_sidecar(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "a.txt").write_text("base\n", encoding="utf-8")
    stack = tmp_path / "stack"

    binding = build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    digest_path = stack / ".layer-metadata" / "B000001-base.digest"
    assert digest_path.exists(), (
        f"_write_base_layer must persist a digest sidecar at {digest_path}"
    )
    digest_text = digest_path.read_text(encoding="utf-8").strip()
    # Must be a sha256 hex string and must match the base_root_hash on the binding.
    assert len(digest_text) == 64
    assert digest_text == binding.base_root_hash
