"""Phase 4 native probes for runtime invoker edge behavior."""

from __future__ import annotations

import pytest

from ..._harness.native_cases import run_native_case
from ..._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
import subprocess
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.overlay import read_output_ref
from sandbox.overlay import OverlayRuntimeInvoker
from sandbox.overlay import OverlayShellRequest

label = "overlay.native.runtime_invoker"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
manifest = manager.read_active_manifest()
invoker = OverlayRuntimeInvoker(storage_root=manager.storage_root, runtime_root=root / "runtime")

failure = invoker.invoke_sync(
    request=OverlayShellRequest(
        request_id="failure",
        command=("bash", "-lc", "printf fail >&2; exit 17"),
        cwd=".",
        env={},
        timeout_seconds=5,
    ),
    manifest=manifest,
)
assert failure.exit_code == 17
assert read_output_ref(failure.stderr_ref) == "fail"

overflow = invoker.invoke_sync(
    request=OverlayShellRequest(
        request_id="stdout-overflow",
        command=("python3", "-c", "import sys; sys.stdout.buffer.write(b'x' * 262144)"),
        cwd=".",
        env={},
        timeout_seconds=5,
    ),
    manifest=manifest,
)
assert overflow.exit_code == 0
assert Path(overflow.stdout_ref).stat().st_size == 262144

non_utf8 = invoker.invoke_sync(
    request=OverlayShellRequest(
        request_id="non-utf8",
        command=("python3", "-c", "import sys; sys.stdout.buffer.write(bytes([0xff, 0xfe, 0x41]))"),
        cwd=".",
        env={},
        timeout_seconds=5,
    ),
    manifest=manifest,
)
assert non_utf8.exit_code == 0
assert read_output_ref(non_utf8.stdout_ref).endswith("A")

timeout_released = False
try:
    invoker.invoke_sync(
        request=OverlayShellRequest(
            request_id="timeout",
            command=("bash", "-lc", "sleep 5"),
            cwd=".",
            env={},
            timeout_seconds=0.2,
        ),
        manifest=manifest,
    )
except subprocess.TimeoutExpired:
    timeout_released = True
assert timeout_released

_emit(label, started, before, {
    "exec_failure_exit_code": failure.exit_code,
    "stdout_overflow_bytes": Path(overflow.stdout_ref).stat().st_size,
    "non_utf8_decoded": read_output_ref(non_utf8.stdout_ref),
    "timeout_raised": timeout_released,
    "failure_timings": failure.timings,
    "overflow_timings": overflow.timings,
})
"""


async def test_daemon_invoker_exec_failure_stdout_timeout_and_non_utf8(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="overlay.native.runtime_invoker",
    )
    assert payload["exec_failure_exit_code"] == 17
    assert payload["stdout_overflow_bytes"] == 262144
    assert payload["timeout_raised"] is True
