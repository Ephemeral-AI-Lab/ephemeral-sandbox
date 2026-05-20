"""Unit tests for namespace_child payload parsing."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.execution.strategies.namespace_child import _payload_request


def _base_payload(tmp_path: Path, **overrides: object) -> dict:
    base = {
        "workspace_root": "/testbed",
        "layer_paths": ["/storage/L1", "/storage/L2"],
        "upperdir": str(tmp_path / "upper"),
        "workdir": str(tmp_path / "work"),
        "stdout_ref": str(tmp_path / "stdout.bin"),
        "stderr_ref": str(tmp_path / "stderr.bin"),
        "timings_ref": str(tmp_path / "timings.json"),
        "control_ref": str(tmp_path / "control.json"),
    }
    base.update(overrides)
    return base


def test_payload_request_parses_layer_paths(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path)
    request = _payload_request(payload)

    assert request.layer_paths == (Path("/storage/L1"), Path("/storage/L2"))


def test_payload_request_workspace_root_is_path(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path)
    request = _payload_request(payload)

    assert request.workspace_root == Path("/testbed")


def test_payload_request_optional_control_ref_none(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path)
    payload.pop("control_ref")
    request = _payload_request(payload)

    assert request.control_ref is None


def test_payload_request_raises_on_missing_layer_paths(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path)
    payload.pop("layer_paths")

    with pytest.raises(KeyError):
        _payload_request(payload)


def test_payload_request_raises_on_empty_layer_paths(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path, layer_paths=[])

    with pytest.raises(ValueError, match="non-empty list"):
        _payload_request(payload)


def test_payload_request_raises_on_non_list_layer_paths(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path, layer_paths="/storage/L1")

    with pytest.raises(ValueError, match="non-empty list"):
        _payload_request(payload)


def test_payload_request_no_lowerdir_attribute(tmp_path: Path) -> None:
    payload = _base_payload(tmp_path)
    request = _payload_request(payload)

    assert not hasattr(request, "lowerdir")
