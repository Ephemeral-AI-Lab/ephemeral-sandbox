"""Phase 05 — OCC mutation gate daemon-boundary + retry-bound tests.

These tests assert the §6 structural invariants:

* occ-server is not a host-callable daemon dispatch module.
* Public data operations do not dispatch through occ-server.
* In-workspace classifier predicate lives in command-exec only;
  occ-server source contains no ``workspace_root`` classification call
  sites.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.daemon import occ_runtime_services


# ---------------------------------------------------------------------------
def test_occ_server_module_does_not_classify_paths() -> None:
    """occ-server must not own the in-workspace classifier — single source of
    truth lives on daemon built-ins (builtin_operations/workspace_tool_payloads.py)."""
    occ_server_source = Path(occ_runtime_services.__file__).read_text()

    assert ".workspace_root" not in occ_server_source
    assert "workspace_root =" not in occ_server_source
    assert "workspace_root==" not in occ_server_source


def test_data_api_ops_do_not_dispatch_to_occ_server() -> None:
    """Data API ops must never route directly to occ-server."""
    from sandbox.daemon.rpc import dispatcher as server

    server._register_builtin_operations()
    for op in ("api.write_file", "api.edit_file", "api.read_file", "api.v1.exec_command"):
        handler = server.OP_TABLE[op]
        assert handler.__module__ != "sandbox.daemon.occ_runtime_services"


# ---------------------------------------------------------------------------
# CAS retry exhaustion bound (MAX_OCC_CAS_RETRIES default 3)
# ---------------------------------------------------------------------------


def test_max_occ_cas_retries_is_named_constant_with_positive_default() -> None:
    """MAX_OCC_CAS_RETRIES is the public, testable retry budget."""
    from sandbox.occ.commit_queue import MAX_OCC_CAS_RETRIES

    assert isinstance(MAX_OCC_CAS_RETRIES, int)
    assert MAX_OCC_CAS_RETRIES >= 1
    # Plan §1 says default = 3.
    assert MAX_OCC_CAS_RETRIES == 3


@pytest.mark.asyncio
async def test_cas_retry_loop_bounded_under_no_contention(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A no-contention write completes promptly — regression guard against the
    retry loop turning into a busy spin."""
    import asyncio

    from sandbox.daemon import builtin_operations, occ_runtime_services
    from sandbox.layer_stack.workspace_base import build_workspace_base

    occ_runtime_services.clear_occ_runtime_services()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    async def fake_run_in_namespace(handle, req):
        target = handle.upperdir / str(req.args["path"])
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(str(req.args["content"]), encoding="utf-8")
        return {"success": True, "status": "ok", "timings": {}}

    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.run_in_namespace",
        fake_run_in_namespace,
    )

    result = await asyncio.wait_for(
        builtin_operations.WORKSPACE_TOOL_HANDLERS["write_file"](
            {
                "layer_stack_root": stack.as_posix(),
                "path": "ok.txt",
                "content": "fine\n",
            }
        ),
        timeout=2.0,
    )
    assert result["success"] is True


@pytest.mark.asyncio
async def test_cas_retry_exhaustion_returns_conflict_result(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Persistent CAS mismatch surfaces a per-path conflict result and does
    NOT loop indefinitely. We monkey-patch the layer-stack publisher to
    always raise :class:`ManifestConflictError` so every retry attempt fails."""
    import asyncio

    from sandbox.layer_stack.manifest import ManifestConflictError
    from sandbox.layer_stack.workspace_base import build_workspace_base
    from sandbox.occ.commit_queue import MAX_OCC_CAS_RETRIES
    from sandbox.daemon import builtin_operations, occ_runtime_services

    occ_runtime_services.clear_occ_runtime_services()
    workspace = tmp_path / "ws"
    workspace.mkdir()
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    async def fake_run_in_namespace(handle, req):
        target = handle.upperdir / str(req.args["path"])
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(str(req.args["content"]), encoding="utf-8")
        return {"success": True, "status": "ok", "timings": {}}

    monkeypatch.setattr(
        "sandbox.ephemeral_workspace.pipeline.run_in_namespace",
        fake_run_in_namespace,
    )

    services = occ_runtime_services.get_occ_runtime_services(stack.as_posix())
    publisher = services.layer_stack_manager._publisher  # type: ignore[attr-defined]

    call_counter = {"n": 0}
    real_publish = publisher.publish_layer

    def always_cas_mismatch(*_args, **_kwargs):
        call_counter["n"] += 1
        raise ManifestConflictError("synthetic CAS mismatch for retry-exhaustion test")

    publisher.publish_layer = always_cas_mismatch  # type: ignore[method-assign]
    try:
        result = await asyncio.wait_for(
            builtin_operations.WORKSPACE_TOOL_HANDLERS["write_file"](
                {
                    "layer_stack_root": stack.as_posix(),
                    "path": "ok.txt",
                    "content": "should-fail\n",
                }
            ),
            timeout=3.0,
        )
    finally:
        publisher.publish_layer = real_publish  # type: ignore[method-assign]

    # Result is a conflict, not an exception; success is False.
    assert result["success"] is False
    assert result["conflict"] is not None
    # The conflict path carries ABORTED_VERSION semantics.
    assert "CAS mismatch retry budget exhausted" in result["conflict"]["message"]
    # Retry budget was respected — exactly MAX retries observed.
    assert call_counter["n"] == MAX_OCC_CAS_RETRIES


# ---------------------------------------------------------------------------
# Phase 05.5 — single OCC runtime service bundle per layer_stack_root across peers
# ---------------------------------------------------------------------------


def test_single_occ_runtime_services_cache_per_layer_stack_root(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """All runtime peers share one OccRuntimeServices per layer_stack_root.

    After Phase 05.5 the OCC service bundle is owned by ``occ_runtime_services``;
    the per-verb handler scaffolding (write/edit/read/shell) and the
    api-handler manager helper all resolve through the same factory.
    """
    from sandbox.daemon import occ_runtime_services

    occ_runtime_services.clear_occ_runtime_services()

    class _FakeManager:
        def __init__(self, root: str) -> None:
            self.root = root

        @property
        def storage_root(self) -> str:
            return f"{self.root}/storage"

    class _FakeLayerStack:
        def __init__(self, manager: _FakeManager) -> None:
            self.manager = manager

        @property
        def storage_root(self) -> str:
            return self.manager.storage_root

    monkeypatch.setattr(
        occ_runtime_services,
        "get_layer_stack_manager",
        lambda root: _FakeManager(str(root)),
    )
    monkeypatch.setattr(occ_runtime_services, "LayerStackPortAdapter", _FakeLayerStack)
    monkeypatch.setattr(
        occ_runtime_services,
        "SnapshotGitignoreOracle",
        lambda layer_stack: ("oracle", layer_stack),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "OccService",
        lambda *, gitignore, **kwargs: ("service", gitignore, kwargs),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "OccClient",
        lambda service, *, binding_reader, workspace_ref: (
            binding_reader,
            ("occ-client", service, workspace_ref),
        )[1],
    )

    backend_a = occ_runtime_services.get_occ_runtime_services("/tmp/a")

    # Every per-verb scaffolding path resolves to the cached OccRuntimeServices
    # regardless of trailing path noise; operation handlers (edit/read/write/shell)
    # all dereference fields off this single instance.
    assert occ_runtime_services.get_occ_runtime_services("/tmp/a") is backend_a
    assert occ_runtime_services.get_occ_runtime_services("/tmp/a/.") is backend_a
