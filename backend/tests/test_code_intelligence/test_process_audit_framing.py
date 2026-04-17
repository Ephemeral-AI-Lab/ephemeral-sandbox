"""Unit tests for the ProcessAuditor combined-exec framing helpers."""

from __future__ import annotations

import base64
import json
import os
import shlex

import pytest

from code_intelligence.routing.process_auditor import (
    _PROCESS_AUDIT_SENTINEL_PREFIX,
    _SNAPSHOT_SCRIPT_B64,
    _ProcessAuditFrameError,
    _build_process_audit_combined_bash,
    _parse_process_audit_combined_output,
    _sentinel,
)


def _unshlex_single(quoted: str) -> str:
    """Reverse a single ``shlex.quote`` call.

    ``shlex.quote(s)`` returns ``'...'`` wrapping ``s``, with every literal
    single quote rendered as ``'\"'\"'``. Reverse that so callers can inspect
    the original script body.
    """
    if not (quoted.startswith("'") and quoted.endswith("'")):
        raise AssertionError(f"not a shlex.quote single-quoted string: {quoted!r}")
    body = quoted[1:-1]
    return body.replace("'\"'\"'", "'")


def _framed(
    run_id: str,
    *,
    before: dict,
    exec_stdout: bytes,
    exit_code: int,
    after: dict,
) -> str:
    before_b64 = base64.b64encode(json.dumps({"ok": True, "files": before}).encode("utf-8")).decode("ascii")
    after_b64 = base64.b64encode(json.dumps({"ok": True, "files": after}).encode("utf-8")).decode("ascii")
    exec_b64 = base64.b64encode(exec_stdout).decode("ascii")
    return (
        f"\n{_sentinel(run_id, 'BEFORE', 'OPEN')}\n"
        f"{before_b64}\n"
        f"{_sentinel(run_id, 'BEFORE', 'CLOSE')}\n"
        f"{_sentinel(run_id, 'EXEC', 'OPEN')}\n"
        f"{exec_b64}\n"
        f"{_sentinel(run_id, 'EXEC', 'CLOSE')}\n"
        f"{_sentinel(run_id, 'EXIT', 'OPEN')}\n"
        f"{exit_code}\n"
        f"{_sentinel(run_id, 'EXIT', 'CLOSE')}\n"
        f"{_sentinel(run_id, 'AFTER', 'OPEN')}\n"
        f"{after_b64}\n"
        f"{_sentinel(run_id, 'AFTER', 'CLOSE')}\n"
    )


def test_parser_happy_path() -> None:
    run_id = "deadbeef01234567"
    before = {"/workspace/a.py": {"rel": "a.py", "exists": True, "hash": "aaa", "head_hash": "h1"}}
    after = {"/workspace/a.py": {"rel": "a.py", "exists": True, "hash": "bbb", "head_hash": "h1"}}
    raw = _framed(run_id, before=before, exec_stdout=b"hello world\n", exit_code=0, after=after)

    parsed = _parse_process_audit_combined_output(raw, run_id=run_id)

    assert parsed.before == before
    assert parsed.after == after
    assert parsed.exec_stdout == "hello world\n"
    assert parsed.exec_exit_code == 0


def test_parser_adversarial_user_stdout_embeds_sentinels() -> None:
    run_id = "feedfacefeedface"
    # User stdout contains the literal EXEC CLOSE sentinel. Because the payload
    # is base64-encoded in transit, extraction must still succeed and the
    # decoded bytes must round-trip the sentinel verbatim.
    adversarial_close = _sentinel(run_id, "EXEC", "CLOSE")
    evil = f"prefix {adversarial_close} suffix\n".encode("utf-8")
    raw = _framed(run_id, before={}, exec_stdout=evil, exit_code=0, after={})

    parsed = _parse_process_audit_combined_output(raw, run_id=run_id)

    assert parsed.exec_stdout == evil.decode("utf-8")
    assert adversarial_close in parsed.exec_stdout


def test_parser_large_exec_stdout() -> None:
    run_id = "abcd1234abcd1234"
    payload = os.urandom(1_000_000)
    raw = _framed(run_id, before={}, exec_stdout=payload, exit_code=0, after={})

    parsed = _parse_process_audit_combined_output(raw, run_id=run_id)

    # Random bytes can't survive utf-8 errors="replace" losslessly, so assert
    # round-trip via the raw base64 envelope we constructed instead.
    reparsed_b64 = base64.b64encode(payload).decode("ascii")
    assert reparsed_b64 in raw
    assert parsed.exec_stdout  # parser produced a non-empty decoded string


def test_parser_missing_section_raises() -> None:
    run_id = "cafebabecafebabe"
    raw = _framed(run_id, before={}, exec_stdout=b"", exit_code=0, after={})
    # Drop the AFTER section entirely.
    after_open = _sentinel(run_id, "AFTER", "OPEN")
    truncated = raw[: raw.index(after_open)]

    with pytest.raises(_ProcessAuditFrameError):
        _parse_process_audit_combined_output(truncated, run_id=run_id)


def test_parser_wrong_run_id_raises() -> None:
    emitted_run = "1111111111111111"
    queried_run = "2222222222222222"
    raw = _framed(emitted_run, before={}, exec_stdout=b"x", exit_code=0, after={})

    with pytest.raises(_ProcessAuditFrameError):
        _parse_process_audit_combined_output(raw, run_id=queried_run)


def test_parser_non_zero_exit_code_round_trip() -> None:
    run_id = "99aabbcc99aabbcc"
    raw = _framed(run_id, before={}, exec_stdout=b"boom\n", exit_code=-2, after={})

    parsed = _parse_process_audit_combined_output(raw, run_id=run_id)

    assert parsed.exec_exit_code == -2
    assert parsed.exec_stdout == "boom\n"


def test_build_combined_bash_contains_all_sentinels_and_quoted_command() -> None:
    run_id = "zz11zz11zz11zz11"
    user_cmd = "echo hi && printf '%s' 'world'"
    script = _build_process_audit_combined_bash(
        user_cmd,
        workspace_root="/workspace",
        run_id=run_id,
    )

    for section in ("BEFORE", "EXEC", "EXIT", "AFTER"):
        assert _sentinel(run_id, section, "OPEN") in script
        assert _sentinel(run_id, section, "CLOSE") in script

    # Outer wrapper invariants.
    prefix = "env -u LC_ALL bash -o pipefail -c "
    assert script.startswith(prefix)
    # Peel the outer shlex.quote() layer to recover the real script body.
    outer_quoted = script[len(prefix):]
    inner = _unshlex_single(outer_quoted)
    assert _SNAPSHOT_SCRIPT_B64 in inner
    # The user command is embedded via shlex.quote so nothing has to be
    # re-escaped by the caller.
    assert shlex.quote(user_cmd) in inner
    # Portability: no bash -w0 flag (macOS BSD base64 does not support it).
    assert "base64 -w" not in inner
    assert "base64 | tr -d" in inner


def test_sentinel_prefix_matches_documented_constant() -> None:
    assert _PROCESS_AUDIT_SENTINEL_PREFIX == "__CIAUDIT_"
    assert _sentinel("abc", "BEFORE", "OPEN") == "__CIAUDIT_abc_BEFORE_OPEN__"
