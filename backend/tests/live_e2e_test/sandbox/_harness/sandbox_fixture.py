"""Generic sandbox fixture for live_e2e_test suites.

Defines :class:`SandboxHandle` and the pytest fixture set described in
§3 of ``../live-e2e-test-suite-plan.md``. The ``live_sandbox`` fixture
is session-scoped and brings up exactly one Daytona sandbox via
``setup_after_create`` — the same path agents use. Per-suite fixtures
reset ``/testbed`` and any sandbox-runtime layer/overlay/OCC state.

The live suite must exercise the Daytona sandbox, either through direct
in-sandbox probes or through the public sandbox API, never through a local
``LayerStack`` or process-local OCC/overlay registry.
"""

from __future__ import annotations

import os
import shlex
import time
from collections.abc import AsyncIterator, Awaitable, Callable, Iterator
from dataclasses import dataclass, field
from typing import Any

import pytest
import pytest_asyncio

from config import load_settings
import sandbox.api as sandbox_api
import sandbox.host.daemon_client as daemon_client_mod
from sandbox.host.daemon_client import DEFAULT_LAYER_STACK_ROOT
from sandbox.api import (
    EditFileRequest,
    EditFileResult,
    ExecCommandRequest,
    ExecCommandResult,
    RawExecResult,
    ReadFileRequest,
    ReadFileResult,
    SandboxCaller,
    SearchReplaceEdit,
    WriteFileRequest,
    WriteFileResult,
)
from sandbox.host.bootstrap import setup_after_create
from sandbox.provider.bootstrap import bootstrap_sandbox_provider
from sandbox.provider.registry import get_default_provider, register_adapter

from .native_probe import (
    BUNDLE_HASH_MARKER,
    BUNDLE_REMOTE_DIR,
    LAYER_STACK_TEST_PREFIX,
)
from .overlay_probe import OVERLAY_ROOT, script_purge_overlay_mounts, wrap_unshare


WORKSPACE_ROOT = "/testbed"


# -- Public handle --------------------------------------------------------


