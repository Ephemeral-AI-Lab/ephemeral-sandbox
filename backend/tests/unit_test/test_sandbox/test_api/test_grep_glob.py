"""Tests for ``sandbox.api.tool.glob`` and ``sandbox.api.tool.grep``."""

from __future__ import annotations

import pytest

from sandbox.api import (
    GlobRequest,
    GrepRequest,
    SandboxCaller,
)
import sandbox.api.tool.glob as glob_module
import sandbox.api.tool.grep as grep_module


_CALLER_FIELDS = {
    "agent_id": "a",
    "run_id": "",
    "agent_run_id": "",
    "task_id": "",
}


@pytest.mark.asyncio
async def test_glob_dispatches_to_sandbox_daemon(
    recording_transport_factory,
) -> None:
    async def fake_call(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": True,
            "filenames": ["a.py", "pkg/b.py"],
            "num_files": 2,
            "truncated": False,
            "timings": {"api.glob.total_s": 0.1},
        }

    transport = recording_transport_factory(fake_call)

    result = await glob_module.glob(
        "sb-1",
        GlobRequest(pattern="*.py", caller=SandboxCaller(agent_id="a")),
        transport=transport,
    )

    assert result.success is True
    assert result.filenames == ("a.py", "pkg/b.py")
    assert result.num_files == 2
    assert result.truncated is False
    assert transport.calls == [
        (
            "sb-1",
            "api.v1.glob",
            {"pattern": "*.py", "caller": _CALLER_FIELDS},
            60,
        ),
    ]


@pytest.mark.asyncio
async def test_glob_passes_optional_path(
    recording_transport_factory,
) -> None:
    async def fake_call(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": True,
            "filenames": [],
            "num_files": 0,
            "truncated": False,
            "timings": {},
        }

    transport = recording_transport_factory(fake_call)
    await glob_module.glob(
        "sb-1",
        GlobRequest(
            pattern="*.py",
            path="pkg/sub",
            caller=SandboxCaller(agent_id="a"),
        ),
        transport=transport,
    )

    args = transport.calls[0][2]
    assert args["path"] == "pkg/sub"


@pytest.mark.asyncio
async def test_glob_truncated_flag_surfaces(
    recording_transport_factory,
) -> None:
    async def fake_call(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": True,
            "filenames": [f"f{i}.py" for i in range(100)],
            "num_files": 100,
            "truncated": True,
            "timings": {},
        }

    transport = recording_transport_factory(fake_call)
    result = await glob_module.glob(
        "sb-1",
        GlobRequest(pattern="*.py", caller=SandboxCaller(agent_id="a")),
        transport=transport,
    )

    assert result.num_files == 100
    assert result.truncated is True


@pytest.mark.asyncio
async def test_grep_dispatches_to_sandbox_daemon(
    recording_transport_factory,
) -> None:
    async def fake_call(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": True,
            "output_mode": "files_with_matches",
            "filenames": ["a.py"],
            "content": "",
            "num_files": 1,
            "num_lines": 0,
            "num_matches": 1,
            "applied_limit": 250,
            "applied_offset": 0,
            "truncated": False,
            "timings": {"api.grep.total_s": 0.1},
        }

    transport = recording_transport_factory(fake_call)

    result = await grep_module.grep(
        "sb-1",
        GrepRequest(
            pattern="hello", caller=SandboxCaller(agent_id="a")
        ),
        transport=transport,
    )

    assert result.success is True
    assert result.output_mode == "files_with_matches"
    assert result.filenames == ("a.py",)
    assert result.num_files == 1
    assert result.applied_limit == 250
    sandbox_id, op, payload, timeout = transport.calls[0]
    assert sandbox_id == "sb-1"
    assert op == "api.v1.grep"
    assert payload["pattern"] == "hello"
    assert payload["output_mode"] == "files_with_matches"
    assert payload["caller"] == _CALLER_FIELDS
    assert timeout == 60


@pytest.mark.asyncio
async def test_grep_passes_optional_filters(
    recording_transport_factory,
) -> None:
    async def fake_call(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": True,
            "output_mode": "content",
            "filenames": [],
            "content": "",
            "num_files": 0,
            "num_lines": 0,
            "num_matches": 0,
            "applied_limit": 50,
            "applied_offset": 5,
            "truncated": False,
            "timings": {},
        }

    transport = recording_transport_factory(fake_call)
    await grep_module.grep(
        "sb-1",
        GrepRequest(
            pattern="needle",
            path="pkg",
            glob_filter="*.py",
            output_mode="content",
            head_limit=50,
            offset=5,
            case_insensitive=True,
            line_numbers=True,
            multiline=False,
            caller=SandboxCaller(agent_id="a"),
        ),
        transport=transport,
    )

    payload = transport.calls[0][2]
    assert payload["path"] == "pkg"
    assert payload["glob_filter"] == "*.py"
    assert payload["output_mode"] == "content"
    assert payload["head_limit"] == 50
    assert payload["offset"] == 5
    assert payload["case_insensitive"] is True
    assert payload["line_numbers"] is True
    assert payload["multiline"] is False


@pytest.mark.asyncio
async def test_grep_zero_head_limit_reaches_daemon_as_zero(
    recording_transport_factory,
) -> None:
    """``head_limit=0`` (the documented unlimited sentinel) must be sent
    verbatim in the daemon payload — not dropped. The daemon treats 0 as
    "unlimited"; dropping the key would silently fall back to the 250-entry
    default and truncate.
    """

    async def fake_call(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": True,
            "output_mode": "count",
            "filenames": ["a.py"],
            "content": "a.py:3",
            "num_files": 1,
            "num_lines": 0,
            "num_matches": 3,
            "applied_limit": None,
            "applied_offset": 0,
            "truncated": False,
            "timings": {},
        }

    transport = recording_transport_factory(fake_call)
    result = await grep_module.grep(
        "sb-1",
        GrepRequest(
            pattern="hello",
            output_mode="count",
            head_limit=0,
            caller=SandboxCaller(agent_id="a"),
        ),
        transport=transport,
    )

    assert result.applied_limit is None
    assert result.num_matches == 3
    assert result.output_mode == "count"
    # Critical contract: the wrapper must forward head_limit=0 to the
    # daemon. Omitting the key would let the daemon default to 250.
    payload = transport.calls[0][2]
    assert payload["head_limit"] == 0


@pytest.mark.asyncio
async def test_glob_audit_sink_receives_start_and_result(
    recording_transport_factory,
) -> None:
    events: list[object] = []

    class SinkStub:
        def publish(self, event: object) -> None:
            events.append(event)

    async def fake_call(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": True,
            "filenames": [],
            "num_files": 0,
            "truncated": False,
            "timings": {},
        }

    transport = recording_transport_factory(fake_call)
    await glob_module.glob(
        "sb-1",
        GlobRequest(pattern="*.py", caller=SandboxCaller(agent_id="a")),
        audit_sink=SinkStub(),
        transport=transport,
    )

    assert events, "audited_operation must publish at least one event"
