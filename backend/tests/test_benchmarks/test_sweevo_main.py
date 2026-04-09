from __future__ import annotations

from message.messages import ConversationMessage, TextBlock
from message.stream_events import AssistantTextDelta, AssistantTurnComplete, SystemNotification
from providers.types import UsageSnapshot
import sys

from benchmarks.sweevo import __main__ as sweevo_main


def test_build_run_log_path_uses_instance_id(tmp_path):
    args = sweevo_main._build_parser().parse_args(
        [
            "--instance-id",
            "pydantic__pydantic_v2.7.0_v2.7.1",
            "--log-dir",
            str(tmp_path),
        ]
    )

    log_path = sweevo_main._build_run_log_path(args, timestamp="20260409T110608Z")

    assert log_path == tmp_path / "20260409T110608Z_pydantic__pydantic_v2.7.0_v2.7.1.log"


def test_main_writes_plaintext_run_log(monkeypatch, tmp_path):
    async def _fake_cmd_run(args):
        assert args.log_dir == str(tmp_path)
        print("=" * 72, flush=True)
        print("  SWE-EVO run  instance=pydantic__pydantic_v2.7.0_v2.7.1", flush=True)
        print("\033[32m[pass]\033[0m recorded", flush=True)
        sys.stderr.write("warning on stderr\n")
        sys.stderr.flush()
        return 0

    monkeypatch.setattr(sweevo_main, "_cmd_run", _fake_cmd_run)

    exit_code = sweevo_main.main(
        [
            "--instance-id",
            "pydantic__pydantic_v2.7.0_v2.7.1",
            "--log-dir",
            str(tmp_path),
        ]
    )

    assert exit_code == 0

    log_files = sorted(tmp_path.glob("*.log"))
    assert len(log_files) == 1

    contents = log_files[0].read_text(encoding="utf-8")
    assert f"Log file: {log_files[0]}" in contents
    assert "SWE-EVO run  instance=pydantic__pydantic_v2.7.0_v2.7.1" in contents
    assert "[pass] recorded" in contents
    assert "warning on stderr" in contents
    assert "\x1b[" not in contents


def test_main_list_does_not_write_run_log(monkeypatch, tmp_path):
    monkeypatch.setattr(sweevo_main, "_cmd_list", lambda _source: 0)

    exit_code = sweevo_main.main(["--list", "--log-dir", str(tmp_path)])

    assert exit_code == 0
    assert list(tmp_path.iterdir()) == []


def test_main_run_log_keeps_full_conversation_messages(monkeypatch, tmp_path):
    from benchmarks.sweevo import runner as sweevo_runner

    long_text = "conversation-" * 60
    long_system = "system-note-" * 50

    async def _fake_run_sweevo_with_agent(*, printer, **kwargs):
        printer.emit(
            AssistantTextDelta(
                text=long_text,
                agent_name="developer",
                work_id="run-1234567890",
            )
        )
        printer.emit(
            AssistantTurnComplete(
                message=ConversationMessage(
                    role="assistant",
                    content=[TextBlock(text=long_text)],
                ),
                usage=UsageSnapshot(),
                agent_name="developer",
                work_id="run-1234567890",
            )
        )
        printer.emit(
            SystemNotification(
                text=long_system,
                category="runtime_note",
                agent_name="developer",
                work_id="run-1234567890",
            )
        )
        return {
            "test": {"exit_code": 0},
            "grading": {},
            "team": {},
            "team_status": "succeeded",
            "agent_events": 1,
        }

    monkeypatch.setattr(sweevo_runner, "run_sweevo_with_agent", _fake_run_sweevo_with_agent)

    exit_code = sweevo_main.main(
        [
            "--instance-id",
            "pydantic__pydantic_v2.7.0_v2.7.1",
            "--log-dir",
            str(tmp_path),
        ]
    )

    assert exit_code == 0

    log_files = sorted(tmp_path.glob("*.log"))
    assert len(log_files) == 1
    contents = log_files[0].read_text(encoding="utf-8")
    assert f"[text] {long_text}" in contents
    assert f"[system:runtime_note] {long_system}" in contents
