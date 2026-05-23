"""PR 0 acceptance backstop: ``_LinuxRuntime.mount_overlay`` actually fires.

This test bypasses ``IsolatedWorkspaceManager.enter()`` entirely. It spawns
the ns_holder + opens ns FDs + invokes ``_LinuxRuntime.mount_overlay``
directly, then asserts the overlay line appears in
``/proc/<root_pid>/mountinfo`` inside the workspace mntns.

Why a backstop: a phase-2 failure in the broader ``enter()`` lifecycle can
otherwise be ambiguous between "mount itself is broken" and "something
around the mount is broken" (veth, cgroup, dns, handshake). With this test
green, a later regression in the full flow points to non-mount surfaces.

Runs the kernel-touching sequence inside the sweevo container via
``raw_exec``; the host-side test only marshals the script and asserts on
``exit_code`` + stdout markers.
"""

from __future__ import annotations

import pytest

from sandbox.api import raw_exec
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)


pytestmark = pytest.mark.asyncio


_IN_CONTAINER_SCRIPT = r"""
import asyncio
import os
import shutil
import sys
import tempfile
from pathlib import Path

from sandbox.isolated_workspace.manager import (
    IsolatedWorkspaceHandle,
    _LinuxRuntime,
)

runtime = _LinuxRuntime()
scratch = Path(tempfile.mkdtemp(prefix="iws-backstop-"))
lower = scratch / "lower"
upper = scratch / "upper"
work = scratch / "work"
for d in (lower, upper, work):
    d.mkdir(parents=True, exist_ok=True)
(lower / "BACKSTOP_SENTINEL").write_text("ok")

handle = IsolatedWorkspaceHandle(
    handle_id="backstop00000000",
    agent_id="backstop",
    lease_id="backstop-lease",
    manifest_version=0,
    manifest_root_hash="",
    workspace_root="/testbed",
    scratch_dir=scratch,
    upperdir=upper,
    workdir=work,
)

exit_code = 0
try:
    handle.root_pid = runtime.spawn_ns_holder(handle, setup_timeout_s=30.0)
    handle.ns_fds.update(runtime.open_ns_fds(handle.root_pid))
    asyncio.run(runtime.mount_overlay(handle, layer_paths=(str(lower),)))

    mi_path = "/proc/%d/mountinfo" % handle.root_pid
    with open(mi_path, "r", encoding="utf-8") as fh:
        mi = fh.read()
    found = any(
        " - overlay overlay " in line and " /testbed " in line
        for line in mi.splitlines()
    )
    if not found:
        sys.stderr.write("BACKSTOP_FAIL no overlay line in %s\n%s\n" % (mi_path, mi))
        exit_code = 1
    else:
        sys.stdout.write("BACKSTOP_OK overlay mounted at /testbed\n")
finally:
    if handle.root_pid:
        runtime.kill_holder(handle.root_pid, grace_s=1.0)
    for fd in handle.ns_fds.values():
        try:
            os.close(fd)
        except OSError:
            pass
    for fd in (handle.readiness_fd, handle.control_fd):
        if fd >= 0:
            try:
                os.close(fd)
            except OSError:
                pass
    shutil.rmtree(scratch, ignore_errors=True)

sys.exit(exit_code)
"""


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(180)
async def test_mount_overlay_backstop(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    # PYTHONPATH=/tmp/eos-sandbox-runtime lets the in-container python3 see
    # the daemon's runtime bundle (sandbox.isolated_workspace.manager etc.).
    result = await raw_exec(
        sandbox_id,
        (
            "PYTHONPATH=/tmp/eos-sandbox-runtime python3 - <<'PY'\n"
            f"{_IN_CONTAINER_SCRIPT}\nPY"
        ),
        cwd="/",
        timeout=120,
    )
    assert result.exit_code == 0, (
        f"mount_overlay backstop failed: exit_code={result.exit_code}\n"
        f"stdout={getattr(result, 'stdout', '')!r}\n"
        f"stderr={getattr(result, 'stderr', '')!r}"
    )
    assert "BACKSTOP_OK" in getattr(result, "stdout", ""), result
