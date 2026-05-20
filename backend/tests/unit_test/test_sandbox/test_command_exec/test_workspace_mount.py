"""Workspace replacement mount behavior tests."""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

import sandbox.execution.runner as command_runner
from sandbox.execution.contract import CommandExecRequest
from sandbox.execution.contract import MountMode
from sandbox.execution.contract import ShellProcessResult
from sandbox.execution.contract import OverlayLayout
from sandbox.execution.overlay import kernel_mount
from sandbox.execution.overlay.capture import walk_upperdir
from sandbox.execution.overlay.change_synthesis import synthesize_writes
from sandbox.execution.strategies.copy_backed import (
    CopyBackedStrategy,
    rewrite_declared_workspace_refs,
)
from sandbox.execution.strategies.namespace import (
    NAMESPACE_CONTROL_REF,
    NAMESPACE_FALLBACK_STRATEGY,
    NAMESPACE_INFRA_EXIT_CODE,
    PrivateNamespaceStrategy,
)


def test_copy_backed_mount_captures_only_workspace_changes(
    tmp_path: Path,
) -> None:
    lower = tmp_path / "lower"
    lower.mkdir()
    (lower / "input.txt").write_text("base\n", encoding="utf-8")
    outside = tmp_path / "outside.txt"
    spec = OverlayLayout(
        workspace_root="/testbed",
        base_repo=str(lower),
        writes=str(tmp_path / "upper"),
        kernel_scratch=str(tmp_path / "work"),
        scratch_root=str(tmp_path),
    )
    request = CommandExecRequest(
        request_id="req-1",
        workspace_ref=str(tmp_path / "stack"),
        workspace_root="/testbed",
        command=(
            "bash",
            "-lc",
            (
                "cat input.txt; "
                "mkdir -p generated; "
                "printf changed > generated/output.txt; "
                f"printf outside > {outside}"
            ),
        ),
    )
    timings: dict[str, float] = {}

    process = command_runner.run_workspace_replaced_command(
        spec=spec,
        request=request,
        run_dir=tmp_path / "run",
        timings=timings,
        strategies=(CopyBackedStrategy(),),
    )
    synthesize_writes(
        merged=Path(process.mounted_workspace_root),
        base_repo=Path(spec.base_repo),
        into=Path(spec.writes),
        timings=timings,
    )
    changes = walk_upperdir(spec.writes, timings=timings)

    assert process.exit_code == 0
    assert Path(process.stdout_ref).read_text(encoding="utf-8") == "base\n"
    assert [change.path for change in changes] == ["generated/output.txt"]
    assert outside.read_text(encoding="utf-8") == "outside"
    assert "command_exec.mount_workspace_s" in timings
    assert "command_exec.run_command_s" in timings
    assert "cmd.exec.user_s" in timings
    assert "cmd.exec.system_s" in timings


def test_copy_backed_mount_rewrites_absolute_workspace_references(
    tmp_path: Path,
) -> None:
    lower = tmp_path / "lower"
    lower.mkdir()
    spec = OverlayLayout(
        workspace_root="/testbed",
        base_repo=str(lower),
        writes=str(tmp_path / "upper"),
        kernel_scratch=str(tmp_path / "work"),
        scratch_root=str(tmp_path),
    )
    request = CommandExecRequest(
        request_id="req-1",
        workspace_ref=str(tmp_path / "stack"),
        workspace_root="/testbed",
        command=("bash", "-lc", 'printf captured > "/testbed/out.txt"'),
    )
    timings: dict[str, float] = {}

    process = command_runner.run_workspace_replaced_command(
        spec=spec,
        request=request,
        run_dir=tmp_path / "run",
        timings=timings,
        strategies=(CopyBackedStrategy(),),
    )
    synthesize_writes(
        merged=Path(process.mounted_workspace_root),
        base_repo=Path(spec.base_repo),
        into=Path(spec.writes),
        timings=timings,
    )
    changes = walk_upperdir(spec.writes, timings=timings)

    assert process.exit_code == 0
    assert (
        Path(process.mounted_workspace_root) / "out.txt"
    ).read_text(encoding="utf-8") == "captured"
    assert [change.path for change in changes] == ["out.txt"]


