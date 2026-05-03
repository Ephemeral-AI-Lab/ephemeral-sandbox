"""Overlay runtime upload and command execution helpers."""

from __future__ import annotations

import asyncio
import base64
import io
import logging
import shlex
import subprocess
import tarfile
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

from sandbox.overlay.engine.constants import (
    PROGRESS_POLL_INTERVAL_SECONDS,
    RUN_DIR_PREFIX,
)
from sandbox.overlay.engine.helpers import command_sample, runtime_command
from sandbox.overlay.engine.runtime_bundle import overlay_runtime_bundle_bytes
from sandbox.overlay.types import OverlayLease, OverlayRunError

logger = logging.getLogger(__name__)


class OverlayRunnerMixin:
    """Runtime upload and command execution for :class:`LocalOverlayEngine`."""

    async def _ensure_runtime_available(self, sandbox: Any) -> None:
        if self._script_uploaded:
            return
        async with self._script_upload_lock:
            if self._script_uploaded:
                return
            if self._can_use_local_run_dir(sandbox):
                root = Path(RUN_DIR_PREFIX)
                root.mkdir(parents=True, exist_ok=True)
                with tarfile.open(
                    fileobj=io.BytesIO(overlay_runtime_bundle_bytes()),
                    mode="r:gz",
                ) as tar:
                    try:
                        tar.extractall(root, filter="data")
                    except TypeError:
                        tar.extractall(root)
                self._script_uploaded = True
                return

            encoded = base64.b64encode(overlay_runtime_bundle_bytes()).decode("ascii")
            upload_snippet = (
                "import base64,io,pathlib,sys,tarfile; "
                "root=pathlib.Path(sys.argv[1]); "
                "root.mkdir(parents=True, exist_ok=True); "
                "data=base64.b64decode(sys.argv[2]); "
                "tar=tarfile.open(fileobj=io.BytesIO(data), mode='r:gz'); "
                "\ntry:\n tar.extractall(root, filter='data')"
                "\nexcept TypeError:\n tar.extractall(root)"
            )
            setup_cmd = (
                f"mkdir -p {shlex.quote(RUN_DIR_PREFIX)} && "
                f"python3 -c {shlex.quote(upload_snippet)} "
                f"{shlex.quote(RUN_DIR_PREFIX)} {shlex.quote(encoded)}"
            )
            _stdout, exit_code = await self._do_exec(sandbox, setup_cmd, timeout=60)
            if exit_code != 0:
                raise OverlayRunError(
                    f"overlay runtime upload failed: exit_code={exit_code}"
                )
            self._script_uploaded = True

    async def _run_overlay(
        self,
        sandbox: Any,
        *,
        lease: OverlayLease,
        user_cmd_b64: str,
        stdin_b64: str,
        timeout: int | None,
    ) -> tuple[str, int]:
        args = self._runtime_args(
            lease=lease,
            user_cmd_b64=user_cmd_b64,
            stdin_b64=stdin_b64,
        )
        inner = runtime_command(args)
        full = (
            f"mkdir -p {shlex.quote(lease.run_dir)} && "
            f"unshare -Urm bash -c {shlex.quote(inner)}"
        )
        return await self._do_exec(sandbox, full, timeout=timeout)

    async def _run_overlay_with_progress(
        self,
        sandbox: Any,
        *,
        lease: OverlayLease,
        user_cmd_b64: str,
        stdin_b64: str,
        timeout: int | None,
        on_progress_line: Callable[[str], None],
    ) -> tuple[str, int]:
        task = asyncio.create_task(
            self._run_overlay(
                sandbox,
                lease=lease,
                user_cmd_b64=user_cmd_b64,
                stdin_b64=stdin_b64,
                timeout=timeout,
            )
        )
        offset = 0
        partial = ""
        try:
            while not task.done():
                await asyncio.sleep(PROGRESS_POLL_INTERVAL_SECONDS)
                offset, partial = await self._emit_stdout_progress_delta(
                    sandbox,
                    lease,
                    offset=offset,
                    partial=partial,
                    on_progress_line=on_progress_line,
                )
            stdout_text, exit_code = await task
            offset, partial = await self._emit_stdout_progress_delta(
                sandbox,
                lease,
                offset=offset,
                partial=partial,
                on_progress_line=on_progress_line,
            )
            if partial:
                on_progress_line(partial)
            return stdout_text, exit_code
        except BaseException:
            if not task.done():
                task.cancel()
            raise

    async def _run_overlay_daemon_local(
        self,
        *,
        lease: OverlayLease,
        user_cmd_b64: str,
        stdin_b64: str,
        timeout: int | None,
    ) -> tuple[str, int]:
        Path(lease.run_dir).mkdir(parents=True, exist_ok=True)
        inner = runtime_command(
            self._runtime_args(
                lease=lease,
                user_cmd_b64=user_cmd_b64,
                stdin_b64=stdin_b64,
            )
        )
        argv = [
            "unshare",
            "-Urm",
            "bash",
            "-o",
            "pipefail",
            "-lc",
            self._daemon_local_shell_script(inner),
        ]
        logger.debug(
            "overlay daemon-local subprocess.run start: kind=unshare "
            "sandbox_id=%s run_dir=%s command=%r",
            self._sandbox_id,
            lease.run_dir,
            command_sample(inner),
        )
        started = time.perf_counter()
        completed = await asyncio.to_thread(
            subprocess.run,
            argv,
            text=True,
            capture_output=True,
            timeout=timeout,
            check=False,
        )
        logger.debug(
            "overlay daemon-local subprocess.run done: kind=unshare elapsed=%.3fs "
            "exit_code=%s sandbox_id=%s run_dir=%s",
            time.perf_counter() - started,
            completed.returncode,
            self._sandbox_id,
            lease.run_dir,
        )
        return (completed.stdout or "") + (completed.stderr or ""), completed.returncode

    def _runtime_args(
        self,
        *,
        lease: OverlayLease,
        user_cmd_b64: str,
        stdin_b64: str,
    ) -> list[str]:
        args = [
            "--workspace-root",
            self._workspace_root,
            "--run-dir",
            lease.run_dir,
            "--upper-size-mb",
            str(self._upper_size_mb),
            "--user-cmd-b64",
            user_cmd_b64,
        ]
        if stdin_b64:
            args.extend(["--stdin-b64", stdin_b64])
        return args

    def _daemon_local_shell_script(self, command: str) -> str:
        return "\n".join(
            [
                "unset LC_ALL",
                'export PATH="$HOME/.local/bin:$PATH"',
                f"cd {shlex.quote(self._workspace_root)}",
                'if [ -d .venv/bin ]; then export PATH="$PWD/.venv/bin:$PATH"; fi',
                f"exec {command}",
            ]
        )


__all__ = ["OverlayRunnerMixin"]
