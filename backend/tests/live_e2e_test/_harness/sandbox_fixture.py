"""Generic sandbox fixture for live_e2e_test suites.

Defines :class:`SandboxHandle` and the pytest fixture set described in
§3 of ``../live-e2e-test-suite-plan.md``. The ``live_sandbox`` fixture
is session-scoped and brings up exactly one Daytona sandbox via
``setup_after_create`` — the same path agents use. Per-suite fixtures
reset ``/testbed`` and populate the layer/overlay/occ slots.

Note on layer-stack scope: the migration plan keeps ``LayerStackManager``
host-side; the suite plan's "tmpfs root inside the sandbox" framing is
realized by giving the host-side manager a *local* ``tmp_path`` storage
root while the sandbox stays up to satisfy the gate. Tests that need
remote shell access reach for ``handle.raw_exec``.
"""

from __future__ import annotations

import asyncio
import time
from collections.abc import AsyncIterator, Awaitable, Callable, Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import pytest_asyncio

from config import load_settings
from sandbox.api import status as sb_status
from sandbox.api.tool import edit as edit_mod
from sandbox.api.tool import read as read_mod
from sandbox.api.tool import shell as shell_mod
from sandbox.api.tool import write as write_mod
from sandbox.api.tool.raw_exec import raw_exec as raw_exec_fn
from sandbox.api.utils.models import (
    EditFileRequest,
    EditFileResult,
    RawExecResult,
    ReadFileRequest,
    ReadFileResult,
    SandboxCaller,
    SearchReplaceEdit,
    ShellRequest,
    ShellResult,
    WriteFileRequest,
    WriteFileResult,
)
from sandbox.control.ops.setup import setup_after_create
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.occ.client import dispose_occ_service, register_occ_service
from sandbox.occ.content.gitignore_oracle import GitignoreOracle
from sandbox.occ.service import OccService
from sandbox.overlay.client import (
    OverlayClient,
    dispose_overlay_client,
    register_overlay_client,
)
from sandbox.overlay.runner.snapshot_overlay_runner import SnapshotOverlayRunner
from sandbox.providers.daytona.bootstrap import bootstrap_daytona_provider
from sandbox.providers.registry import get_default_provider, register_adapter

from .occ_workload import make_sandbox_gitignore_run_fn
from .overlay_probe import OVERLAY_ROOT, script_purge_overlay_mounts, wrap_unshare


WORKSPACE_ROOT = "/testbed"


# -- Public handle --------------------------------------------------------


@dataclass(frozen=True)
class ToolBundle:
    """Bound wrappers over the four ``sandbox.api.tool`` verbs."""

    sandbox_id: str
    caller: SandboxCaller

    async def read_file(self, path: str) -> ReadFileResult:
        return await read_mod.read_file(
            self.sandbox_id, ReadFileRequest(path=path, caller=self.caller)
        )

    async def write_file(
        self,
        path: str,
        content: str,
        *,
        overwrite: bool = True,
        description: str = "",
    ) -> WriteFileResult:
        return await write_mod.write_file(
            self.sandbox_id,
            WriteFileRequest(
                path=path,
                content=content,
                caller=self.caller,
                description=description,
                overwrite=overwrite,
            ),
        )

    async def edit_file(
        self,
        path: str,
        edits: list[tuple[str, str]],
        *,
        description: str = "",
    ) -> EditFileResult:
        return await edit_mod.edit_file(
            self.sandbox_id,
            EditFileRequest(
                path=path,
                edits=tuple(
                    SearchReplaceEdit(old_text=old, new_text=new) for old, new in edits
                ),
                caller=self.caller,
                description=description,
            ),
        )

    async def shell(
        self,
        command: str,
        *,
        cwd: str | None = None,
        timeout: float | None = None,
        description: str = "",
    ) -> ShellResult:
        return await shell_mod.shell(
            self.sandbox_id,
            ShellRequest(
                command=command,
                caller=self.caller,
                cwd=cwd,
                timeout=timeout,
                description=description,
            ),
        )


@dataclass(frozen=True)
class SandboxHandle:
    """Single contract every live test depends on. See plan §3."""

    sandbox_id: str
    caller: SandboxCaller
    raw_exec: Callable[..., Awaitable[RawExecResult]]
    tool: ToolBundle
    workspace_root: str = WORKSPACE_ROOT
    layer_stack: LayerStackManager | None = None
    overlay_client: Any | None = None
    occ_service: Any | None = None
    extras: dict[str, Any] = field(default_factory=dict)


