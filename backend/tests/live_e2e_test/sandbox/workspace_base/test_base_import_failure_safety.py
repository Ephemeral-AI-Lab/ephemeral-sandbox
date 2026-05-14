"""Phase-01 fail-closed checks for workspace-base import hazards."""

from __future__ import annotations

import pytest

from .._harness.sandbox_fixture import SandboxHandle
from .._harness.workspace_base_metrics import write_jsonl_artifact
from .._harness.workspace_base_probe import run_workspace_base_probe


pytestmark = pytest.mark.asyncio


_FAILURE_SAFETY_BODY = r"""
import sandbox.layer_stack.workspace_base as wb

label = "workspace_base.import_failure_safety"
case = "base_import_failure_safety"
started = time.perf_counter()
seed = WORKSPACE_ROOT / "phase01-failure-fixtures"
shutil.rmtree(seed, ignore_errors=True)
seed.mkdir(parents=True, exist_ok=True)
(seed / "stable.txt").write_text("stable\n", encoding="utf-8")

summary_stack = _phase01_root(label, "summary")
summary_binding, summary_timings = _build_base(summary_stack)
workspace_inv = _inventory(WORKSPACE_ROOT)
rows = []


def _empty_publish_state(stack_root):
    stack_root = Path(stack_root)
    binding_exists = workspace_binding_path(stack_root).exists()
    manifest = read_manifest(manifest_path(stack_root))
    layers = list((stack_root / "layers").iterdir()) if (stack_root / "layers").exists() else []
    staging = list((stack_root / "staging").iterdir()) if (stack_root / "staging").exists() else []
    return {
        "binding_exists": binding_exists,
        "manifest_version": manifest.version,
        "manifest_depth": manifest.depth,
        "layers": [path.name for path in layers],
        "staging": [path.name for path in staging],
    }


def _record_failure(case_label, func, *, expect_empty=True):
    stack_root = _phase01_root(label, case_label)
    t0 = time.perf_counter()
    error_kind = ""
    error_message = ""
    try:
        func(stack_root)
    except Exception as exc:
        error_kind = type(exc).__name__
        error_message = str(exc)
    else:
        raise AssertionError("%s unexpectedly succeeded" % case_label)
    state = _empty_publish_state(stack_root)
    if expect_empty:
        assert state["binding_exists"] is False, (case_label, state)
        assert state["manifest_version"] in (0,), (case_label, state)
        assert state["manifest_depth"] == 0, (case_label, state)
        assert state["staging"] == [], (case_label, state)
    rows.append(_call_row(
        case,
        case_label,
        True,
        t0,
        extra={
            "error_kind": error_kind,
            "error_message": error_message,
            "publish_state": state,
        },
    ))


def _build(stack_root):
    wb.build_workspace_base(
        workspace_root=WORKSPACE_ROOT,
        layer_stack_root=stack_root,
    )


def _special_file(stack_root):
    fifo = seed / "special.fifo"
    try:
        os.mkfifo(fifo)
        _build(stack_root)
    finally:
        try:
            fifo.unlink()
        except FileNotFoundError:
            pass


def _file_disappears(stack_root):
    target = seed / "disappears.txt"
    target.write_text("before\n", encoding="utf-8")
    original = wb._write_base_layer

    def wrapped(root, entries):
        target.unlink()
        return original(root, entries)

    wb._write_base_layer = wrapped
    try:
        _build(stack_root)
    finally:
        wb._write_base_layer = original


def _file_content_changes(stack_root):
    target = seed / "changes.txt"
    target.write_text("before\n", encoding="utf-8")
    original = wb._write_base_layer

    def wrapped(root, entries):
        target.write_text("after\n", encoding="utf-8")
        return original(root, entries)

    wb._write_base_layer = wrapped
    try:
        _build(stack_root)
    finally:
        wb._write_base_layer = original


def _new_file_appears(stack_root):
    target = seed / "appears-during-import.txt"
    target.unlink(missing_ok=True)
    original = wb._write_base_layer

    def wrapped(root, entries):
        manifest = original(root, entries)
        target.write_text("new\n", encoding="utf-8")
        return manifest

    wb._write_base_layer = wrapped
    try:
        _build(stack_root)
    finally:
        wb._write_base_layer = original
        target.unlink(missing_ok=True)


def _stack_inside_workspace(stack_root):
    del stack_root
    wb.build_workspace_base(
        workspace_root=WORKSPACE_ROOT,
        layer_stack_root=WORKSPACE_ROOT / "phase01-stack-inside-workspace",
    )


def _existing_manifest_or_binding(stack_root):
    _build(stack_root)
    wb.build_workspace_base(
        workspace_root=WORKSPACE_ROOT,
        layer_stack_root=stack_root,
    )


_record_failure("special_file", _special_file)
_record_failure("file_disappears_during_import", _file_disappears)
_record_failure("file_content_changes_during_import", _file_content_changes)
_record_failure("new_file_appears_during_import", _new_file_appears)
_record_failure("layer_stack_root_inside_workspace", _stack_inside_workspace)
_record_failure("existing_manifest_or_binding", _existing_manifest_or_binding, expect_empty=False)

assert all(row["success"] for row in rows)
summary = _base_summary(
    case,
    summary_binding,
    workspace_inv,
    summary_timings,
    pass_bars={
        "special_file_fail_closed": True,
        "file_disappears_fail_closed": True,
        "file_content_changes_fail_closed": True,
        "new_file_appears_fail_closed": True,
        "stack_root_inside_workspace_rejected": True,
        "existing_binding_rejected": True,
    },
)
_emit_workspace_payload(
    label,
    started,
    summary,
    rows,
    extra={"failure_rows": rows},
)
"""


async def test_base_import_failures_do_not_publish_partial_workspace_truth(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    payload = await run_workspace_base_probe(
        workspace_base_sandbox,
        _FAILURE_SAFETY_BODY,
        label="workspace_base.import_failure_safety",
        timeout=240,
    )
    rows = payload["failure_rows"]
    assert len(rows) == 6
    assert all(row["success"] for row in rows)
    artifact = write_jsonl_artifact(
        case="base_import_failure_safety",
        summary=payload["summary"],
        rows=payload["rows"],
    )
    print(f"\n[phase01:base_import_failure_safety] artifact={artifact}")
