"""Unit tests for PrivateNamespaceStrategy payload dispatch."""

from __future__ import annotations

import json
import subprocess
import unittest.mock
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from sandbox.execution.contract import CommandExecRequest
from sandbox.execution.overlay.layout import LayerPathsLayout, MaterializeLayout
from sandbox.execution.strategies.namespace import PrivateNamespaceStrategy


def _make_request(tmp_path: Path) -> CommandExecRequest:
    return CommandExecRequest(
        request_id="req-test",
        workspace_ref=str(tmp_path / "stack"),
        workspace_root="/testbed",
        command=("echo", "hi"),
    )


def _make_namespace_strategy() -> PrivateNamespaceStrategy:
    return PrivateNamespaceStrategy(available=True)


def _run_and_capture_payload(
    spec: MaterializeLayout | LayerPathsLayout,
    tmp_path: Path,
) -> dict:
    """Run strategy with subprocess mocked out; return the written JSON payload."""
    run_dir = tmp_path / "run"
    run_dir.mkdir(parents=True)
    # Create stdout/stderr sink files so the real open() works
    (run_dir / "stdout.bin").touch()
    (run_dir / "stderr.bin").touch()
    strategy = _make_namespace_strategy()
    request = _make_request(tmp_path)

    fake_result = MagicMock()
    fake_result.returncode = 0

    with unittest.mock.patch("subprocess.run", return_value=fake_result):
        strategy.run(spec=spec, request=request, run_dir=run_dir, timings={})

    return json.loads((run_dir / "namespace-request.json").read_text(encoding="utf-8"))


def test_namespace_payload_contains_layer_paths_for_layer_paths_layout(
    tmp_path: Path,
) -> None:
    spec = LayerPathsLayout(
        workspace_root="/testbed",
        layer_paths=(
            str(tmp_path / "layers" / "L1"),
            str(tmp_path / "layers" / "L2"),
        ),
        layer_storage_root=str(tmp_path / "layers"),
        writes=str(tmp_path / "scratch" / "upper"),
        kernel_scratch=str(tmp_path / "scratch" / "work"),
        scratch_root=str(tmp_path / "scratch"),
    )

    payload = _run_and_capture_payload(spec, tmp_path)

    assert "layer_paths" in payload
    assert payload["layer_paths"] == list(spec.layer_paths)
    assert "lowerdir" not in payload


def test_namespace_payload_contains_lowerdir_for_materialize_layout(
    tmp_path: Path,
) -> None:
    spec = MaterializeLayout(
        workspace_root="/testbed",
        base_repo=str(tmp_path / "scratch" / "base"),
        writes=str(tmp_path / "scratch" / "upper"),
        kernel_scratch=str(tmp_path / "scratch" / "work"),
        scratch_root=str(tmp_path / "scratch"),
    )

    payload = _run_and_capture_payload(spec, tmp_path)

    assert "lowerdir" in payload
    assert payload["lowerdir"] == spec.base_repo
    assert "layer_paths" not in payload