@dataclass(frozen=True)
class ToolBundle:
    """Bound wrappers over the public ``sandbox.api`` verbs."""

    sandbox_id: str
    caller: SandboxCaller

    async def read_file(self, path: str) -> ReadFileResult:
        return await sandbox_api.read_file(
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
        return await sandbox_api.write_file(
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
        return await sandbox_api.edit_file(
            self.sandbox_id,
            EditFileRequest(
                path=path,
                edits=tuple(SearchReplaceEdit(old_text=old, new_text=new) for old, new in edits),
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
    ) -> ExecCommandResult:
        if cwd:
            command = f"cd {shlex.quote(cwd)} && {command}"
        return await sandbox_api.exec_command(
            self.sandbox_id,
            ExecCommandRequest(
                cmd=command,
                caller=self.caller,
                timeout=int(timeout) if timeout is not None else None,
                description=description,
            ),
        )

    async def layer_metrics(self) -> dict[str, object]:
        return await daemon_client_mod.call_daemon_api(
            self.sandbox_id,
            "api.layer_metrics",
            {"agent_id": self.caller.agent_id},
            timeout=60,
        )

    async def workspace_binding(self) -> dict[str, object]:
        return await daemon_client_mod.call_daemon_api(
            self.sandbox_id,
            "api.workspace_binding",
            {"agent_id": self.caller.agent_id},
            timeout=30,
        )


@dataclass(frozen=True)
class SandboxHandle:
    """Single contract every live test depends on. See plan §3."""

    sandbox_id: str
    caller: SandboxCaller
    raw_exec: Callable[..., Awaitable[RawExecResult]]
    tool: ToolBundle
    workspace_root: str = WORKSPACE_ROOT
    extras: dict[str, Any] = field(default_factory=dict)


# -- Lifecycle helpers ----------------------------------------------------


def _make_caller() -> SandboxCaller:
    return SandboxCaller(agent_id="live-e2e-tests")


def _resolve_live_image(provider_name: str) -> str:
    """Resolve the live-e2e image string, with provider-gated fallback.

    EOS_LIVE_E2E_IMAGE always wins. Daytona falls back to
    settings.sandbox.daytona.default_image so scenario tests use the same
    default prebaked image unless the operator overrides it. Docker requires an
    explicit image because the central Docker config only carries snapshots.
    """
    explicit = (os.environ.get("EOS_LIVE_E2E_IMAGE") or "").strip()
    if explicit:
        return explicit
    if provider_name == "daytona":
        image = load_settings().sandbox.daytona.default_image.strip()
        if image:
            return image
        pytest.skip(
            "live test requires EOS_LIVE_E2E_IMAGE or "
            "settings.sandbox.daytona.default_image "
            "(set EPHEMERALOS_SANDBOX_DEFAULT_IMAGE in .env) - a prebaked image "
            "with git, /testbed, and the runtime bundle marker"
        )
    pytest.skip(
        f"live test under EOS_SANDBOX_PROVIDER={provider_name} requires "
        "EOS_LIVE_E2E_IMAGE to resolve "
        "to a locally-available image tag with git, /testbed, and the "
        "runtime bundle marker."
    )


def _bring_up_sandbox(name: str) -> str:
    bootstrap_sandbox_provider()
    provider = get_default_provider()
    image = _resolve_live_image(provider.name)
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
        sandbox_api.delete_sandbox(sandbox_id)
        return
    for sandbox in sandbox_api.list_sandboxes():
        if sandbox.get("name") == name and sandbox.get("id"):
            sandbox_api.delete_sandbox(str(sandbox["id"]))


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
            raw_exec=sandbox_api.raw_exec,
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
    result = await sandbox_api.raw_exec(
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


async def _reset_runtime_layer_stack(sandbox_id: str) -> None:
    """Remove guarded API state so integrated tests start from an empty stack."""
    quoted_root = shlex.quote(DEFAULT_LAYER_STACK_ROOT)
    result = await sandbox_api.raw_exec(
        sandbox_id,
        f"rm -rf {quoted_root} && mkdir -p {quoted_root}",
        timeout=60,
    )
    if result.exit_code != 0:
        pytest.fail(f"runtime layer-stack reset failed: {result.stderr or result.stdout}")


async def _build_workspace_base(sandbox_id: str) -> None:
    """Recreate the workspace binding/base after clearing runtime state."""
    result = await daemon_client_mod.call_daemon_api(
        sandbox_id,
        "api.build_workspace_base",
        {"workspace_root": WORKSPACE_ROOT, "reset": True},
        timeout=180,
    )
    if not result.get("success"):
        pytest.fail(f"workspace base build failed: {result}")


async def _purge_overlay_mounts(sandbox_id: str) -> None:
    """Detach any leaked overlayfs mounts the previous test left under OVERLAY_ROOT."""
    cmd = wrap_unshare(script_purge_overlay_mounts(overlay_root=OVERLAY_ROOT))
    # Best-effort: some kernels reject unshare without privileges; ignore failures.
    await sandbox_api.raw_exec(sandbox_id, cmd, timeout=30)


@pytest_asyncio.fixture
async def overlay_sandbox(
    live_sandbox: SandboxHandle,
) -> AsyncIterator[SandboxHandle]:
    """Live sandbox + per-test cleanup for direct in-sandbox overlay probes."""
    await _reset_workspace(live_sandbox.sandbox_id)
    await _purge_overlay_mounts(live_sandbox.sandbox_id)

    handle = SandboxHandle(
        sandbox_id=live_sandbox.sandbox_id,
        caller=live_sandbox.caller,
        raw_exec=live_sandbox.raw_exec,
        tool=live_sandbox.tool,
        extras={
            "overlay_root": OVERLAY_ROOT,
        },
    )
    try:
        yield handle
    finally:
        await _purge_overlay_mounts(live_sandbox.sandbox_id)


@pytest_asyncio.fixture
async def integrated_sandbox(
    live_sandbox: SandboxHandle,
) -> AsyncIterator[SandboxHandle]:
    """Live sandbox with public sandbox API state reset inside the runtime."""
    await _reset_workspace(live_sandbox.sandbox_id)
    await _reset_runtime_layer_stack(live_sandbox.sandbox_id)
    await _build_workspace_base(live_sandbox.sandbox_id)
    yield live_sandbox


@pytest_asyncio.fixture
async def workspace_base_sandbox(
    live_sandbox: SandboxHandle,
) -> AsyncIterator[SandboxHandle]:
    """Live sandbox reset for phase-01 workspace-base import tests."""
    await _assert_runtime_bundle_installed(live_sandbox.sandbox_id)
    await _reset_workspace(live_sandbox.sandbox_id)
    await _reset_runtime_layer_stack(live_sandbox.sandbox_id)
    yield live_sandbox


async def _assert_runtime_bundle_installed(sandbox_id: str) -> None:
    """Fail fast if the prebaked image's ``setup_after_create`` did not stage the bundle."""
    quoted = shlex.quote(BUNDLE_HASH_MARKER)
    result = await sandbox_api.raw_exec(
        sandbox_id,
        f"test -f {quoted} && cat {quoted}",
        timeout=15,
    )
    if result.exit_code != 0:
        pytest.fail(
            f"runtime bundle marker {BUNDLE_HASH_MARKER} missing — "
            "did setup_after_create run? "
            f"stderr={result.stderr!r} stdout={result.stdout!r}"
        )


async def _purge_layer_stack_test_roots(sandbox_id: str) -> None:
    """Remove per-probe scratch dirs left under ``/eos/layer-stack-test-*``."""
    pattern = shlex.quote(LAYER_STACK_TEST_PREFIX) + "*"
    # Use shell glob so the pattern is expanded inside the sandbox.
    await sandbox_api.raw_exec(
        sandbox_id,
        f"sh -c 'rm -rf -- {pattern}'",
        timeout=30,
    )


@pytest_asyncio.fixture
async def native_sandbox(
    live_sandbox: SandboxHandle,
) -> AsyncIterator[SandboxHandle]:
    """Live sandbox prepared for native probes that import the runtime bundle.

    Confirms ``/eos/daemon/.bundle-hash`` exists, resets
    ``/testbed``, clears ``DEFAULT_LAYER_STACK_ROOT``, and removes any
    per-probe scratch dirs left under
    ``/eos/layer-stack-test-*``.
    """
    await _assert_runtime_bundle_installed(live_sandbox.sandbox_id)
    await _reset_workspace(live_sandbox.sandbox_id)
    await _reset_runtime_layer_stack(live_sandbox.sandbox_id)
    await _purge_layer_stack_test_roots(live_sandbox.sandbox_id)

    handle = SandboxHandle(
        sandbox_id=live_sandbox.sandbox_id,
        caller=live_sandbox.caller,
        raw_exec=live_sandbox.raw_exec,
        tool=live_sandbox.tool,
        extras={
            "bundle_remote_dir": BUNDLE_REMOTE_DIR,
            "bundle_hash_marker": BUNDLE_HASH_MARKER,
            "layer_stack_test_prefix": LAYER_STACK_TEST_PREFIX,
        },
    )
    try:
        yield handle
    finally:
        await _purge_layer_stack_test_roots(live_sandbox.sandbox_id)


__all__ = [
    "SandboxHandle",
    "ToolBundle",
    "WORKSPACE_ROOT",
    "live_sandbox",
    "overlay_sandbox",
    "integrated_sandbox",
    "native_sandbox",
]
