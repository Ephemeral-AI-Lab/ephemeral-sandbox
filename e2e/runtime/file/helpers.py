"""Shared helpers for the runtime/file live e2e matrix."""

from __future__ import annotations

import contextlib
import json
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

import pytest

from core.cli import internal_runtime, is_error, observability, runtime
from core.config import IMAGE
from manager.management import helpers as mgmt


def file_read(
    sandbox_id,
    path,
    *,
    offset=None,
    limit=None,
    workspace_session_id=None,
    timeout=180,
):
    args = ["--path", path]
    if offset is not None:
        args += ["--offset", str(offset)]
    if limit is not None:
        args += ["--limit", str(limit)]
    if workspace_session_id is not None:
        args += ["--workspace-session-id", workspace_session_id]
    return runtime(sandbox_id, "file_read", *args, timeout=timeout)


def file_write(
    sandbox_id,
    path,
    content,
    *,
    workspace_session_id=None,
    timeout=180,
):
    args = ["--path", path, "--content", content]
    if workspace_session_id is not None:
        args += ["--workspace-session-id", workspace_session_id]
    return runtime(sandbox_id, "file_write", *args, timeout=timeout)


def file_edit(
    sandbox_id,
    path,
    edits,
    *,
    workspace_session_id=None,
    timeout=180,
):
    args = ["--path", path, "--edits", json.dumps(edits)]
    if workspace_session_id is not None:
        args += ["--workspace-session-id", workspace_session_id]
    return runtime(sandbox_id, "file_edit", *args, timeout=timeout)


def file_blame(sandbox_id, path, *, workspace_session_id=None, timeout=180):
    assert workspace_session_id is None, "file_blame has no session-scoped CLI flag"
    return runtime(sandbox_id, "file_blame", "--path", path, timeout=timeout)


def exec_command(
    sandbox_id,
    command,
    *,
    workspace_session_id=None,
    timeout_ms=None,
    yield_time_ms=None,
    timeout=180,
):
    """Run a command; responses include workspace_session_id for command-backed workspaces."""
    args = []
    if workspace_session_id is not None:
        args += ["--workspace-session-id", workspace_session_id]
    if timeout_ms is not None:
        args += ["--timeout-ms", str(timeout_ms)]
    if yield_time_ms is not None:
        args += ["--yield-time-ms", str(yield_time_ms)]
    args.append(command)
    return runtime(sandbox_id, "exec_command", *args, timeout=timeout)


def write_command_stdin(
    sandbox_id,
    command_session_id,
    stdin,
    *,
    yield_time_ms=None,
    timeout=180,
):
    args = ["--command-session-id", command_session_id]
    if yield_time_ms is not None:
        args += ["--yield-time-ms", str(yield_time_ms)]
    args.append(stdin)
    return runtime(sandbox_id, "write_command_stdin", *args, timeout=timeout)


def read_command_lines(
    sandbox_id,
    command_session_id,
    *,
    start_offset=None,
    limit=None,
    timeout=180,
):
    args = ["--command-session-id", command_session_id]
    if start_offset is not None:
        args += ["--start-offset", str(start_offset)]
    if limit is not None:
        args += ["--limit", str(limit)]
    return runtime(sandbox_id, "read_command_lines", *args, timeout=timeout)


def create_workspace_session(sandbox_id, *, network_profile=None):
    args = {}
    if network_profile is not None:
        args["network_profile"] = network_profile
    result = internal_runtime(sandbox_id, "create_workspace_session", args)
    assert_ok(result)
    assert result["finalize_policy"] == "no_op", result
    return result["workspace_session_id"]


def destroy_workspace_session(sandbox_id, workspace_session_id, *, grace_s=None):
    args = {"workspace_session_id": workspace_session_id}
    if grace_s is not None:
        args["grace_s"] = grace_s
    return internal_runtime(sandbox_id, "destroy_workspace_session", args)


@pytest.fixture
def workspace_session(sandbox):
    workspace_session_id = create_workspace_session(sandbox)
    try:
        yield workspace_session_id
    finally:
        destroy_workspace_session(sandbox, workspace_session_id, grace_s=1)


