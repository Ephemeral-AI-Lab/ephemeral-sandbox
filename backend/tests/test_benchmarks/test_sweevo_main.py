from __future__ import annotations

from message.messages import ConversationMessage, TextBlock
from message.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    SystemNotification,
    ToolExecutionCompleted,
)
from providers.types import UsageSnapshot
import logging
import sys

from benchmarks.sweevo import __main__ as sweevo_main
import asyncio


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

    run_logs = sorted(path for path in tmp_path.glob("*.log") if ".code-intelligence." not in path.name)
    assert len(run_logs) == 1

    ci_logs = sorted(tmp_path.glob("*.code-intelligence.log"))
    assert len(ci_logs) == 1

    contents = run_logs[0].read_text(encoding="utf-8")
    assert "SWE-EVO run  instance=pydantic__pydantic_v2.7.0_v2.7.1" in contents
    assert "[pass] recorded" in contents
    assert "warning on stderr" in contents
    assert "\x1b[" not in contents


def test_main_writes_code_intelligence_log_in_parallel(monkeypatch, tmp_path):
    async def _fake_cmd_run(_args):
        logging.getLogger("code_intelligence.routing.service").info("indexed workspace")
        logging.getLogger("server.routers.code_intelligence").info("router request")
        logging.getLogger("benchmarks.sweevo.runner").warning("benchmark warning")
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

    ci_logs = sorted(tmp_path.glob("*.code-intelligence.log"))
    assert len(ci_logs) == 1

    contents = ci_logs[0].read_text(encoding="utf-8")
    assert "code_intelligence.routing.service: indexed workspace" in contents
    assert "server.routers.code_intelligence: router request" in contents
    assert "benchmarks.sweevo.runner" not in contents


def test_main_run_log_records_info_level_python_logs(monkeypatch, tmp_path):
    async def _fake_cmd_run(_args):
        logging.getLogger("benchmarks.sweevo.runner").info("benchmark info message")
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

    run_logs = sorted(path for path in tmp_path.glob("*.log") if ".code-intelligence." not in path.name)
    assert len(run_logs) == 1
    contents = run_logs[0].read_text(encoding="utf-8")
    assert "INFO benchmarks.sweevo.runner: benchmark info message" in contents


def test_main_does_not_print_log_paths_into_run_log(monkeypatch, tmp_path):
    async def _fake_cmd_run(_args):
        print("benchmark body", flush=True)
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

    run_logs = sorted(path for path in tmp_path.glob("*.log") if ".code-intelligence." not in path.name)
    assert len(run_logs) == 1
    contents = run_logs[0].read_text(encoding="utf-8")
    assert "benchmark body" in contents
    assert "Log file:" not in contents
    assert "Code intelligence log file:" not in contents


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

    run_logs = sorted(path for path in tmp_path.glob("*.log") if ".code-intelligence." not in path.name)
    assert len(run_logs) == 1
    contents = run_logs[0].read_text(encoding="utf-8")
    assert f"[text] {long_text}" in contents
    assert f"[system:runtime_note] {long_system}" in contents


def test_main_run_log_keeps_full_tool_done_messages(monkeypatch, tmp_path):
    from benchmarks.sweevo import runner as sweevo_runner

    long_tool_output = '{\n  "scope_paths": [\n    "dask/dataframe/groupby.py",\n    "dask/dataframe/io/hdf.py",\n    "dask/dataframe/io/json.py"\n  ],\n  "details": "' + ("benchmark-log-" * 30) + '"\n}'

    async def _fake_run_sweevo_with_agent(*, printer, **kwargs):
        printer.emit(
            ToolExecutionCompleted(
                tool_name="ci_scoped_status",
                output=long_tool_output,
                agent_name="team_planner",
                work_id="2af5cbde-0bae-4f7f-98f1-5aa6d9a13b6c",
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

    run_logs = sorted(path for path in tmp_path.glob("*.log") if ".code-intelligence." not in path.name)
    assert len(run_logs) == 1
    contents = run_logs[0].read_text(encoding="utf-8")
    assert (
        "[team_planner  ] [2af5cbde-0bae-4f7f-98f1-5aa6d9a13b6c] "
        "<- tool_done:  ci_scoped_status [ok] {"
    ) in contents
    assert (
        '[team_planner  ] [2af5cbde-0bae-4f7f-98f1-5aa6d9a13b6c] │   "scope_paths": ['
    ) in contents
    assert (
        '[team_planner  ] [2af5cbde-0bae-4f7f-98f1-5aa6d9a13b6c] │     "dask/dataframe/io/json.py"'
    ) in contents
    assert (
        '[team_planner  ] [2af5cbde-0bae-4f7f-98f1-5aa6d9a13b6c] │   "details": "'
    ) in contents


def test_ansi_stripping_tee_flush_tolerates_closed_mirror(tmp_path):
    mirror = (tmp_path / "run.log").open("w", encoding="utf-8")
    tee = sweevo_main._AnsiStrippingTee(sys.stdout, mirror)

    mirror.close()

    tee.flush()


def test_cmd_run_forces_color_even_when_stdout_is_not_tty(monkeypatch):
    created: dict[str, object] = {}

    class _FakePrinter:
        def __init__(self, *, color, truncate, timestamps, sink):
            created["color"] = color
            created["truncate"] = truncate
            created["timestamps"] = timestamps
            created["sink"] = sink

        def summary(self):
            return {"totals": {}}

        def raw_line(self, agent, body):
            return None

    async def _fake_run_sweevo_with_agent(**kwargs):
        return {
            "test": {"exit_code": 0},
            "grading": {},
            "team": {},
            "team_status": "succeeded",
            "agent_events": 0,
        }

    monkeypatch.setattr("message.event_printer.MultiAgentEventPrinter", _FakePrinter)
    monkeypatch.setattr("benchmarks.sweevo.runner.run_sweevo_with_agent", _fake_run_sweevo_with_agent)

    class _FakeStdout:
        def write(self, data):
            return len(data)

        def flush(self):
            return None

        def isatty(self):
            return False

    monkeypatch.setattr(sys, "stdout", _FakeStdout())

    args = sweevo_main._build_parser().parse_args(["--instance-id", "pydantic__pydantic_v2.7.0_v2.7.1"])
    exit_code = asyncio.run(sweevo_main._cmd_run(args))

    assert exit_code == 0
    assert created["color"] is True
