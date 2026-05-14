"""Environment isolation tests for ``overlay.namespace.command.run_user_command``.

Host environment variables (secrets, tokens, etc.) must not
leak into the user command. Only an explicit minimal allow-list plus any
caller-supplied ``env`` should be visible to the child process.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.overlay import run_user_command


def _run(
    tmp_path: Path,
    *,
    command: tuple[str, ...],
    env: dict[str, str] | None = None,
) -> tuple[int, str, str]:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    stdout_ref = tmp_path / "stdout.bin"
    stderr_ref = tmp_path / "stderr.bin"
    result = run_user_command(
        command=command,
        workspace_root=workspace,
        cwd=".",
        env=dict(env or {}),
        timeout_seconds=10,
        stdout_ref=stdout_ref,
        stderr_ref=stderr_ref,
    )
    return (
        result.exit_code,
        Path(result.stdout_ref).read_text(encoding="utf-8"),
        Path(result.stderr_ref).read_text(encoding="utf-8"),
    )


def test_host_secrets_do_not_leak_into_user_command(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AWS_ACCESS_KEY_ID", "AKIA-leaked")
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-ant-leaked")
    monkeypatch.setenv("OPENAI_API_KEY", "sk-leaked")

    exit_code, stdout, _stderr = _run(
        tmp_path,
        command=(
            "sh",
            "-c",
            "printf '%s|%s|%s' "
            "\"${AWS_ACCESS_KEY_ID-unset}\" "
            "\"${ANTHROPIC_API_KEY-unset}\" "
            "\"${OPENAI_API_KEY-unset}\"",
        ),
    )

    assert exit_code == 0
    assert stdout == "unset|unset|unset"


def test_printenv_does_not_expose_host_secret(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AWS_ACCESS_KEY_ID", "AKIA-leaked")

    exit_code, stdout, _stderr = _run(
        tmp_path,
        command=("printenv", "AWS_ACCESS_KEY_ID"),
    )

    # printenv returns 1 when the requested var is unset; assert that and
    # confirm no value was printed.
    assert exit_code == 1
    assert stdout == ""


def test_caller_env_is_visible_to_user_command(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AWS_ACCESS_KEY_ID", "AKIA-leaked")

    exit_code, stdout, _stderr = _run(
        tmp_path,
        command=("sh", "-c", "printf '%s' \"$MY_VAR\""),
        env={"MY_VAR": "caller-value"},
    )

    assert exit_code == 0
    assert stdout == "caller-value"


def test_path_is_present_so_basic_commands_resolve(
    tmp_path: Path,
) -> None:
    # The minimal env must include PATH (or POSIX builtin sh resolution
    # must work) so callers can keep invoking commands like ``printf``,
    # ``sh``, ``printenv`` without explicitly supplying PATH every time.
    exit_code, stdout, _stderr = _run(
        tmp_path,
        command=("sh", "-c", "printf ok"),
    )

    assert exit_code == 0
    assert stdout == "ok"
