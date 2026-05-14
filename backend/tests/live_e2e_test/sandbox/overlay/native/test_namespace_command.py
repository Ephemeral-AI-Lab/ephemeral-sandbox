"""Phase 4 native probes for command execution edge handling."""

from __future__ import annotations

import pytest

from ..._harness.native_cases import run_native_case
from ..._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
from sandbox.overlay import run_user_command
from sandbox.overlay import read_output_ref

label = "overlay.native.namespace_command"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
workspace = root / "workspace"
workspace.mkdir(parents=True)
refs = root / "refs"

invalid_cwd_rejected = False
try:
    run_user_command(
        command=("bash", "-lc", "true"),
        workspace_root=workspace,
        cwd="../escape",
        env={},
        timeout_seconds=2,
        stdout_ref=refs / "bad.out",
        stderr_ref=refs / "bad.err",
    )
except ValueError:
    invalid_cwd_rejected = True
assert invalid_cwd_rejected

env_result = run_user_command(
    command=("bash", "-lc", "printf env:$EOS_PHASE4_FLAG"),
    workspace_root=workspace,
    cwd=".",
    env={"EOS_PHASE4_FLAG": "visible"},
    timeout_seconds=2,
    stdout_ref=refs / "env.out",
    stderr_ref=refs / "env.err",
)
assert env_result.exit_code == 0
assert read_output_ref(env_result.stdout_ref) == "env:visible"

signal_result = run_user_command(
    command=("bash", "-lc", "kill -TERM $$"),
    workspace_root=workspace,
    cwd=".",
    env={},
    timeout_seconds=2,
    stdout_ref=refs / "signal.out",
    stderr_ref=refs / "signal.err",
)
assert signal_result.exit_code != 0

missing_cap = run_user_command(
    command=("bash", "-lc", "unshare -Urm true >/dev/null 2>&1"),
    workspace_root=workspace,
    cwd=".",
    env={},
    timeout_seconds=5,
    stdout_ref=refs / "cap.out",
    stderr_ref=refs / "cap.err",
)

_emit(label, started, before, {
    "invalid_cwd_rejected": invalid_cwd_rejected,
    "signal_exit_code": signal_result.exit_code,
    "unshare_exit_code": missing_cap.exit_code,
    "env_stdout": read_output_ref(env_result.stdout_ref),
})
"""


async def test_namespace_command_edges(native_sandbox: SandboxHandle) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="overlay.native.namespace_command",
    )
    assert payload["invalid_cwd_rejected"] is True
    assert payload["signal_exit_code"] != 0
