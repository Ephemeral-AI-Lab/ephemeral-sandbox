"""Tests for overlay NDJSON parsing and capture-runner readback."""

from __future__ import annotations

import base64
import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from sandbox.overlay.engine import LocalOverlayEngine
from sandbox.overlay.wire import parse_diff_ndjson
from sandbox.overlay.types import (
    OverlayCapture,
    OverlayPolicyReject,
    OverlayRunError,
)
from sandbox.runtime.registry import dispose_all_code_intelligence


@pytest.fixture(autouse=True)
def _registry() -> None:
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


def _meta_line(**overrides) -> str:
    base = {
        "exit_code": 0,
        "upper_bytes": 0,
        "upper_files": 0,
        "upper_changes": 0,
        "run_timings": {},
        "warnings": [],
    }
    base.update(overrides)
    return json.dumps({"_meta": base}, separators=(",", ":"))


def _change_line(
    *,
    rel: str,
    kind: str = "regular",
    base: bytes | None = None,
    upper: bytes | None = b"after\n",
    base_existed: bool = False,
) -> str:
    return json.dumps(
        {
            "rel": rel,
            "kind": kind,
            "base_bytes_b64": (
                None if base is None else base64.b64encode(base).decode("ascii")
            ),
            "upper_bytes_b64": (
                None if upper is None else base64.b64encode(upper).decode("ascii")
            ),
            "base_existed": base_existed,
        },
        separators=(",", ":"),
    )


def test_parse_ndjson_empty_body_raises() -> None:
    with pytest.raises(OverlayRunError):
        parse_diff_ndjson("")


def test_parse_ndjson_returns_policy_reject() -> None:
    raw = json.dumps(
        {
            "_reject": {
                "reason": "overlay_upper_full",
                "paths": [],
                "run_timings": {"walk_upperdir": 0.2},
            }
        }
    )

    result = parse_diff_ndjson(raw)

    assert isinstance(result, OverlayPolicyReject)
    assert result.reason == "overlay_upper_full"
    assert result.paths == ()
    assert result.run_timings == {"walk_upperdir": 0.2}


def test_parse_ndjson_meta_and_upper_change_bytes() -> None:
    raw = "\n".join(
        [
            _meta_line(upper_changes=1, upper_files=1, upper_bytes=6),
            _change_line(
                rel="src/app.py",
                base=b"before\n",
                upper=b"after\n",
                base_existed=True,
            ),
        ]
    )

    result = parse_diff_ndjson(raw)

    assert isinstance(result, OverlayCapture)
    assert result.upper_bytes == 6
    assert len(result.upper_changes) == 1
    change = result.upper_changes[0]
    assert change.rel == "src/app.py"
    assert change.kind == "regular"
    assert change.base_bytes == b"before\n"
    assert change.upper_bytes == b"after\n"
    assert change.base_existed is True


def test_parse_ndjson_invalid_meta_raises() -> None:
    with pytest.raises(OverlayRunError):
        parse_diff_ndjson("not-json\n")


def test_parse_ndjson_invalid_entry_raises() -> None:
    raw = _meta_line(upper_changes=1) + "\nnot-valid-json"
    with pytest.raises(OverlayRunError):
        parse_diff_ndjson(raw)


@pytest.mark.asyncio
async def test_read_diff_error_includes_overlay_output() -> None:
    async def _missing_diff_exec(_sandbox, _command, *, timeout=None):
        return SimpleNamespace(
            result="cat: /tmp/run/diff.ndjson: No such file or directory",
            exit_code=1,
        )

    capture_runner = LocalOverlayEngine(
        sandbox_id="overlay-missing-diff",
        workspace_root="/workspace",
        exec_process=_missing_diff_exec,
    )

    with pytest.raises(OverlayRunError) as exc_info:
        await capture_runner._read_diff(
            object(),
            SimpleNamespace(run_dir="/tmp/run"),
            overlay_stdout="mount setup failed",
            overlay_exit_code=255,
        )

    message = str(exc_info.value)
    assert "overlay_exit_code=255" in message
    assert "mount setup failed" in message


@pytest.mark.asyncio
async def test_local_daemon_readback_uses_filesystem_without_exec(
    tmp_path: Path,
) -> None:
    async def _should_not_exec(_sandbox, _command, *, timeout=None):
        raise AssertionError("local daemon readback should not shell out")

    run_dir = tmp_path / "run"
    run_dir.mkdir()
    (run_dir / "stdout.bin").write_text("local stdout\n", encoding="utf-8")
    (run_dir / "diff.ndjson").write_text(_meta_line(exit_code=0), encoding="utf-8")
    capture_runner = LocalOverlayEngine(
        sandbox_id="local",
        workspace_root=str(tmp_path),
        exec_process=_should_not_exec,
    )
    lease = SimpleNamespace(run_dir=str(run_dir))

    assert (
        await capture_runner._read_stdout(None, lease, fallback="fallback")
        == "local stdout\n"
    )
    diff = await capture_runner._read_diff(
        None,
        lease,
        overlay_stdout="local stdout\n",
        overlay_exit_code=0,
    )
    assert isinstance(diff, OverlayCapture)

    await capture_runner._cleanup_run_dir(None, lease)
    assert not run_dir.exists()


def _make_guarded_capture_runner(tmp_path: Path) -> LocalOverlayEngine:
    async def _unused_exec(*_args, **_kwargs):
        raise AssertionError("freshness guard test should not execute commands")

    return LocalOverlayEngine(
        sandbox_id=f"freshness-{tmp_path.name}",
        workspace_root=str(tmp_path),
        exec_process=_unused_exec,
        daemon_local=True,
    )


@pytest.mark.asyncio
async def test_freshness_guard_rejects_external_idle_mutation(tmp_path: Path) -> None:
    capture_runner = _make_guarded_capture_runner(tmp_path)
    await capture_runner._begin_workspace_fingerprint_guard()
    await capture_runner._end_workspace_fingerprint_guard()

    (tmp_path / "external.txt").write_text("outside\n", encoding="utf-8")

    with pytest.raises(OverlayRunError, match="workspace changed outside"):
        await capture_runner._begin_workspace_fingerprint_guard()


@pytest.mark.asyncio
async def test_freshness_guard_allows_concurrent_active_window(tmp_path: Path) -> None:
    capture_runner = _make_guarded_capture_runner(tmp_path)
    await capture_runner._begin_workspace_fingerprint_guard()
    await capture_runner._end_workspace_fingerprint_guard()

    await capture_runner._begin_workspace_fingerprint_guard()
    (tmp_path / "during-active.txt").write_text("ok\n", encoding="utf-8")
    await capture_runner._begin_workspace_fingerprint_guard()
    await capture_runner._end_workspace_fingerprint_guard()
    await capture_runner._end_workspace_fingerprint_guard()
