"""Scratch-worktree transactions for coordinated CodeAct mutations."""

from __future__ import annotations

import base64
import logging
import shlex
from dataclasses import dataclass, field
from pathlib import PurePosixPath
from typing import Any

from tools.core.base import ToolExecutionContext
from tools.core.ci_runtime import commit_ci_change_against_base, get_ci_service
from tools.daytona_toolkit._daytona_utils import (
    _extract_exit_code,
    _wrap_bash_command,
    _team_repo_write_error,
    _team_repo_write_warning,
    record_coordination_warning,
)

logger = logging.getLogger(__name__)


@dataclass
class RepoChange:
    path: str
    status: str
    base_content: str | None
    final_content: str | None
    message: str | None = None


@dataclass
class FileCommitResult:
    path: str
    status: str
    message: str | None = None


@dataclass
class CommitReport:
    committed: list[FileCommitResult] = field(default_factory=list)
    conflicts: list[FileCommitResult] = field(default_factory=list)
    errors: list[FileCommitResult] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)


@dataclass
class CodeActTransaction:
    repo_root: str
    scratch_root: str
    base_tree: str
    patch_path: str


async def _sandbox_exec(
    sandbox: Any,
    command: str,
    *,
    timeout: int = 120,
) -> tuple[int, str]:
    response = await sandbox.process.exec(_wrap_bash_command(command), timeout=timeout)
    stdout = getattr(response, "result", "") or ""
    fallback_exit_code = getattr(response, "exit_code", None)
    cleaned, exit_code = _extract_exit_code(stdout, fallback_exit_code=fallback_exit_code)
    return exit_code, cleaned


async def _sandbox_exec_checked(
    sandbox: Any,
    command: str,
    *,
    timeout: int = 120,
    error_prefix: str,
) -> str:
    exit_code, stdout = await _sandbox_exec(sandbox, command, timeout=timeout)
    if exit_code == 0:
        return stdout
    detail = stdout.strip() or f"exit {exit_code}"
    raise RuntimeError(f"{error_prefix}: {detail}")


def _decode_text(raw: bytes | str) -> tuple[str | None, str | None]:
    if isinstance(raw, bytes):
        try:
            return raw.decode("utf-8"), None
        except UnicodeDecodeError:
            return None, "Binary or non-UTF-8 content is not supported"
    return str(raw), None


def _parse_name_status(output: str) -> list[tuple[str, str]]:
    changes: list[tuple[str, str]] = []
    for line in output.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        parts = stripped.split(maxsplit=1)
        if len(parts) != 2:
            continue
        changes.append((parts[0], parts[1]))
    return changes


def _parse_binary_paths(output: str) -> set[str]:
    binary_paths: set[str] = set()
    for line in output.splitlines():
        stripped = line.rstrip()
        if not stripped:
            continue
        added, removed, path = stripped.split("\t", 2)
        if added == "-" or removed == "-":
            binary_paths.add(path)
    return binary_paths


async def _read_git_object_text(
    sandbox: Any,
    *,
    scratch_root: str,
    treeish: str,
    path: str,
) -> tuple[str | None, str | None]:
    object_spec = f"{treeish}:{path}"
    encoded = await _sandbox_exec_checked(
        sandbox,
        (
            f"git -C {shlex.quote(scratch_root)} show {shlex.quote(object_spec)} | "
            "python3 -c 'import base64, sys; "
            "sys.stdout.write(base64.b64encode(sys.stdin.buffer.read()).decode())'"
        ),
        timeout=120,
        error_prefix=f"Failed to read base content for {path}",
    )
    try:
        return base64.b64decode(encoded.encode("ascii")).decode("utf-8"), None
    except UnicodeDecodeError:
        return None, f"{path}: binary or non-UTF-8 content is not supported"


