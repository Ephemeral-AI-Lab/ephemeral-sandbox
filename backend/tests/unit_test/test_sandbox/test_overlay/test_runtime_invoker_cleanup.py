"""Lifecycle tests for ``OverlayRuntimeInvoker`` / ``execute_request`` run_dir.

The bulk-growth intermediates inside ``run_dir``
(``lower/``, ``merged/``, ``work/``) must be reaped after the invocation.
Load-bearing artifacts (``upper/`` with ``content_path`` refs, ``stdout.bin``,
``stderr.bin``, ``result.json``) MUST remain readable after return because
``OverlayCapture`` carries references into them that downstream consumers
read post-invocation.
"""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack import WriteLayerChange, LayerStackManager
from sandbox.overlay import OverlayRuntimeInvoker, OverlayShellRequest


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def test_execute_request_removes_intermediate_dirs_but_keeps_outputs(
    tmp_path: Path,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="pkg/value.txt",
                source_path=_source(tmp_path, "value.txt", b"old\n"),
            )
        ]
    )
    runtime_root = tmp_path / "runtime"
    invoker = OverlayRuntimeInvoker(
        storage_root=manager.storage_root,
        runtime_root=runtime_root,
    )
    request = OverlayShellRequest(
        request_id="request-cleanup",
        command=(
            "bash",
            "-lc",
            "printf new > pkg/value.txt; printf out; printf err >&2",
        ),
        cwd=".",
        env={},
        timeout_seconds=10,
    )

    capture = invoker.invoke_sync(
        request=request, manifest=manager.read_active_manifest()
    )

    run_dirs = list(runtime_root.iterdir())
    assert len(run_dirs) == 1
    run_dir = run_dirs[0]

    # Bulk-growth intermediates must be gone.
    assert not (run_dir / "lower").exists()
    assert not (run_dir / "merged").exists()
    assert not (run_dir / "work").exists()

    # Load-bearing artifacts MUST still exist and be readable.
    assert (run_dir / "upper").is_dir()
    assert Path(capture.stdout_ref).read_text(encoding="utf-8") == "out"
    assert Path(capture.stderr_ref).read_text(encoding="utf-8") == "err"
    assert (run_dir / "result.json").is_file()

    # content_path refs into upper/ must still be readable.
    assert len(capture.changes) == 1
    change = capture.changes[0]
    assert change.content_path is not None
    assert Path(change.content_path).read_bytes() == b"new"


def test_execute_request_cleans_intermediate_dirs_even_on_nonzero_exit(
    tmp_path: Path,
) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="value.txt",
                source_path=_source(tmp_path, "value.txt", b"x\n"),
            )
        ]
    )
    runtime_root = tmp_path / "runtime"
    invoker = OverlayRuntimeInvoker(
        storage_root=manager.storage_root,
        runtime_root=runtime_root,
    )
    request = OverlayShellRequest(
        request_id="request-fail",
        command=("bash", "-lc", "exit 3"),
        cwd=".",
        env={},
        timeout_seconds=10,
    )

    capture = invoker.invoke_sync(
        request=request, manifest=manager.read_active_manifest()
    )

    run_dir = next(iter(runtime_root.iterdir()))
    assert not (run_dir / "lower").exists()
    assert not (run_dir / "merged").exists()
    assert not (run_dir / "work").exists()
    assert capture.exit_code == 3