def test_copy_backed_mount_rewrites_workspace_env_values(
    tmp_path: Path,
) -> None:
    lower = tmp_path / "lower"
    lower.mkdir()
    spec = OverlayLayout(
        workspace_root="/testbed",
        base_repo=str(lower),
        writes=str(tmp_path / "upper"),
        kernel_scratch=str(tmp_path / "work"),
        scratch_root=str(tmp_path),
    )
    request = CommandExecRequest(
        request_id="req-1",
        workspace_ref=str(tmp_path / "stack"),
        workspace_root="/testbed",
        command=("bash", "-lc", "printf env > \"$WORKSPACE_DIR/env.txt\""),
        env={"WORKSPACE_DIR": "/testbed"},
    )

    process = command_runner.run_workspace_replaced_command(
        spec=spec,
        request=request,
        run_dir=tmp_path / "run",
        timings={},
        strategies=(CopyBackedStrategy(),),
    )

    assert process.exit_code == 0
    assert (
        Path(process.mounted_workspace_root) / "env.txt"
    ).read_text(encoding="utf-8") == "env"


def test_namespace_mount_failure_falls_back_to_copy_backed(
    tmp_path: Path,
) -> None:
    lower = tmp_path / "lower"
    lower.mkdir()
    stderr_ref = tmp_path / "run" / "stderr.bin"

    spec = OverlayLayout(
        workspace_root="/testbed",
        base_repo=str(lower),
        writes=str(tmp_path / "upper"),
        kernel_scratch=str(tmp_path / "work"),
        scratch_root=str(tmp_path),
    )
    request = CommandExecRequest(
        request_id="req-1",
        workspace_ref=str(tmp_path / "stack"),
        workspace_root="/testbed",
        command=("bash", "-lc", "printf ok > /testbed/out.txt"),
    )

    class FakePrivateNamespaceStrategy:
        name = "private_namespace"

        def is_available(self) -> bool:
            return True

        def run(self, **_: object) -> ShellProcessResult:
            run_dir = tmp_path / "run"
            stderr_ref.parent.mkdir(parents=True, exist_ok=True)
            stderr_ref.write_text(
                '{"detail":"overlay rejected mount","error_kind":"mount_failed"}\n',
                encoding="utf-8",
            )
            (run_dir / NAMESPACE_CONTROL_REF).write_text(
                (
                    '{"detail":"overlay rejected mount",'
                    '"error_kind":"mount_failed",'
                    f'"fallback":"{NAMESPACE_FALLBACK_STRATEGY}"'
                    "}\n"
                ),
                encoding="utf-8",
            )
            return ShellProcessResult(
                exit_code=NAMESPACE_INFRA_EXIT_CODE,
                stdout_ref=str(run_dir / "stdout.bin"),
                stderr_ref=str(stderr_ref),
                mounted_workspace_root="/testbed",
                mount_mode=MountMode.PRIVATE_NAMESPACE,
            )

        def should_fall_back(
            self,
            result: ShellProcessResult,
            *,
            run_dir: Path,
        ) -> bool:
            return PrivateNamespaceStrategy(available=True).should_fall_back(
                result,
                run_dir=run_dir,
            )

    timings: dict[str, float] = {}

    process = command_runner.run_workspace_replaced_command(
        spec=spec,
        request=request,
        run_dir=tmp_path / "run",
        timings=timings,
        strategies=(FakePrivateNamespaceStrategy(), CopyBackedStrategy()),
    )

    assert process.exit_code == 0
    assert process.mount_mode == MountMode.COPY_BACKED
    assert (Path(process.mounted_workspace_root) / "out.txt").read_text(
        encoding="utf-8"
    ) == "ok"
    assert timings["command_exec.private_mount_fallback"] == 1.0


def test_namespace_mount_failure_requires_control_sidecar(tmp_path: Path) -> None:
    stderr_ref = tmp_path / "run" / "stderr.bin"
    stderr_ref.parent.mkdir(parents=True)
    stderr_ref.write_text(
        '{"detail":"user output","error_kind":"mount_failed"}\n',
        encoding="utf-8",
    )
    process = ShellProcessResult(
        exit_code=NAMESPACE_INFRA_EXIT_CODE,
        stdout_ref=str(tmp_path / "run" / "stdout.bin"),
        stderr_ref=str(stderr_ref),
        mounted_workspace_root="/testbed",
        mount_mode=MountMode.PRIVATE_NAMESPACE,
    )

    assert PrivateNamespaceStrategy(available=True).should_fall_back(
        process,
        run_dir=tmp_path / "run",
    ) is False


def test_workspace_rewrite_rewrites_quoted_shell_paths() -> None:
    rewritten = rewrite_declared_workspace_refs(
        ("bash", "-lc", 'cat "/testbed/file.txt"; cat /testbed/other.txt'),
        workspace_root="/testbed",
        mounted_workspace_root="/tmp/run/workspace",
    )

    assert rewritten[-1] == (
        'cat "/tmp/run/workspace/file.txt"; cat /tmp/run/workspace/other.txt'
    )