# -- Lifecycle helpers ----------------------------------------------------


def _make_caller() -> SandboxCaller:
    return SandboxCaller(agent_id="live-e2e-tests")


def _bring_up_sandbox(name: str) -> str:
    bootstrap_daytona_provider()
    settings = load_settings()
    image = settings.sandbox.default_image.strip()
    if not image:
        pytest.skip(
            "live test requires settings.sandbox.default_image (set "
            "EPHEMERALOS_SANDBOX_DEFAULT_IMAGE in .env to a prebaked "
            "image with git, /testbed, and the runtime bundle marker)"
        )
    provider = get_default_provider()
    created = provider.create(
        name=name,
        image=image,
        language="python",
        labels={"purpose": "live-e2e-tests", "project_dir": WORKSPACE_ROOT},
    )
    sandbox_id = str(created["id"])
    register_adapter(sandbox_id, provider)
    setup_after_create(sandbox_id, WORKSPACE_ROOT)
    return sandbox_id


def _delete_sandbox_quietly(sandbox_id: str, name: str) -> None:
    if sandbox_id:
        sb_status.delete_sandbox(sandbox_id)
        return
    for sandbox in sb_status.list_sandboxes():
        if sandbox.get("name") == name and sandbox.get("id"):
            sb_status.delete_sandbox(str(sandbox["id"]))


# -- Pytest fixtures ------------------------------------------------------


@pytest.fixture(scope="session")
def live_sandbox() -> Iterator[SandboxHandle]:
    """Session-scoped Daytona sandbox brought up via ``setup_after_create``.

    Bring-up takes ~7 s; per-test fixtures reset ``/testbed`` instead of
    rebuilding the sandbox.
    """
    name = f"eos-live-e2e-{int(time.time())}"
    sandbox_id = ""
    try:
        sandbox_id = _bring_up_sandbox(name)
        caller = _make_caller()
        handle = SandboxHandle(
            sandbox_id=sandbox_id,
            caller=caller,
            raw_exec=raw_exec_fn,
            tool=ToolBundle(sandbox_id=sandbox_id, caller=caller),
        )
        yield handle
    finally:
        _delete_sandbox_quietly(sandbox_id, name)


async def _reset_workspace(sandbox_id: str) -> None:
    """Per-test reset of ``/testbed`` to its post-``ensure_git`` baseline.

    Assumes the prebaked image already provides ``/testbed`` and ``git``;
    falls back to seeding an empty git repo on first use only if the
    image's ``/testbed`` lacks ``.git``. Otherwise just runs ``reset
    --hard`` + ``clean -fdx``.
    """
    result = await raw_exec_fn(
        sandbox_id,
        "set -e; "
        f"cd {WORKSPACE_ROOT}; "
        "if [ ! -d .git ]; then "
        "  git -c init.defaultBranch=main init -q .; "
        "  git -c user.email=eos@local -c user.name=eos "
        "      commit -q --allow-empty -m 'live-e2e: baseline'; "
        "fi; "
        "git reset --hard HEAD >/dev/null 2>&1 || true; "
        "git clean -fdx >/dev/null 2>&1 || true",
        timeout=60,
    )
    if result.exit_code != 0:
        pytest.fail(f"workspace reset failed: {result.stderr or result.stdout}")


@pytest_asyncio.fixture
async def layer_stack_sandbox(
    live_sandbox: SandboxHandle, tmp_path: Path
) -> AsyncIterator[SandboxHandle]:
    """``live_sandbox`` + a host-local ``LayerStackManager`` rooted at ``tmp_path``.

    Layer-stack atomicity / GC / squash / lease properties are host-side
    invariants; the live sandbox is held up only to keep the gate honest
    for parity with neighbouring suites.
    """
    await _reset_workspace(live_sandbox.sandbox_id)
    storage = tmp_path / "layer_stack_storage"
    manager = LayerStackManager(storage)
    handle = SandboxHandle(
        sandbox_id=live_sandbox.sandbox_id,
        caller=live_sandbox.caller,
        raw_exec=live_sandbox.raw_exec,
        tool=live_sandbox.tool,
        layer_stack=manager,
        extras={"storage_root": storage},
    )
    yield handle


