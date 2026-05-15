"""Workspace replacement mount behavior tests."""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

import sandbox.execution.orchestrator as command_runner
from sandbox.execution.contract import CommandExecRequest
from sandbox.execution.contract import MountMode
from sandbox.execution.contract import ShellProcessResult
from sandbox.execution.contract import WorkspaceReplacementMountSpec
from sandbox.execution import entrypoints as namespace_helper
from sandbox.execution.overlay_capture import capture_changes
from sandbox.execution.strategy_copy_backed import (
    CopyBackedStrategy,
    rewrite_declared_workspace_refs,
)
from sandbox.execution.strategy_private_namespace import (
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
    spec = WorkspaceReplacementMountSpec(
        workspace_root="/testbed",
        lowerdir=str(lower),
        upperdir=str(tmp_path / "upper"),
        workdir=str(tmp_path / "work"),
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
    changes = capture_changes(
        spec.upperdir,
        lowerdir=spec.lowerdir,
        workspace_root=process.mounted_workspace_root,
        timings=timings,
    )

    assert process.exit_code == 0
    assert Path(process.stdout_ref).read_text(encoding="utf-8") == "base\n"
    assert [change.path for change in changes] == ["generated/output.txt"]
    assert outside.read_text(encoding="utf-8") == "outside"
    assert "command_exec.mount_workspace_s" in timings
    assert "command_exec.run_command_s" in timings


def test_copy_backed_mount_rewrites_absolute_workspace_references(
    tmp_path: Path,
) -> None:
    lower = tmp_path / "lower"
    lower.mkdir()
    spec = WorkspaceReplacementMountSpec(
        workspace_root="/testbed",
        lowerdir=str(lower),
        upperdir=str(tmp_path / "upper"),
        workdir=str(tmp_path / "work"),
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
    changes = capture_changes(
        spec.upperdir,
        lowerdir=spec.lowerdir,
        workspace_root=process.mounted_workspace_root,
        timings=timings,
    )

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
    spec = WorkspaceReplacementMountSpec(
        workspace_root="/testbed",
        lowerdir=str(lower),
        upperdir=str(tmp_path / "upper"),
        workdir=str(tmp_path / "work"),
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

    spec = WorkspaceReplacementMountSpec(
        workspace_root="/testbed",
        lowerdir=str(lower),
        upperdir=str(tmp_path / "upper"),
        workdir=str(tmp_path / "work"),
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

        def is_recoverable_failure(
            self,
            result: ShellProcessResult,
            *,
            run_dir: Path,
        ) -> bool:
            return PrivateNamespaceStrategy(available=True).is_recoverable_failure(
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

    assert PrivateNamespaceStrategy(available=True).is_recoverable_failure(
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
        match="upperdir must be strictly under scratch_root",
    ):
        WorkspaceReplacementMountSpec(
            workspace_root="/testbed",
            lowerdir=str(tmp_path / "lower"),
            upperdir="/tmp/not-owned",
            workdir=str(tmp_path / "work"),
            scratch_root=str(tmp_path),
        )


def test_mount_spec_rejects_scratch_root_itself(tmp_path: Path) -> None:
    with pytest.raises(
        ValueError,
        match="upperdir must be strictly under scratch_root",
    ):
        WorkspaceReplacementMountSpec(
            workspace_root="/testbed",
            lowerdir=str(tmp_path / "lower"),
            upperdir=str(tmp_path),
            workdir=str(tmp_path / "work"),
            scratch_root=str(tmp_path),
        )


def test_mount_spec_rejects_duplicate_mount_paths(tmp_path: Path) -> None:
    shared = tmp_path / "same"
    with pytest.raises(ValueError, match="workdir must be distinct from upperdir"):
        WorkspaceReplacementMountSpec(
            workspace_root="/testbed",
            lowerdir=str(tmp_path / "lower"),
            upperdir=str(shared),
            workdir=str(shared),
            scratch_root=str(tmp_path),
        )


def test_namespace_mount_validation_returns_fd_backed_paths(tmp_path: Path) -> None:
    workspace_root = tmp_path / "workspace"
    lowerdir = tmp_path / "lower"
    upperdir = tmp_path / "upper"
    workdir = tmp_path / "work"
    workspace_root.mkdir()
    lowerdir.mkdir()

    inputs = namespace_helper._validate_mount_inputs(
        workspace_root=workspace_root,
        lowerdir=lowerdir,
        upperdir=upperdir,
        workdir=workdir,
    )
    try:
        assert inputs.workspace_root.as_posix().startswith("/proc/self/fd/")
        assert inputs.lowerdir.as_posix().startswith("/proc/self/fd/")
        assert inputs.upperdir.as_posix().startswith("/proc/self/fd/")
        assert inputs.workdir.as_posix().startswith("/proc/self/fd/")
    finally:
        inputs.close()


def test_namespace_mount_passes_fd_paths_to_mount_subprocess(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace_root = tmp_path / "workspace"
    lowerdir = tmp_path / "lower"
    upperdir = tmp_path / "upper"
    workdir = tmp_path / "work"
    workspace_root.mkdir()
    lowerdir.mkdir()
    inputs = namespace_helper._validate_mount_inputs(
        workspace_root=workspace_root,
        lowerdir=lowerdir,
        upperdir=upperdir,
        workdir=workdir,
    )
    calls: list[dict[str, object]] = []

    def fake_run(*args: object, **kwargs: object) -> subprocess.CompletedProcess:
        calls.append({"args": args, "kwargs": kwargs})
        return subprocess.CompletedProcess(args=args, returncode=0)

    monkeypatch.setattr(namespace_helper.subprocess, "run", fake_run)
    try:
        namespace_helper._mount_overlay(
            workspace_root=inputs.workspace_root,
            lowerdir=inputs.lowerdir,
            upperdir=inputs.upperdir,
            workdir=inputs.workdir,
            pass_fds=inputs.fds,
        )
    finally:
        inputs.close()

    assert calls
    assert calls[0]["kwargs"]["pass_fds"] == inputs.fds