def test_mount_spec_rejects_paths_outside_scratch_root(tmp_path: Path) -> None:
    with pytest.raises(
        ValueError,
        match="writes must be strictly under scratch_root",
    ):
        OverlayLayout(
            workspace_root="/testbed",
            base_repo=str(tmp_path / "lower"),
            writes="/tmp/not-owned",
            kernel_scratch=str(tmp_path / "work"),
            scratch_root=str(tmp_path),
        )


def test_mount_spec_rejects_scratch_root_itself(tmp_path: Path) -> None:
    with pytest.raises(
        ValueError,
        match="writes must be strictly under scratch_root",
    ):
        OverlayLayout(
            workspace_root="/testbed",
            base_repo=str(tmp_path / "lower"),
            writes=str(tmp_path),
            kernel_scratch=str(tmp_path / "work"),
            scratch_root=str(tmp_path),
        )


def test_mount_spec_rejects_duplicate_mount_paths(tmp_path: Path) -> None:
    shared = tmp_path / "same"
    with pytest.raises(ValueError, match="kernel_scratch must be distinct from writes"):
        OverlayLayout(
            workspace_root="/testbed",
            base_repo=str(tmp_path / "lower"),
            writes=str(shared),
            kernel_scratch=str(shared),
            scratch_root=str(tmp_path),
        )


def test_namespace_mount_validation_returns_fd_backed_paths(tmp_path: Path) -> None:
    workspace_root = tmp_path / "workspace"
    layer1 = tmp_path / "layer1"
    layer2 = tmp_path / "layer2"
    upperdir = tmp_path / "upper"
    workdir = tmp_path / "work"
    workspace_root.mkdir()
    layer1.mkdir()
    layer2.mkdir()

    inputs = kernel_mount.validate_mount_inputs(
        workspace_root=workspace_root,
        layer_paths=(layer1, layer2),
        upperdir=upperdir,
        workdir=workdir,
    )
    try:
        assert inputs.workspace_root.as_posix().startswith("/proc/self/fd/")
        assert len(inputs.layer_paths) == 2
        assert all(p.as_posix().startswith("/proc/self/fd/") for p in inputs.layer_paths)
        assert inputs.upperdir.as_posix().startswith("/proc/self/fd/")
        assert inputs.workdir.as_posix().startswith("/proc/self/fd/")
    finally:
        inputs.close()


def test_namespace_mount_passes_fd_paths_to_new_mount_api(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace_root = tmp_path / "workspace"
    layer1 = tmp_path / "layer1"
    upperdir = tmp_path / "upper"
    workdir = tmp_path / "work"
    workspace_root.mkdir()
    layer1.mkdir()
    inputs = kernel_mount.validate_mount_inputs(
        workspace_root=workspace_root,
        layer_paths=(layer1,),
        upperdir=upperdir,
        workdir=workdir,
    )
    syscalls: list[tuple[object, ...]] = []

    import ctypes

    mock_libc = type("MockLibc", (), {})()

    def fake_syscall(*args: object) -> int:
        syscalls.append(args)
        return 0

    mock_libc.syscall = fake_syscall
    monkeypatch.setattr(kernel_mount, "_get_libc", lambda: mock_libc)

    try:
        kernel_mount.mount_overlay(
            workspace_root=inputs.workspace_root,
            layer_paths=inputs.layer_paths,
            upperdir=inputs.upperdir,
            workdir=inputs.workdir,
            pass_fds=inputs.fds,
        )
    finally:
        inputs.close()

    syscall_numbers = [call[0] for call in syscalls]
    from sandbox.execution.overlay.new_mount_api import SYS_fsopen, SYS_fsconfig, SYS_fsmount, SYS_move_mount
    assert SYS_fsopen in syscall_numbers
    assert SYS_fsconfig in syscall_numbers


def test_namespace_helper_module_path_in_strategy_argv_is_importable() -> None:
    """Pin the `python -m` module path so a rename of the child file fails fast.

    On macOS the namespace strategy cannot actually run (needs Linux user
    namespaces), so the argv-string vs filesystem agreement is our only
    pre-merge check that the path the strategy hands to ``unshare`` actually
    resolves to a real module.
    """
    import importlib.util
    import inspect

    from sandbox.execution.strategies import namespace as namespace_strategy

    source = inspect.getsource(namespace_strategy.PrivateNamespaceStrategy.run)
    assert '"sandbox.execution.strategies.namespace_child"' in source
    assert (
        importlib.util.find_spec("sandbox.execution.strategies.namespace_child")
        is not None
    )