async def _purge_overlay_mounts(sandbox_id: str) -> None:
    """Detach any leaked overlayfs mounts the previous test left under OVERLAY_ROOT."""
    cmd = wrap_unshare(script_purge_overlay_mounts(overlay_root=OVERLAY_ROOT))
    # Best-effort: some kernels reject unshare without privileges; ignore failures.
    await raw_exec_fn(sandbox_id, cmd, timeout=30)


@pytest_asyncio.fixture
async def overlay_sandbox(
    live_sandbox: SandboxHandle, tmp_path: Path
) -> AsyncIterator[SandboxHandle]:
    """Live sandbox + host-side overlay client + per-test mount cleanup.

    Registers an :class:`OverlayClient` over a host-local
    :class:`LayerStackManager` so callers exercising the typed runtime
    route work end-to-end. Direct ``mount(2)`` measurements live inside the
    sandbox and reach for ``handle.raw_exec`` plus the helpers in
    ``_harness/overlay_probe.py``.
    """
    await _reset_workspace(live_sandbox.sandbox_id)
    await _purge_overlay_mounts(live_sandbox.sandbox_id)

    storage = tmp_path / "overlay_storage"
    manager = LayerStackManager(storage)
    overlay_client = OverlayClient(runner=SnapshotOverlayRunner(manager))
    register_overlay_client(live_sandbox.sandbox_id, overlay_client)

    handle = SandboxHandle(
        sandbox_id=live_sandbox.sandbox_id,
        caller=live_sandbox.caller,
        raw_exec=live_sandbox.raw_exec,
        tool=live_sandbox.tool,
        layer_stack=manager,
        overlay_client=overlay_client,
        extras={
            "storage_root": storage,
            "overlay_root": OVERLAY_ROOT,
        },
    )
    try:
        yield handle
    finally:
        try:
            await _purge_overlay_mounts(live_sandbox.sandbox_id)
        finally:
            dispose_overlay_client(live_sandbox.sandbox_id)


@pytest_asyncio.fixture
async def occ_sandbox(
    live_sandbox: SandboxHandle, tmp_path: Path
) -> AsyncIterator[SandboxHandle]:
    """Live sandbox + host-side ``OccService`` whose oracle queries ``/testbed``.

    The migration plan keeps :class:`LayerStackManager` host-side, so
    OCC bookkeeping lives in ``tmp_path/occ_storage``. But the
    :class:`GitignoreOracle` is bridged to the live sandbox: every
    ``git check-ignore`` lookup ships over ``raw_exec`` against
    ``/testbed`` so OCC's route decisions consult the real sandbox
    state agents see, not a synthetic host workspace.

    ``_reset_workspace`` runs first to give us a clean ``/testbed``
    git baseline that test bodies can layer ``.gitignore`` patterns
    onto via :func:`write_sandbox_gitignore`.
    """
    await _reset_workspace(live_sandbox.sandbox_id)

    storage = tmp_path / "occ_storage"
    manager = LayerStackManager(storage)
    loop = asyncio.get_running_loop()
    run_fn = make_sandbox_gitignore_run_fn(live_sandbox.sandbox_id, loop)
    gitignore = GitignoreOracle("/testbed", run=run_fn)
    service = OccService(gitignore=gitignore, layer_stack=manager)
    register_occ_service(live_sandbox.sandbox_id, service)

    handle = SandboxHandle(
        sandbox_id=live_sandbox.sandbox_id,
        caller=live_sandbox.caller,
        raw_exec=live_sandbox.raw_exec,
        tool=live_sandbox.tool,
        layer_stack=manager,
        occ_service=service,
        extras={
            "storage_root": storage,
            "workspace_root": "/testbed",
            "gitignore_oracle": gitignore,
            "gitignore_run_fn": run_fn,
            "payloads_root": tmp_path / "occ_payloads",
        },
    )
    try:
        yield handle
    finally:
        dispose_occ_service(live_sandbox.sandbox_id)


@pytest_asyncio.fixture
async def integrated_sandbox(
    live_sandbox: SandboxHandle,
) -> AsyncIterator[SandboxHandle]:
    pytest.skip("integrated_sandbox fixture lands with the integrated suite")
    yield live_sandbox  # pragma: no cover


__all__ = [
    "SandboxHandle",
    "ToolBundle",
    "WORKSPACE_ROOT",
    "live_sandbox",
    "layer_stack_sandbox",
    "overlay_sandbox",
    "occ_sandbox",
    "integrated_sandbox",
]
