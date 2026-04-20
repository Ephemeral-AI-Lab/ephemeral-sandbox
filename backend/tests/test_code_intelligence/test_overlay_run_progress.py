from __future__ import annotations

import time
from concurrent.futures import ThreadPoolExecutor

from code_intelligence.routing.overlay_run import run_user_command


def test_run_user_command_writes_stdout_incrementally(tmp_path) -> None:
    stdout_path = tmp_path / "stdout.bin"
    command = (
        "python3 -c 'import sys,time; "
        "print(\"first\", flush=True); "
        "time.sleep(1.0); "
        "print(\"second\", flush=True)'"
    )

    with ThreadPoolExecutor(max_workers=1) as pool:
        future = pool.submit(
            run_user_command,
            user_cmd=command,
            stdin_bytes=None,
            cwd=str(tmp_path),
            stdout_path=str(stdout_path),
        )
        deadline = time.monotonic() + 3.0
        while time.monotonic() < deadline:
            if stdout_path.exists() and "first" in stdout_path.read_text(
                encoding="utf-8"
            ):
                break
            time.sleep(0.02)
        else:
            raise AssertionError("stdout.bin did not receive first line mid-run")

        assert not future.done()
        stdout, exit_code = future.result(timeout=3.0)

    assert exit_code == 0
    assert stdout == b"first\nsecond\n"
    assert stdout_path.read_bytes() == stdout
