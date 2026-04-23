"""Direct tests for Daytona pre-hook policy modules."""

from __future__ import annotations

import asyncio
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock

from code_intelligence.hashing import content_hash
from code_intelligence.types import SemanticFileChange, SemanticRenamePlan, SymbolInfo, SymbolKind
from tools.core.base import ToolExecutionContext
from tools.core.hooks import ToolHookRegistry
from tools.daytona_toolkit.delete_move_tool import (
    DaytonaDeleteFileInput,
    DaytonaMoveFileInput,
)
from tools.daytona_toolkit.hooks.prehook import (
    shell_destructive_git,
    shell_destructive_shell,
    shell_output_pipeline_policy,
    shell_package_mutation_policy,
    shell_stderr_suppression_policy,
    move_src_scope_deny,
    rename_scope_policy,
    repo_operation_guard,
    write_scope_deny,
)
from tools.daytona_toolkit.shell_tool import DaytonaShellInput
from tools.daytona_toolkit.rename_tool import DaytonaRenameSymbolsInput


def _ctx(metadata: dict | None = None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/ws"), metadata=metadata or {})


def _coord_ctx(**extra: object) -> ToolExecutionContext:
    metadata = {
        "agent_name": "developer",
        "repo_root": "/ws",
        "daytona_cwd": "/ws",
    }
    metadata.update(extra)
    return _ctx(metadata)


def _run(awaitable):
    return asyncio.run(awaitable)


def _rename_plan(*paths: str, new_name: str = "bar") -> SemanticRenamePlan:
    base = "def foo(): pass\n"
    return SemanticRenamePlan(
        new_name=new_name,
        origin=(paths[0] if paths else "/ws/a.py", 1, 4),
        changes=tuple(
            SemanticFileChange(
                file_path=path,
                base_content=base,
                base_hash=content_hash(base),
                final_content=f"def {new_name}(): pass\n",
            )
            for path in paths
        ),
    )


def _symbol(path: str = "/ws/a.py") -> SymbolInfo:
    return SymbolInfo(
        name="foo",
        kind=SymbolKind.FUNCTION,
        file_path=path,
        line=1,
        character=4,
        signature="def foo()",
    )


def _rename_service(plan: SemanticRenamePlan):
    return SimpleNamespace(
        symbol_index=SimpleNamespace(
            ensure_built=MagicMock(),
            find=MagicMock(return_value=[_symbol()]),
        ),
        rename_symbol_plan=MagicMock(return_value=plan),
    )


def test_repo_guard_blocks_delete_of_repo_root() -> None:
    ctx = _coord_ctx()
    args = DaytonaDeleteFileInput(path="/ws")

    outcome = _run(repo_operation_guard.hook("daytona_delete_file", args, ctx))

    assert outcome.has_error is True
    assert "repo root" in (outcome.error_message or "")


def test_repo_guard_blocks_nested_move() -> None:
    ctx = _coord_ctx()
    args = DaytonaMoveFileInput(src_path="/ws/pkg", target_path="/ws/pkg/sub")

    outcome = _run(repo_operation_guard.hook("daytona_move_file", args, ctx))

    assert outcome.has_error is True
    assert "inside source" in (outcome.error_message or "")


def test_repo_guard_blocks_parent_escape_delete() -> None:
    ctx = _coord_ctx()
    args = DaytonaDeleteFileInput(path="../outside.py")

    outcome = _run(repo_operation_guard.hook("daytona_delete_file", args, ctx))

    assert outcome.has_error is True
    assert "outside repo root" in (outcome.error_message or "")


def test_repo_guard_blocks_parent_escape_move_destination() -> None:
    ctx = _coord_ctx()
    args = DaytonaMoveFileInput(src_path="/ws/pkg/a.py", target_path="../outside.py")

    outcome = _run(repo_operation_guard.hook("daytona_move_file", args, ctx))

    assert outcome.has_error is True
    assert "outside repo root" in (outcome.error_message or "")


def test_write_scope_deny_checks_folder_members_before_delete_body() -> None:
    svc = MagicMock()
    svc.list_folder_files.return_value = ["/ws/pkg/a.py", "/ws/other/b.py"]
    ctx = _coord_ctx(write_scope=["pkg/"], ci_service=svc)
    args = DaytonaDeleteFileInput(path="/ws/pkg", is_folder=True)

    outcome = _run(write_scope_deny.hook("daytona_delete_file", args, ctx))

    assert outcome.has_error is True
    assert "folder members" in (outcome.error_message or "")
    assert "/ws/other/b.py" in (outcome.error_message or "")
    assert "/ws/pkg/a.py" not in (outcome.error_message or "")
    svc.list_folder_files.assert_called_once_with("/ws/pkg")


def test_write_scope_deny_blocks_test_file_folder_members() -> None:
    svc = MagicMock()
    svc.list_folder_files.return_value = ["/ws/pkg/tests/test_a.py"]
    ctx = _coord_ctx(write_scope=["pkg/"], ci_service=svc)
    args = DaytonaDeleteFileInput(path="/ws/pkg", is_folder=True)

    outcome = _run(write_scope_deny.hook("daytona_delete_file", args, ctx))

    assert outcome.has_error is True
    assert "BLOCKED_TEST_FILE_EDIT" in (outcome.error_message or "")
    assert "/ws/pkg/tests/test_a.py" in (outcome.error_message or "")


def test_write_scope_deny_blocks_test_file_folder_members_without_scope() -> None:
    svc = MagicMock()
    svc.list_folder_files.return_value = ["/ws/pkg/tests/test_a.py"]
    ctx = _coord_ctx(ci_service=svc)
    args = DaytonaDeleteFileInput(path="/ws/pkg", is_folder=True)

    outcome = _run(write_scope_deny.hook("daytona_delete_file", args, ctx))

    assert outcome.has_error is True
    assert "BLOCKED_TEST_FILE_EDIT" in (outcome.error_message or "")


def test_move_src_scope_deny_checks_folder_members_before_move_body() -> None:
    svc = MagicMock()
    svc.list_folder_files.return_value = ["/ws/pkg/a.py", "/ws/other/b.py"]
    ctx = _coord_ctx(write_scope=["pkg/"], ci_service=svc)
    args = DaytonaMoveFileInput(
        src_path="/ws/pkg",
        target_path="/ws/moved_pkg",
        is_folder=True,
    )

    outcome = _run(move_src_scope_deny.hook("daytona_move_file", args, ctx))

    assert outcome.has_error is True
    assert "folder members" in (outcome.error_message or "")
    assert "/ws/other/b.py" in (outcome.error_message or "")
    assert "/ws/pkg/a.py" not in (outcome.error_message or "")
    svc.list_folder_files.assert_called_once_with("/ws/pkg")


def test_move_src_scope_deny_blocks_test_file_folder_members() -> None:
    svc = MagicMock()
    svc.list_folder_files.return_value = ["/ws/pkg/tests/test_a.py"]
    ctx = _coord_ctx(write_scope=["pkg/"], ci_service=svc)
    args = DaytonaMoveFileInput(
        src_path="/ws/pkg",
        target_path="/ws/moved_pkg",
        is_folder=True,
    )

    outcome = _run(move_src_scope_deny.hook("daytona_move_file", args, ctx))

    assert outcome.has_error is True
    assert "BLOCKED_TEST_FILE_EDIT" in (outcome.error_message or "")
    assert "/ws/pkg/tests/test_a.py" in (outcome.error_message or "")


def test_rename_scope_policy_caches_allowed_plan() -> None:
    plan = _rename_plan("/ws/pkg/a.py")
    svc = _rename_service(plan)
    ctx = _coord_ctx(ci_service=svc, write_scope=["pkg/"])
    args = DaytonaRenameSymbolsInput(symbol="foo", new_name="bar")

    outcome = _run(rename_scope_policy.hook("daytona_rename_symbol", args, ctx))

    assert outcome.has_error is False
    cached = ctx.metadata.get("_daytona_rename_preplan")
    assert cached["plan"] is plan
    assert cached["resolved_path"] == "/ws/a.py"


def test_rename_scope_policy_blocks_planned_out_of_scope_file() -> None:
    svc = _rename_service(_rename_plan("/ws/pkg/a.py", "/ws/other/b.py"))
    ctx = _coord_ctx(ci_service=svc, write_scope=["pkg/"])
    args = DaytonaRenameSymbolsInput(symbol="foo", new_name="bar")

    outcome = _run(rename_scope_policy.hook("daytona_rename_symbol", args, ctx))

    assert outcome.has_error is True
    assert "daytona_rename_symbol blocked by write-scope policy" in (
        outcome.error_message or ""
    )
    assert "/ws/other/b.py" in (outcome.error_message or "")
    assert "_daytona_rename_preplan" not in ctx.metadata


def test_shell_stderr_suppression_policy_blocks_dev_null_stderr() -> None:
    ctx = _ctx()
    args = DaytonaShellInput(command="find . -name '*.py' 2>/dev/null|head -1")

    outcome = _run(
        shell_stderr_suppression_policy.hook("daytona_shell", args, ctx)
    )

    assert outcome.has_error is True
    assert "daytona_shell commands must preserve stderr" in (outcome.error_message or "")
    assert "2>/dev/null" in (outcome.error_message or "")


def test_shell_output_pipeline_policy_sanitizes_shell_command() -> None:
    ctx = _ctx()
    args = DaytonaShellInput(
        command="cd /testbed && pytest tests/unit/test_x.py -q 2>&1 | head -200"
    )

    outcome = _run(
        shell_output_pipeline_policy.hook("daytona_shell", args, ctx)
    )

    assert outcome.has_error is False
    assert outcome.tool_input is not None
    assert outcome.tool_input.command == "pytest tests/unit/test_x.py -q"
    assert "sanitized daytona_shell command" in outcome.advisories[0]


def test_shell_output_pipeline_policy_sanitizes_head_tail_command() -> None:
    ctx = _ctx()
    args = DaytonaShellInput(command="tail -n 40 logs/test.log > /tmp/out")

    outcome = _run(
        shell_output_pipeline_policy.hook("daytona_shell", args, ctx)
    )

    assert outcome.has_error is False
    assert outcome.tool_input is not None
    assert outcome.tool_input.command == "cat logs/test.log"


def test_shell_output_pipeline_policy_sanitizes_command_substitution_pipeline() -> None:
    ctx = _ctx()
    args = DaytonaShellInput(
        command="files=$(find . -name '*.py' 2>/dev/null | head -1); printf '%s\\n' \"$files\""
    )

    outcome = _run(
        shell_output_pipeline_policy.hook("daytona_shell", args, ctx)
    )

    assert outcome.has_error is False
    assert outcome.tool_input is not None
    assert (
        outcome.tool_input.command
        == "files=$(find . -name '*.py'); printf '%s\\n' \"$files\""
    )


def test_shell_output_pipeline_policy_ignores_arithmetic_expansion() -> None:
    ctx = _ctx()
    args = DaytonaShellInput(command='count=$((1 + 2)); echo "$count"')

    outcome = _run(
        shell_output_pipeline_policy.hook("daytona_shell", args, ctx)
    )

    assert outcome.has_error is False
    assert outcome.tool_input is None
    assert outcome.advisories == ()


def test_shell_output_pipeline_policy_keeps_arithmetic_inside_substitution() -> None:
    ctx = _ctx()
    args = DaytonaShellInput(command='value=$(echo $((1 + 2)) | tail -1); echo "$value"')

    outcome = _run(
        shell_output_pipeline_policy.hook("daytona_shell", args, ctx)
    )

    assert outcome.has_error is False
    assert outcome.tool_input is not None
    assert outcome.tool_input.command == 'value=$(echo $((1 + 2))); echo "$value"'




def test_shell_destructive_git_blocks_common_clean_forms() -> None:
    for command in ("git clean -xdf", "git clean -x -d -f"):
        assert shell_destructive_git.destructive_git_command_error(command) is not None


def test_shell_destructive_git_blocks_metadata_mutation_commands() -> None:
    commands = [
        "git add dask/core.py",
        "git -C /testbed update-index --refresh",
        "git read-tree HEAD",
        "git apply --cached /tmp/patch.diff",
        "git apply /tmp/patch.diff",
        "git restore --staged dask/core.py",
        "command git commit -m repair",
    ]

    for command in commands:
        err = shell_destructive_git.destructive_git_command_error(command)
        assert err is not None, command
        assert "git mutation commands" in err






def test_shell_package_mutation_policy_blocks_package_manager_mutations() -> None:
    ctx = _ctx()
    commands = [
        "pip install ujson -q",
        "pip3 install ujson -q",
        "python -m pip install pandas",
        "python3.11 -u -m pip install pandas",
        "uv add pandas",
        "uv sync --extra dev",
        "uv run pip install ujson",
        "conda install pandas -y",
        "sudo apt install libhdf5-dev",
        "env -u PIP_INDEX_URL pip install ujson",
        "npm install",
        "pnpm add vite",
        "yarn add react",
        "poetry add pytest",
        "(pip install ujson)",
        "version=$(pip install ujson)",
    ]

    for command in commands:
        args = DaytonaShellInput(command=command)
        outcome = _run(
            shell_package_mutation_policy.hook("daytona_shell", args, ctx)
        )
        assert outcome.has_error is True, command
        assert "package and environment mutation commands are forbidden" in (
            outcome.error_message or ""
        )




def test_shell_package_mutation_policy_allows_read_only_or_quoted_mentions() -> None:
    ctx = _ctx()
    commands = [
        "pip list",
        "uv run pytest -q",
        "npm run build",
        "pnpm test",
        "yarn test",
        "poetry run pytest",
        "python -c \"print('pip install ujson')\"",
        "printf '%s\\n' 'npm install'",
    ]

    for command in commands:
        args = DaytonaShellInput(command=command)
        outcome = _run(
            shell_package_mutation_policy.hook("daytona_shell", args, ctx)
        )
        assert outcome.has_error is False, command


def test_shell_destructive_git_allows_clean_dry_run() -> None:
    for command in ("git clean -ndf", "git clean --dry-run -xdf"):
        assert shell_destructive_git.destructive_git_command_error(command) is None


def test_shell_destructive_git_allows_read_only_git_commands() -> None:
    commands = [
        "git status --short",
        "git diff --cached",
        "git show HEAD:README.md",
        "git ls-files",
        "git merge-base HEAD origin/main",
        "git apply --check /tmp/patch.diff",
        "git -C /testbed status --short",
    ]

    for command in commands:
        assert shell_destructive_git.destructive_git_command_error(command) is None, command


def test_shell_stderr_suppression_policy_blocks_equivalent_forms() -> None:
    ctx = _coord_ctx(team_run_id="run-1", work_item_id="task-1")
    args_list = [
        DaytonaShellInput(command="find . -name '*.py' 2> /dev/null"),
        DaytonaShellInput(command="pytest 2>>/dev/null"),
        DaytonaShellInput(command="command -v rg >/dev/null 2>&1"),
        DaytonaShellInput(command="optional-probe &>/dev/null"),
        DaytonaShellInput(command="pytest 2>&-"),
    ]

    for args in args_list:
        outcome = _run(
            shell_stderr_suppression_policy.hook("daytona_shell", args, ctx)
        )
        assert outcome.has_error is True, args


def test_shell_stderr_suppression_policy_ignores_quoted_text_and_plain_merge() -> None:
    ctx = _coord_ctx(team_run_id="run-1", work_item_id="task-1")
    args_list = [
        DaytonaShellInput(command="python -c \"print('2>/dev/null')\""),
        DaytonaShellInput(command="printf '%s\\n' '2>/dev/null'"),
        DaytonaShellInput(command="pytest 2>&1"),
        DaytonaShellInput(command="pytest 2>/tmp/errors.log"),
    ]

    for args in args_list:
        outcome = _run(
            shell_stderr_suppression_policy.hook("daytona_shell", args, ctx)
        )
        assert outcome.has_error is False, args


def test_new_pre_hooks_register_once() -> None:
    registry = ToolHookRegistry()

    repo_operation_guard.register(registry)
    repo_operation_guard.register(registry)
    rename_scope_policy.register(registry)
    rename_scope_policy.register(registry)
    shell_package_mutation_policy.register(registry)
    shell_package_mutation_policy.register(registry)
    shell_stderr_suppression_policy.register(registry)
    shell_stderr_suppression_policy.register(registry)

    assert len(registry.matching("daytona_delete_file", "pre")) == 1
    assert len(registry.matching("daytona_move_file", "pre")) == 1
    assert len(registry.matching("daytona_rename_symbol", "pre")) == 1
    assert len(registry.matching("daytona_shell", "pre")) == 2