@contextlib.contextmanager
def sandbox_from_workspace(tmp_path, files=None, executable=(), dirs=()):
    root = tmp_path / "workspace"
    root.mkdir()
    for directory in dirs:
        (root / directory).mkdir(parents=True, exist_ok=True)
    for name, content in (files or {}).items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        if name in executable:
            path.chmod(0o755)
    created = mgmt.create_sandbox(image=IMAGE, workspace_root=str(root))
    sandbox_id = created.get("id")
    assert sandbox_id, created
    try:
        yield sandbox_id
    finally:
        mgmt.destroy_sandbox(sandbox_id)


def assert_ok(result):
    assert not is_error(result), result
    return result


def assert_error(result, kind, message_contains=None):
    assert is_error(result), result
    error = result["error"]
    assert error.get("kind") == kind, result
    if message_contains is not None:
        assert message_contains in error.get("message", ""), result
    return error


def assert_content(result, content):
    assert_ok(result)
    assert result["content"] == content, result
    assert result["bytes_read"] == len(content.encode("utf-8")), result
    return result


def edit(old_string, new_string, *, replace_all=None):
    item = {"old_string": old_string, "new_string": new_string}
    if replace_all is not None:
        item["replace_all"] = replace_all
    return item


def layerstack(sandbox_id, *, workspace_id=None, window_ms=None):
    args = ["--sandbox-id", sandbox_id]
    if workspace_id is not None:
        args += ["--workspace-id", workspace_id]
    if window_ms is not None:
        args += ["--window-ms", str(window_ms)]
    return observability("layerstack", *args)


def snapshot(sandbox_id):
    return observability("snapshot", "--sandbox-id", sandbox_id)


def manifest_version(sandbox_id):
    return layerstack(sandbox_id)["manifest_version"]


def layer_ids(sandbox_id):
    return [layer["layer_id"] for layer in layerstack(sandbox_id)["layers"]]


def assert_manifest_delta(sandbox_id, before, delta):
    after = layerstack(sandbox_id)
    assert after["manifest_version"] == before["manifest_version"] + delta, after
    return after


def assert_blame_tiling(sandbox_id, path, blame=None):
    read = assert_ok(file_read(sandbox_id, path))
    blame = assert_ok(blame or file_blame(sandbox_id, path))
    ranges = blame["ranges"]
    expected_start = 1
    total = 0
    for item in ranges:
        assert item["start_line"] == expected_start, blame
        assert item["line_count"] > 0, blame
        expected_start += item["line_count"]
        total += item["line_count"]
    assert total == read["total_lines"], {"read": read, "blame": blame}
    return blame


def owners_by_line(blame):
    owners = []
    for item in blame["ranges"]:
        owners.extend([item["owner"]] * item["line_count"])
    return owners


def assert_blame_owners(sandbox_id, path, expected_owners, blame=None):
    blame = assert_blame_tiling(sandbox_id, path, blame)
    assert owners_by_line(blame) == list(expected_owners), blame
    return blame


def assert_blame_ranges(sandbox_id, path, expected_ranges, blame=None):
    blame = assert_blame_tiling(sandbox_id, path, blame)
    actual = [
        (item["start_line"], item["line_count"], item["owner"])
        for item in blame["ranges"]
    ]
    assert actual == expected_ranges, blame
    return blame


def owner_for_line(sandbox_id, path, line=1):
    blame = assert_blame_tiling(sandbox_id, path)
    return owners_by_line(blame)[line - 1]


def assert_single_owner(sandbox_id, path, owner=None, prefix=None):
    blame = assert_blame_tiling(sandbox_id, path)
    owners = set(owners_by_line(blame))
    assert len(owners) == 1, blame
    found = next(iter(owners))
    if owner is not None:
        assert found == owner, blame
    if prefix is not None:
        assert found.startswith(prefix), blame
    return found


def run_concurrently(calls, *, max_workers=None):
    results = [None] * len(calls)
    with ThreadPoolExecutor(max_workers=max_workers or len(calls)) as executor:
        future_indexes = {
            executor.submit(call): index for index, call in enumerate(calls)
        }
        for future in as_completed(future_indexes):
            results[future_indexes[future]] = future.result()
    return results


def write_text(path: Path, content, *, executable=False):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    if executable:
        path.chmod(0o755)
    return path
