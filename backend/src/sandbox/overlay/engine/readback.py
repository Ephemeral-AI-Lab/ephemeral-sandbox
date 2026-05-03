"""Overlay run readback, cleanup, and instrumentation helpers."""

from __future__ import annotations

import asyncio
import base64
import json
import logging
import posixpath
import shlex
import shutil
import subprocess
import time
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

from sandbox.api.bash import extract_exit_code, wrap_bash_command
from sandbox.overlay.engine.constants import (
    PROGRESS_READ_CHUNK_BYTES,
    SLOW_OVERLAY_STAGE_SECONDS,
    SLOW_OVERLAY_TOTAL_SECONDS,
)
from sandbox.overlay.engine.fingerprint import workspace_fingerprint
from sandbox.overlay.engine.helpers import command_sample
from sandbox.overlay.types import (
    OverlayCapture,
    OverlayLease,
    OverlayPolicyReject,
    OverlayRunError,
    OverlayRunOutcome,
)
from sandbox.overlay.wire import parse_diff_ndjson

logger = logging.getLogger(__name__)


class OverlayReadbackMixin:
    """Read runtime artifacts and manage local/remote cleanup."""

    async def _emit_stdout_progress_delta(
        self,
        sandbox: Any,
        lease: OverlayLease,
        *,
        offset: int,
        partial: str,
        on_progress_line: Callable[[str], None],
    ) -> tuple[int, str]:
        try:
            chunk, new_offset = await self._read_stdout_delta(
                sandbox,
                lease,
                offset=offset,
                max_bytes=PROGRESS_READ_CHUNK_BYTES,
            )
        except Exception:
            logger.debug(
                "overlay stdout progress read failed for %s",
                lease.run_dir,
                exc_info=True,
            )
            return offset, partial
        if not chunk:
            return new_offset, partial
        text = partial + chunk.decode("utf-8", "replace")
        if text.endswith(("\n", "\r")):
            emit_text = text
            partial = ""
        else:
            lines = text.splitlines(keepends=True)
            partial = lines[-1] if lines else text
            emit_text = "".join(lines[:-1]) if lines else ""
        if emit_text:
            on_progress_line(emit_text)
        return new_offset, partial

    async def _read_stdout(
        self, sandbox: Any, lease: OverlayLease, *, fallback: str
    ) -> str:
        stdout_path = posixpath.join(lease.run_dir, "stdout.bin")
        if self._can_use_local_run_dir(sandbox):
            try:
                return Path(stdout_path).read_bytes().decode("utf-8", "replace")
            except OSError:
                return fallback
        script = (
            "import base64,pathlib,sys; "
            "sys.stdout.write(base64.b64encode(pathlib.Path(sys.argv[1]).read_bytes()).decode('ascii'))"
        )
        cmd = f"python3 -c {shlex.quote(script)} {shlex.quote(stdout_path)}"
        encoded, exit_code = await self._do_exec(sandbox, cmd, timeout=60)
        if exit_code != 0:
            return fallback
        try:
            return base64.b64decode(encoded.strip()).decode("utf-8", "replace")
        except Exception:
            logger.debug("overlay stdout decode failed for %s", stdout_path, exc_info=True)
            return fallback

    async def _read_stdout_delta(
        self,
        sandbox: Any,
        lease: OverlayLease,
        *,
        offset: int,
        max_bytes: int,
    ) -> tuple[bytes, int]:
        stdout_path = posixpath.join(lease.run_dir, "stdout.bin")
        if self._can_use_local_run_dir(sandbox):
            try:
                data = Path(stdout_path).read_bytes()
            except OSError:
                return b"", offset
            size = len(data)
            start = offset if offset <= size else 0
            start = max(start, size - max_bytes)
            return data[start:size], size
        script = (
            "import base64,json,pathlib,sys; "
            "path=pathlib.Path(sys.argv[1]); "
            "offset=max(0,int(sys.argv[2])); "
            "limit=max(1,int(sys.argv[3])); "
            "data=path.read_bytes() if path.exists() else b''; "
            "size=len(data); "
            "start=offset if offset <= size else 0; "
            "start=max(start, size-limit); "
            "chunk=data[start:size]; "
            "print(json.dumps({'start': start, 'size': size, "
            "'chunk': base64.b64encode(chunk).decode('ascii')}))"
        )
        cmd = (
            f"python3 -c {shlex.quote(script)} "
            f"{shlex.quote(stdout_path)} {offset} {max_bytes}"
        )
        raw, exit_code = await self._do_exec(sandbox, cmd, timeout=60)
        if exit_code != 0:
            return b"", offset
        payload = json.loads(raw or "{}")
        size = int(payload.get("size") or 0)
        chunk_b64 = str(payload.get("chunk") or "")
        if not chunk_b64:
            return b"", size
        return base64.b64decode(chunk_b64), size

    async def _read_diff(
        self,
        sandbox: Any,
        lease: OverlayLease,
        *,
        overlay_stdout: str = "",
        overlay_exit_code: int | None = None,
    ) -> OverlayCapture | OverlayPolicyReject:
        diff_path = posixpath.join(lease.run_dir, "diff.ndjson")
        if self._can_use_local_run_dir(sandbox):
            try:
                return parse_diff_ndjson(Path(diff_path).read_text(encoding="utf-8"))
            except OSError as exc:
                raise OverlayRunError(
                    "overlay diff.ndjson missing at "
                    f"{diff_path}: {exc} overlay_exit_code={overlay_exit_code!r} "
                    f"overlay_output={overlay_stdout[-2000:]!r}"
                ) from exc
        cmd = f"cat {shlex.quote(diff_path)}"
        stdout, exit_code = await self._do_exec(sandbox, cmd, timeout=60)
        if exit_code != 0:
            raise OverlayRunError(
                "overlay diff.ndjson missing at "
                f"{diff_path}: cat={stdout[-1000:]!r} "
                f"overlay_exit_code={overlay_exit_code!r} "
                f"overlay_output={overlay_stdout[-2000:]!r}"
            )
        return parse_diff_ndjson(stdout)

    async def _read_result_envelope(
        self,
        lease: OverlayLease,
        *,
        overlay_stdout: str,
        overlay_exit_code: int,
    ) -> dict[str, Any]:
        path = Path(lease.run_dir) / "result.json"
        try:
            raw = path.read_text(encoding="utf-8")
        except OSError as exc:
            raise OverlayRunError(
                "overlay result.json missing at "
                f"{path}: {exc} overlay_exit_code={overlay_exit_code!r} "
                f"overlay_output={overlay_stdout[-2000:]!r}"
            ) from exc
        try:
            payload = json.loads(raw)
        except json.JSONDecodeError as exc:
            raise OverlayRunError(f"invalid overlay result.json at {path}: {exc}") from exc
        if not isinstance(payload, dict):
            raise OverlayRunError(
                f"overlay result.json at {path} must be an object: {payload!r}"
            )
        logger.debug(
            "overlay direct-runtime result envelope read: sandbox_id=%s run_dir=%s "
            "exit_code=%s rejected=%s",
            self._sandbox_id,
            lease.run_dir,
            payload.get("exit_code"),
            payload.get("rejected"),
        )
        return payload

    async def _cleanup_run_dir(self, sandbox: Any, lease: OverlayLease) -> None:
        if self._can_use_local_run_dir(sandbox):
            await asyncio.to_thread(shutil.rmtree, lease.run_dir, ignore_errors=True)
            return
        await self._do_exec(sandbox, f"rm -rf {shlex.quote(lease.run_dir)}", timeout=60)

    def _can_use_local_run_dir(self, sandbox: Any) -> bool:
        return sandbox is None

    async def _do_exec(
        self,
        sandbox: Any,
        command: str,
        *,
        timeout: int | None,
    ) -> tuple[str, int]:
        """Exec ``command`` and return ``(stdout, exit_code)``."""
        response = await self._exec_process(
            sandbox, wrap_bash_command(command), timeout=timeout
        )
        cleaned, exit_code = extract_exit_code(
            str(getattr(response, "result", "") or ""),
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return cleaned, exit_code

    async def _begin_workspace_fingerprint_guard(self) -> None:
        async with self._fingerprint_lock:
            if self._active_fingerprint_guards == 0:
                current = workspace_fingerprint(self._workspace_root)
                previous = self._last_workspace_fingerprint
                if previous is not None and current != previous:
                    raise OverlayRunError(
                        "workspace changed outside the overlay OCC path; "
                        "refusing lowerdir snapshot"
                    )
            self._active_fingerprint_guards += 1

    async def _end_workspace_fingerprint_guard(self) -> None:
        async with self._fingerprint_lock:
            if self._active_fingerprint_guards > 0:
                self._active_fingerprint_guards -= 1
            if self._active_fingerprint_guards == 0:
                self._last_workspace_fingerprint = workspace_fingerprint(
                    self._workspace_root
                )

    async def _timed_stage(
        self,
        stage: str,
        *,
        stage_timings: dict[str, float],
        lease: OverlayLease,
        command: str,
        awaitable: Awaitable[Any],
    ) -> Any:
        started = time.perf_counter()
        logger.debug(
            "overlay command stage start: stage=%s sandbox_id=%s run_dir=%s command=%r",
            stage,
            self._sandbox_id,
            lease.run_dir,
            command_sample(command),
        )
        try:
            return await awaitable
        finally:
            elapsed = round(time.perf_counter() - started, 6)
            stage_timings[stage] = elapsed
            logger.debug(
                "overlay command stage done: stage=%s elapsed=%.3fs "
                "sandbox_id=%s run_dir=%s command=%r timings=%s",
                stage,
                elapsed,
                self._sandbox_id,
                lease.run_dir,
                command_sample(command),
                dict(stage_timings),
            )
            if elapsed >= SLOW_OVERLAY_STAGE_SECONDS:
                logger.warning(
                    "overlay command stage slow: stage=%s elapsed=%.3fs "
                    "sandbox_id=%s run_dir=%s command=%r timings=%s",
                    stage,
                    elapsed,
                    self._sandbox_id,
                    lease.run_dir,
                    command_sample(command),
                    dict(stage_timings),
                )

    def _log_execution_summary(
        self,
        *,
        command: str,
        lease: OverlayLease,
        stage_timings: dict[str, float],
        outcome: OverlayRunOutcome | None,
        error: BaseException | None,
    ) -> None:
        total = stage_timings.get("total", 0.0)
        rejected = bool(outcome and outcome.overlay_rejected)
        conflict = outcome.conflict if outcome is not None else None
        failed = rejected or conflict is not None
        if error is None and not failed and total < SLOW_OVERLAY_TOTAL_SECONDS:
            return
        error_text = f"{type(error).__name__}: {error}" if error is not None else None
        logger.warning(
            "overlay command summary: total=%.3fs rejected=%s exit_code=%s "
            "conflict_file=%s conflict_reason=%s error=%s sandbox_id=%s "
            "run_dir=%s timings=%s overlay_run_timings=%s command=%r",
            total,
            rejected,
            getattr(outcome, "exit_code", None),
            getattr(conflict, "conflict_file", None),
            getattr(conflict, "reason", None),
            error_text,
            self._sandbox_id,
            lease.run_dir,
            dict(stage_timings),
            dict(getattr(outcome, "overlay_run_timings", {}) or {}),
            command_sample(command),
        )

    async def _local_exec_process(
        self,
        _sandbox: Any,
        command: str,
        *,
        timeout: int | None,
    ) -> Any:
        completed = await asyncio.to_thread(
            subprocess.run,
            command,
            shell=True,
            text=True,
            capture_output=True,
            timeout=timeout,
            check=False,
        )
        return type(
            "LocalProcessResult",
            (),
            {
                "result": completed.stdout + completed.stderr,
                "exit_code": completed.returncode,
            },
        )()


__all__ = ["OverlayReadbackMixin"]
