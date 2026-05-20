"""Plan §A8 Part 2 — runtime payload token-leak regression.

Under a plan-mode-active recorder lifecycle, no file produced anywhere
under ``run_dir`` may contain the OAuth token literal. Both Anthropic
and Codex literals are checked. Walk EVERY file as bytes (binary-safe,
no extension filtering) and scan for the literal byte sequence.

Failure surface: any ``__repr__`` / ``asdict()`` / json.dumps() call
that captures a plan-mode client object or a token string into a
recorder record would surface as the literal appearing in
``run.json`` / ``metrics.json`` / ``message.jsonl`` / etc.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from task_center_runner.audit.recorder import AuditRecorder


# Sentinel token literals — distinct, easy to grep for if a regression
# surfaces them.
ANTHROPIC_FAKE_TOKEN = b"sk-ant-oat01-FAKE_TOKEN_LITERAL_DO_NOT_LEAK"
CODEX_FAKE_ACCESS_TOKEN = b"codex_access_FAKE_TOKEN_LITERAL_DO_NOT_LEAK"
CODEX_FAKE_ID_TOKEN = b"eyJ.FAKE_CODEX_JWT_PAYLOAD_DO_NOT_LEAK.signature"


def _walk_run_dir_for_literal(run_dir: Path, literal: bytes) -> list[Path]:
    """Return every file under run_dir whose bytes contain *literal*.

    Recursive walk; reads files as bytes (binary-safe).
    """
    offenders: list[Path] = []
    for path in run_dir.rglob("*"):
        if not path.is_file():
            continue
        try:
            if literal in path.read_bytes():
                offenders.append(path)
        except OSError:
            continue
    return offenders


@pytest.mark.parametrize(
    "literals",
    [
        pytest.param([ANTHROPIC_FAKE_TOKEN], id="anthropic"),
        pytest.param([CODEX_FAKE_ACCESS_TOKEN, CODEX_FAKE_ID_TOKEN], id="codex"),
    ],
)
def test_recorder_lifecycle_does_not_leak_token(
    tmp_path: Path, literals: list[bytes]
) -> None:
    recorder = AuditRecorder(
        tmp_path,
        task_center_run_id="leak-guard-run",
        scenario_name="leak_guard",
        instance_id="leak-instance",
        sandbox_id="leak-sandbox",
        coding_plan_mode_active=True,
    )
    recorder.start()
    recorder.dispose()

    for literal in literals:
        offenders = _walk_run_dir_for_literal(tmp_path, literal)
        assert not offenders, (
            f"Token literal {literal!r} leaked into recorder output. "
            f"Offending files: {[str(p) for p in offenders]}"
        )