async def create_codeact_transaction(
    sandbox: Any,
    repo_root: str,
) -> CodeActTransaction:
    scratch_root = ""
    patch_path = ""
    try:
        scratch_root = (
            await _sandbox_exec_checked(
                sandbox,
                "mktemp -d /tmp/codeact-tx-XXXXXX",
                error_prefix="Failed to create scratch worktree directory",
            )
        ).strip()
        patch_path = (
            await _sandbox_exec_checked(
                sandbox,
                "mktemp /tmp/codeact-tx-patch-XXXXXX",
                error_prefix="Failed to create scratch patch file",
            )
        ).strip()

        await _sandbox_exec_checked(
            sandbox,
            (
                f"git -C {shlex.quote(repo_root)} worktree add --detach "
                f"{shlex.quote(scratch_root)} HEAD"
            ),
            timeout=180,
            error_prefix="Failed to create scratch git worktree",
        )

        seed_script = f"""
set -e
repo_root={shlex.quote(repo_root)}
scratch_root={shlex.quote(scratch_root)}
patch_path={shlex.quote(patch_path)}
git_index_path=$(git -C "$repo_root" rev-parse --path-format=absolute --git-path index)
seed_index=$(mktemp -u)
base_index=$(mktemp -u)
cleanup() {{
  rm -f "$seed_index" "$base_index"
}}
trap cleanup EXIT
if [ -f "$git_index_path" ]; then
  cp "$git_index_path" "$seed_index"
fi
GIT_INDEX_FILE="$seed_index" git -C "$repo_root" add -A -- .
GIT_INDEX_FILE="$seed_index" git -C "$repo_root" diff --cached --binary HEAD > "$patch_path"
if [ -s "$patch_path" ]; then
  git -C "$scratch_root" apply --binary "$patch_path"
fi
GIT_INDEX_FILE="$base_index" git -C "$scratch_root" add -A -- .
GIT_INDEX_FILE="$base_index" git -C "$scratch_root" write-tree
"""
        base_tree = (
            await _sandbox_exec_checked(
                sandbox,
                seed_script,
                timeout=180,
                error_prefix="Failed to seed scratch worktree",
            )
        ).strip()
        return CodeActTransaction(
            repo_root=repo_root,
            scratch_root=scratch_root,
            base_tree=base_tree,
            patch_path=patch_path,
        )
    except Exception as exc:
        if scratch_root:
            await cleanup_codeact_transaction(
                sandbox,
                CodeActTransaction(
                    repo_root=repo_root,
                    scratch_root=scratch_root,
                    base_tree="",
                    patch_path=patch_path,
                ),
            )
            logger.warning(
                "create_codeact_transaction failed (%s), cleaned up scratch_root=%s: %s",
                type(exc).__name__,
                scratch_root,
                exc,
            )
        raise


async def collect_transaction_changes(
    sandbox: Any,
    tx: CodeActTransaction,
) -> list[RepoChange]:
    diff_script = f"""
set -e
scratch_root={shlex.quote(tx.scratch_root)}
base_tree={shlex.quote(tx.base_tree)}
tmp_index=$(mktemp -u)
cleanup() {{
  rm -f "$tmp_index"
}}
trap cleanup EXIT
GIT_INDEX_FILE="$tmp_index" git -C "$scratch_root" add -A -- .
GIT_INDEX_FILE="$tmp_index" git -C "$scratch_root" diff --cached --name-status --no-renames "$base_tree"
"""
    numstat_script = f"""
set -e
scratch_root={shlex.quote(tx.scratch_root)}
base_tree={shlex.quote(tx.base_tree)}
tmp_index=$(mktemp -u)
cleanup() {{
  rm -f "$tmp_index"
}}
trap cleanup EXIT
GIT_INDEX_FILE="$tmp_index" git -C "$scratch_root" add -A -- .
GIT_INDEX_FILE="$tmp_index" git -C "$scratch_root" diff --cached --numstat --no-renames "$base_tree"
"""
    name_status_output = await _sandbox_exec_checked(
        sandbox,
        diff_script,
        timeout=180,
        error_prefix="Failed to collect scratch diff",
    )
    if not name_status_output.strip():
        return []
    binary_paths = _parse_binary_paths(
        await _sandbox_exec_checked(
            sandbox,
            numstat_script,
            timeout=180,
            error_prefix="Failed to collect scratch numstat",
        )
    )

    changes: list[RepoChange] = []
    for raw_status, path in _parse_name_status(name_status_output):
        status = {
            "M": "modified",
            "A": "created",
            "D": "deleted",
        }.get(raw_status, "unsupported")
        if status == "unsupported":
            changes.append(
                RepoChange(
                    path=path,
                    status="unsupported",
                    base_content=None,
                    final_content=None,
                    message=f"Unsupported git diff status: {raw_status}",
                )
            )
            continue

        if path in binary_paths:
            changes.append(
                RepoChange(
                    path=path,
                    status="unsupported",
                    base_content=None,
                    final_content=None,
                    message=f"{path}: binary repo changes are not supported",
                )
            )
            continue

        base_content = None
        final_content = None

        if status != "created":
            base_content, base_error = await _read_git_object_text(
                sandbox,
                scratch_root=tx.scratch_root,
                treeish=tx.base_tree,
                path=path,
            )
            if base_error is not None:
                changes.append(
                    RepoChange(
                        path=path,
                        status="unsupported",
                        base_content=None,
                        final_content=None,
                        message=base_error,
                    )
                )
                continue

        if status != "deleted":
            raw = await sandbox.fs.download_file(str(PurePosixPath(tx.scratch_root) / path))
            final_content, decode_error = _decode_text(raw)
            if decode_error is not None:
                changes.append(
                    RepoChange(
                        path=path,
                        status="unsupported",
                        base_content=None,
                        final_content=None,
                        message=f"{path}: {decode_error}",
                    )
                )
                continue

        changes.append(
            RepoChange(
                path=path,
                status=status,
                base_content=base_content,
                final_content=final_content,
            )
        )

    return changes


