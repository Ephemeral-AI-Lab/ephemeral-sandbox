"""Runtime readiness probe tests."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox.daemon.handler import health
from sandbox.daemon.service import occ_backend, workspace_server


@pytest.fixture(autouse=True)
def _clear_runtime_caches() -> None:
    occ_backend.clear_backend_cache()
    workspace_server.clear_layer_stack_server_caches_for_tests()
    try:
        yield
    finally:
        occ_backend.clear_backend_cache()
        workspace_server.clear_layer_stack_server_caches_for_tests()


def _probe(response: dict[str, object], name: str) -> dict[str, object]:
    probes = response["probes"]
    assert isinstance(probes, list)
    for probe in probes:
        if isinstance(probe, dict) and probe.get("name") == name:
            return probe
    raise AssertionError(f"missing probe: {name}")


def test_daemon_ready_bound_workspace_returns_ready(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "file.txt").write_text("hello\n", encoding="utf-8")
    stack = tmp_path / "layer-stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    response = health.runtime_ready({"layer_stack_root": stack.as_posix()})

    assert response["success"] is True
    assert response["ready"] is True
    control = _probe(response, "control_plane")
    assert control["status"] == "ok"
    details = control["details"]
    assert isinstance(details, dict)
    assert details["workspace_root"] == workspace.as_posix()
    assert details["manifest_version"] == 1
    assert details["manifest_depth"] == 1


def test_daemon_ready_unbound_root_fails_closed(
    tmp_path: Path,
) -> None:
    stack = tmp_path / "layer-stack"

    response = health.runtime_ready({"layer_stack_root": stack.as_posix()})

    assert response["success"] is True
    assert response["ready"] is False
    control = _probe(response, "control_plane")
    assert control["status"] == "down"
    details = control["details"]
    assert isinstance(details, dict)
    assert details["error_type"] == "WorkspaceBindingError"
    assert "workspace binding is missing" in str(details["error"])


def test_daemon_ready_reports_data_plane_failure(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def fail_services(_layer_stack_root: str) -> object:
        raise RuntimeError("synthetic data-plane failure")

    monkeypatch.setattr(health.request_context, "services", fail_services)

    response = health.runtime_ready(
        {"layer_stack_root": (tmp_path / "stack").as_posix()}
    )

    assert response["success"] is True
    assert response["ready"] is False
    data_plane = _probe(response, "data_plane")
    assert data_plane["status"] == "down"
    details = data_plane["details"]
    assert isinstance(details, dict)
    assert details["error_type"] == "RuntimeError"
    assert "synthetic data-plane failure" in str(details["error"])


def test_daemon_ready_reports_incomplete_data_plane_backend(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _Backend:
        layer_stack = object()

    def incomplete_services(_layer_stack_root: str) -> _Backend:
        return _Backend()

    def fake_shell_services(_args: dict[str, object]) -> tuple[object, object, object, Path]:
        return object(), object(), object(), tmp_path

    monkeypatch.setattr(health.request_context, "services", incomplete_services)
    monkeypatch.setattr(
        health.shell_runner,
        "services",
        fake_shell_services,
    )

    response = health.runtime_ready(
        {"layer_stack_root": (tmp_path / "stack").as_posix()}
    )

    assert response["success"] is True
    assert response["ready"] is False
    data_plane = _probe(response, "data_plane")
    assert data_plane["status"] == "down"
    details = data_plane["details"]
    assert isinstance(details, dict)
    assert details["error_type"] == "RuntimeError"
    assert "handler services returned" in str(details["error"])


def test_daemon_ready_reports_mutation_gate_failure(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _Backend:
        layer_stack = object()
        occ_client = object()
        gitignore = object()
        manager = object()

    def fake_services(_layer_stack_root: str) -> _Backend:
        return _Backend()

    def fake_shell_services(_args: dict[str, object]) -> tuple[object, object, object, Path]:
        return object(), object(), object(), tmp_path

    def fail_backend(_layer_stack_root: str) -> object:
        raise RuntimeError("synthetic mutation-gate failure")

    monkeypatch.setattr(health.request_context, "services", fake_services)
    monkeypatch.setattr(
        health.shell_runner,
        "services",
        fake_shell_services,
    )
    monkeypatch.setattr(health.occ_backend, "build_occ_backend", fail_backend)

    response = health.runtime_ready(
        {"layer_stack_root": (tmp_path / "stack").as_posix()}
    )

    assert response["success"] is True
    assert response["ready"] is False
    mutation_gate = _probe(response, "mutation_gate")
    assert mutation_gate["status"] == "down"
    details = mutation_gate["details"]
    assert isinstance(details, dict)
    assert details["error_type"] == "RuntimeError"
    assert "synthetic mutation-gate failure" in str(details["error"])


def test_daemon_ready_reports_incomplete_mutation_gate_backend(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _Backend:
        layer_stack = object()
        occ_client = object()
        gitignore = object()

    def fake_services(_layer_stack_root: str) -> _Backend:
        return _Backend()

    def fake_shell_services(_args: dict[str, object]) -> tuple[object, object, object, Path]:
        return object(), object(), object(), tmp_path

    monkeypatch.setattr(health.request_context, "services", fake_services)
    monkeypatch.setattr(
        health.shell_runner,
        "services",
        fake_shell_services,
    )
    monkeypatch.setattr(
        health.occ_backend,
        "build_occ_backend",
        fake_services,
    )

    response = health.runtime_ready(
        {"layer_stack_root": (tmp_path / "stack").as_posix()}
    )

    assert response["success"] is True
    assert response["ready"] is False
    mutation_gate = _probe(response, "mutation_gate")
    assert mutation_gate["status"] == "down"
    details = mutation_gate["details"]
    assert isinstance(details, dict)
    assert details["error_type"] == "RuntimeError"
    assert "OCC backend type mismatch" in str(details["error"])


def test_daemon_ready_reports_explicit_workspace_mount_mode(tmp_path: Path) -> None:
    response = health.runtime_ready(
        {"layer_stack_root": (tmp_path / "stack").as_posix()}
    )

    data_plane = _probe(response, "data_plane")
    assert data_plane["status"] == "ok"
    details = data_plane["details"]
    assert isinstance(details, dict)
    assert details["workspace_mount_mode"] in {
        "private_namespace",
        "copy_backed",
    }
