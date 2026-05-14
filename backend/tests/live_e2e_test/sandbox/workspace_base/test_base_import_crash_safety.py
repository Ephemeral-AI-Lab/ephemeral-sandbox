"""Phase-01 crash-safety probes for interrupted workspace-base imports."""

from __future__ import annotations

import pytest

from .._harness.sandbox_fixture import SandboxHandle
from .._harness.workspace_base_metrics import write_jsonl_artifact
from .._harness.workspace_base_probe import run_workspace_base_probe


pytestmark = pytest.mark.asyncio


_CRASH_SAFETY_BODY = r"""
import signal
import sys
from sandbox.daemon.service.workspace_server import LayerStackWorkspaceServer

label = "workspace_base.import_crash_safety"
case = "base_import_crash_safety"
started = time.perf_counter()
seed = WORKSPACE_ROOT / "phase01-crash-fixtures"
shutil.rmtree(seed, ignore_errors=True)
seed.mkdir(parents=True, exist_ok=True)
for index in range(64):
    (seed / ("%03d.txt" % index)).write_text("crash-%03d\n" % index, encoding="utf-8")

summary_stack = _phase01_root(label, "summary")
summary_binding, summary_timings = _build_base(summary_stack)
workspace_inv = _inventory(WORKSPACE_ROOT)
rows = []

CHILD_SOURCE = r'''
import os
import sys
import time
from pathlib import Path

import sandbox.layer_stack.workspace_base as wb

workspace = Path(sys.argv[1])
stack = Path(sys.argv[2])
marker = Path(sys.argv[3])
mode = sys.argv[4]

marker.parent.mkdir(parents=True, exist_ok=True)

if mode == "during_layer_write":
    def stuck_write(stack_root, entries):
        staging = Path(stack_root) / "staging" / "B000001-base.staging"
        staging.mkdir(parents=True, exist_ok=True)
        (staging / "partial.txt").write_text("partial\n", encoding="utf-8")
        marker.write_text("ready\n", encoding="utf-8")
        time.sleep(3600)
    wb._write_base_layer = stuck_write

elif mode == "after_base_layer_rename_before_manifest":
    def stuck_quiescent(*, workspace, expected_entries, expected_root_hash):
        marker.write_text("ready\n", encoding="utf-8")
        time.sleep(3600)
    wb._assert_workspace_quiescent = stuck_quiescent

elif mode == "after_manifest_before_workspace_json":
    def stuck_binding(binding):
        marker.write_text("ready\n", encoding="utf-8")
        time.sleep(3600)
    wb.write_workspace_binding_atomic = stuck_binding

else:
    raise AssertionError(mode)

wb.build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
'''


def _publish_state(stack_root):
    stack_root = Path(stack_root)
    binding_exists = workspace_binding_path(stack_root).exists()
    manifest_file = manifest_path(stack_root)
    manifest = read_manifest(manifest_file)
    layers = list((stack_root / "layers").iterdir()) if (stack_root / "layers").exists() else []
    staging = list((stack_root / "staging").iterdir()) if (stack_root / "staging").exists() else []
    consistent = (
        binding_exists
        and manifest_file.exists()
        and manifest.version > 0
        and all(path.exists() for path in layers)
    )
    return {
        "binding_exists": binding_exists,
        "manifest_exists": manifest_file.exists(),
        "manifest_version": manifest.version,
        "manifest_depth": manifest.depth,
        "layers": [path.name for path in layers],
        "staging": [path.name for path in staging],
        "consistent": consistent,
    }


def _restart_ensure_outcome(stack_root):
    try:
        binding, created = LayerStackWorkspaceServer(stack_root).ensure_workspace_base(
            workspace_root=WORKSPACE_ROOT,
        )
        return {
            "success": True,
            "created": created,
            "binding": binding.to_dict(),
        }
    except Exception as exc:
        return {
            "success": False,
            "error_kind": type(exc).__name__,
            "error_message": str(exc),
        }


def _kill_case(mode):
    stack_root = _phase01_root(label, mode)
    marker = stack_root / ("%s.ready" % mode)
    t0 = time.perf_counter()
    proc = subprocess.Popen(
        [sys.executable, "-c", CHILD_SOURCE, str(WORKSPACE_ROOT), str(stack_root), str(marker), mode],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if marker.exists():
            break
        if proc.poll() is not None:
            stdout, stderr = proc.communicate(timeout=5)
            raise AssertionError("%s exited before marker stdout=%r stderr=%r" % (mode, stdout, stderr))
        time.sleep(0.05)
    else:
        proc.kill()
        stdout, stderr = proc.communicate(timeout=5)
        raise AssertionError("%s did not reach interruption point stdout=%r stderr=%r" % (mode, stdout, stderr))

    os.kill(proc.pid, signal.SIGKILL)
    stdout, stderr = proc.communicate(timeout=10)
    state = _publish_state(stack_root)
    ensure = _restart_ensure_outcome(stack_root)
    assert ensure["success"] is False, (mode, state, ensure)
    assert state["consistent"] is False, (mode, state)
    rows.append(_call_row(
        case,
        mode,
        True,
        t0,
        extra={
            "returncode": proc.returncode,
            "stdout": stdout,
            "stderr": stderr,
            "publish_state": state,
            "restart_ensure": ensure,
        },
    ))


for mode in (
    "during_layer_write",
    "after_base_layer_rename_before_manifest",
    "after_manifest_before_workspace_json",
):
    _kill_case(mode)

clean_stack = _phase01_root(label, "restart-clean")
clean_t0 = time.perf_counter()
clean_binding, created = LayerStackWorkspaceServer(clean_stack).ensure_workspace_base(
    workspace_root=WORKSPACE_ROOT,
)
clean_state = _publish_state(clean_stack)
assert created is True
assert clean_state["consistent"] is True, clean_state
again_binding, again_created = LayerStackWorkspaceServer(clean_stack).ensure_workspace_base(
    workspace_root=WORKSPACE_ROOT,
)
assert again_created is False
assert again_binding.to_dict() == clean_binding.to_dict()
rows.append(_call_row(
    case,
    "daemon_restart_clean_ensure_workspace_base",
    True,
    clean_t0,
    extra={
        "created": created,
        "second_created": again_created,
        "publish_state": clean_state,
    },
))

summary = _base_summary(
    case,
    summary_binding,
    workspace_inv,
    summary_timings,
    pass_bars={
        "kill_during_layer_write_fail_closed": True,
        "kill_after_base_layer_rename_before_manifest_fail_closed": True,
        "kill_after_manifest_before_workspace_json_fail_closed": True,
        "restart_ensure_requires_consistent_binding_manifest_and_layer": True,
    },
)
_emit_workspace_payload(label, started, summary, rows)
"""


async def test_interrupted_base_import_never_observes_inconsistent_success(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    payload = await run_workspace_base_probe(
        workspace_base_sandbox,
        _CRASH_SAFETY_BODY,
        label="workspace_base.import_crash_safety",
        timeout=300,
    )
    rows = payload["rows"]
    assert len(rows) == 4
    assert all(row["success"] for row in rows)
    artifact = write_jsonl_artifact(
        case="base_import_crash_safety",
        summary=payload["summary"],
        rows=rows,
    )
    print(f"\n[phase01:base_import_crash_safety] artifact={artifact}")