async def commit_transaction_changes(
    context: ToolExecutionContext,
    tx: CodeActTransaction,
    changes: list[RepoChange],
) -> CommitReport:
    report = CommitReport()
    if not changes:
        return report

    if get_ci_service(context) is None:
        report.errors.append(
            FileCommitResult(
                path=tx.repo_root,
                status="error",
                message="Coordinated CodeAct transaction requires CI service",
            )
        )
        return report

    for change in changes:
        if change.status == "unsupported":
            report.errors.append(
                FileCommitResult(
                    path=change.path,
                    status="unsupported",
                    message=change.message or "Unsupported transaction change",
                )
            )
            continue

        file_path = str(PurePosixPath(tx.repo_root) / change.path)
        contract_error = _team_repo_write_error(
            context,
            file_path,
            tool_name="daytona_codeact.transaction",
        )
        if contract_error is not None:
            report.errors.append(
                FileCommitResult(path=change.path, status="error", message=contract_error)
            )
            continue
        contract_warning = _team_repo_write_warning(
            context,
            file_path,
            tool_name="daytona_codeact.transaction",
        )
        if contract_warning is not None:
            report.warnings.append(contract_warning)
            record_coordination_warning(
                context,
                category="write_scope",
                message=contract_warning,
            )
        try:
            result = commit_ci_change_against_base(
                context,
                file_path,
                base_content=change.base_content,
                final_content=change.final_content,
                edit_type="codeact",
                description="daytona_codeact transaction",
            )
        except Exception as exc:
            report.errors.append(
                FileCommitResult(path=change.path, status="error", message=str(exc))
            )
            continue

        message = str(getattr(result, "message", "") or "")
        if bool(getattr(result, "success", False)):
            report.committed.append(
                FileCommitResult(path=change.path, status="ok", message=message)
            )
            continue
        if bool(getattr(result, "conflict", False)):
            report.conflicts.append(
                FileCommitResult(path=change.path, status="conflict", message=message)
            )
            continue
        report.errors.append(FileCommitResult(path=change.path, status="error", message=message))

    return report


async def cleanup_codeact_transaction(
    sandbox: Any,
    tx: CodeActTransaction,
) -> None:
    commands: list[str] = []
    if tx.scratch_root:
        commands.append(
            (
                f"git -C {shlex.quote(tx.repo_root)} worktree remove --force "
                f"{shlex.quote(tx.scratch_root)} >/dev/null 2>&1 || "
                f"rm -rf {shlex.quote(tx.scratch_root)}"
            )
        )
    if tx.patch_path:
        commands.append(f"rm -f {shlex.quote(tx.patch_path)}")
    if not commands:
        return
    exit_code, stdout = await _sandbox_exec(sandbox, "\n".join(commands), timeout=120)
    if exit_code != 0:
        logger.warning("Failed to fully clean up codeact transaction: %s", stdout.strip())


__all__ = [
    "CodeActTransaction",
    "CommitReport",
    "FileCommitResult",
    "RepoChange",
    "cleanup_codeact_transaction",
    "collect_transaction_changes",
    "commit_transaction_changes",
    "create_codeact_transaction",
]
