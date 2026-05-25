"""Unit contracts for isolated workspace orphan GC helpers."""

from __future__ import annotations

import signal
from pathlib import Path

from sandbox.isolated_workspace._control_plane import orphan_reaper as reaper_module


def _proc(
    root: Path,
    *,
    pid: int,
    ppid: int,
    state: str,
    comm: str,
    cmdline: str,
) -> None:
    proc = root / str(pid)
    proc.mkdir()
    (proc / "stat").write_text(
        f"{pid} ({comm}) {state} {ppid} 0 0 0\n",
        encoding="utf-8",
    )
    (proc / "cmdline").write_bytes(cmdline.encode("utf-8").replace(b" ", b"\0"))


def test_iter_namespace_holder_processes_reads_unshare_and_child(tmp_path: Path) -> None:
    _proc(
        tmp_path,
        pid=11,
        ppid=1,
        state="S",
        comm="unshare",
        cmdline=(
            "unshare --fork /usr/bin/python3.10 -m "
            "sandbox.isolated_workspace.scripts.ns_holder 24 25"
        ),
    )
    _proc(
        tmp_path,
        pid=12,
        ppid=11,
        state="T",
        comm="python3.10",
        cmdline="/usr/bin/python3.10 -m sandbox.isolated_workspace.scripts.ns_holder 24 25",
    )
    _proc(
        tmp_path,
        pid=13,
        ppid=1,
        state="S",
        comm="python3.10",
        cmdline="/usr/bin/python3.10 -m unrelated",
    )

    processes = reaper_module._iter_namespace_holder_processes(tmp_path)

    assert [(proc.pid, proc.ppid, proc.state, proc.comm) for proc in processes] == [
        (11, 1, "S", "unshare"),
        (12, 11, "T", "python3.10"),
    ]


def test_namespace_holder_signal_order_terminates_child_before_unshare_parent() -> None:
    processes = [
        reaper_module._NamespaceHolderProcess(11, 1, "S", "unshare", "holder"),
        reaper_module._NamespaceHolderProcess(12, 11, "S", "python3.10", "holder"),
    ]

    assert [proc.pid for proc in reaper_module._namespace_holder_signal_order(processes)] == [
        12,
        11,
    ]


def test_veth_handle_prefix_preserves_handle_chars_before_suffix() -> None:
    assert reaper_module._handle_prefix_from_veth_name("eos-iws-abchnhh") == "abchnh"
    assert reaper_module._handle_prefix_from_veth_name("eos-iws-abchnhn") == "abchnh"
    assert reaper_module._handle_prefix_from_veth_name("eos-iws-too-short") is None


def test_reap_orphan_holder_processes_continues_then_kills(
    monkeypatch,
) -> None:
    signals: list[tuple[int, int]] = []
    snapshots = [
        [
            reaper_module._NamespaceHolderProcess(11, 1, "T", "unshare", "holder"),
            reaper_module._NamespaceHolderProcess(12, 11, "T", "python3.10", "holder"),
        ],
        [
            reaper_module._NamespaceHolderProcess(11, 1, "T", "unshare", "holder"),
            reaper_module._NamespaceHolderProcess(12, 11, "T", "python3.10", "holder"),
        ],
        [],
    ]

    class Harness(reaper_module._OrphanResourceReaperMixin):
        _handles = {}

        def __init__(self) -> None:
            self.events: list[dict[str, object]] = []

        def _clock(self) -> float:
            return 1.0

        def _emit(self, _event_type: str, payload: dict[str, object]) -> None:
            self.events.append(payload)

    def fake_iter() -> list[reaper_module._NamespaceHolderProcess]:
        return snapshots.pop(0) if snapshots else []

    def fake_kill(pid: int, sig: int) -> None:
        signals.append((pid, sig))

    monkeypatch.setattr(reaper_module, "_iter_namespace_holder_processes", fake_iter)
    monkeypatch.setattr(reaper_module.os, "kill", fake_kill)
    monkeypatch.setattr(reaper_module.time, "sleep", lambda _seconds: None)

    harness = Harness()
    harness._reap_orphan_holder_processes()

    assert (11, signal.SIGCONT) in signals
    assert (12, signal.SIGCONT) in signals
    assert (12, signal.SIGTERM) in signals
    assert (11, signal.SIGTERM) in signals
    assert {event["kind"] for event in harness.events} == {"holder"}
