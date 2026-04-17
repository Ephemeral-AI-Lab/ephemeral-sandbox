"""Clients for persistent Jedi worker processes.

Manages the lifecycle of a single worker process per :class:`LspClient`:
spawn lazily on first use, serialize requests under a lock, detect
crashes (EOF on stdout, JSON decode error), respawn once automatically,
then surrender to the caller's subprocess-per-call fallback.

Two transports are supported:

* ``JediWorkerClient`` runs a local subprocess over stdio.
* ``SandboxJediWorkerClient`` uploads the same worker into a sandbox,
  runs it as a daemon, and talks to it over a Unix-domain socket via
  small ``process.exec`` RPC calls.
"""

from __future__ import annotations

import json
import logging
import os
import shlex
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path
from typing import Any

from code_intelligence._async_bridge import run_sync
from tools.daytona_toolkit._daytona_utils import (
    _build_write_text_file_command,
    _extract_exit_code,
    _wrap_bash_command,
)

logger = logging.getLogger(__name__)

WORKER_SCRIPT = str(Path(__file__).with_name("_jedi_worker.py"))
ENV_FLAG = "CI_JEDI_WORKER_ENABLED"
RENAME_ENV_FLAG = "CI_JEDI_WORKER_RENAME_ENABLED"
_CRASH_BACKOFF_SEC = 30.0
_RPC_CLIENT_SOURCE = r"""
from __future__ import annotations

import json
import os
import socket
import sys

socket_path = sys.argv[1]
payload = sys.argv[2]
timeout = float(os.environ.get("CI_JEDI_WORKER_RPC_TIMEOUT", "10"))

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.settimeout(timeout)
try:
    sock.connect(socket_path)
    sock.sendall(payload.encode("utf-8") + b"\n")
    chunks = []
    while True:
        chunk = sock.recv(65536)
        if not chunk:
            break
        chunks.append(chunk)
        if chunk.endswith(b"\n"):
            break
finally:
    sock.close()

data = b"".join(chunks).decode("utf-8")
if not data:
    raise RuntimeError("empty response from jedi worker")
json.loads(data)
print(data.strip())
""".lstrip()


def is_enabled() -> bool:
    """Check the env-var kill-switch (default off)."""
    return os.environ.get(ENV_FLAG, "0").strip().lower() in {"1", "true", "yes", "on"}


def rename_is_enabled() -> bool:
    """Check the worker-backed rename kill-switch (default off)."""
    return os.environ.get(RENAME_ENV_FLAG, "0").strip().lower() in {"1", "true", "yes", "on"}


class WorkerUnavailable(RuntimeError):
    """Raised when the worker is dead and fallback must be used."""


class BaseJediWorkerClient:
    """Shared interface for local and sandbox worker clients."""

    def request(self, op: str, args: dict[str, Any] | None = None) -> Any:
        raise NotImplementedError

    def shutdown(self) -> None:
        raise NotImplementedError


class JediWorkerClient(BaseJediWorkerClient):
    """Owns one long-lived worker process.

    The client is safe to construct eagerly but only spawns on first
    :meth:`request`.
    """

    def __init__(
        self,
        workspace_root: str,
        *,
        worker_script: str | None = None,
        python_executable: str | None = None,
        request_timeout: float = 10.0,
    ) -> None:
        self._workspace_root = str(workspace_root or "")
        self._worker_script = worker_script or WORKER_SCRIPT
        self._python = python_executable or sys.executable or "python3"
        self._request_timeout = float(request_timeout)

        self._proc: subprocess.Popen[str] | None = None
        self._lock = threading.Lock()
        self._seq = 0
        self._crashes_in_window: list[float] = []
        self._dead_until: float = 0.0

    # -- Lifecycle ------------------------------------------------------------

    def _spawn(self) -> subprocess.Popen[str]:
        if self._dead_until > time.time():
            raise WorkerUnavailable("worker in crash-backoff")
        env = os.environ.copy()
        proc = subprocess.Popen(
            [self._python, self._worker_script, self._workspace_root],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
            env=env,
        )
        try:
            self._send_raw(proc, {"id": "ping", "op": "ping", "args": {}})
            response = self._read_raw(proc)
        except Exception as exc:
            self._kill(proc)
            raise WorkerUnavailable(f"worker ping failed: {exc}") from exc
        if not isinstance(response, dict) or not response.get("ok"):
            self._kill(proc)
            raise WorkerUnavailable(f"worker bad ping response: {response}")
        return proc

    def _ensure_proc(self) -> subprocess.Popen[str]:
        proc = self._proc
        if proc is not None and proc.poll() is None:
            return proc
        if proc is not None:
            self._record_crash()
        try:
            self._proc = self._spawn()
        except WorkerUnavailable:
            self._proc = None
            raise
        return self._proc

    def _record_crash(self) -> None:
        now = time.time()
        window_start = now - _CRASH_BACKOFF_SEC
        self._crashes_in_window = [t for t in self._crashes_in_window if t >= window_start]
        self._crashes_in_window.append(now)
        if len(self._crashes_in_window) >= 3:
            self._dead_until = now + _CRASH_BACKOFF_SEC
            logger.warning(
                "jedi worker crashed %d times in %.0fs — backing off until %.0f",
                len(self._crashes_in_window), _CRASH_BACKOFF_SEC, self._dead_until,
            )

    @staticmethod
    def _kill(proc: subprocess.Popen[str]) -> None:
        try:
            proc.kill()
        except Exception:
            pass
        try:
            proc.wait(timeout=1.0)
        except Exception:
            pass

    def shutdown(self) -> None:
        with self._lock:
            proc = self._proc
            self._proc = None
        if proc is None:
            return
        try:
            if proc.poll() is None:
                self._send_raw(proc, {"id": "bye", "op": "shutdown", "args": {}})
                proc.wait(timeout=2.0)
        except Exception:
            self._kill(proc)

    # -- Request plumbing -----------------------------------------------------

    @staticmethod
    def _send_raw(proc: subprocess.Popen[str], req: dict[str, Any]) -> None:
        assert proc.stdin is not None
        proc.stdin.write(json.dumps(req) + "\n")
        proc.stdin.flush()

    @staticmethod
    def _read_raw(proc: subprocess.Popen[str]) -> dict[str, Any]:
        assert proc.stdout is not None
        line = proc.stdout.readline()
        if not line:
            raise WorkerUnavailable("worker closed stdout (EOF)")
        try:
            return json.loads(line)
        except Exception as exc:
            raise WorkerUnavailable(f"worker emitted non-JSON: {line!r}") from exc

    def request(self, op: str, args: dict[str, Any] | None = None) -> Any:
        """Send one op and return ``result``. Raises :class:`WorkerUnavailable`.

        On a transient crash (EOF, decode error) this method attempts
        exactly one automatic respawn + retry. Persistent crashes latch
        the backoff and raise so the caller falls back to the subprocess
        path.
        """
        if not is_enabled():
            raise WorkerUnavailable("worker disabled (CI_JEDI_WORKER_ENABLED != 1)")
        payload = {"args": args or {}}
        with self._lock:
            for attempt in (0, 1):
                try:
                    proc = self._ensure_proc()
                    self._seq += 1
                    req_id = f"{op}-{self._seq}"
                    self._send_raw(proc, {"id": req_id, "op": op, **payload})
                    response = self._read_raw(proc)
                except WorkerUnavailable:
                    self._teardown_locked()
                    if attempt == 0:
                        continue
                    raise
                if not isinstance(response, dict):
                    self._teardown_locked()
                    if attempt == 0:
                        continue
                    raise WorkerUnavailable("malformed worker response")
                if not response.get("ok"):
                    raise RuntimeError(
                        f"worker op {op!r} failed: {response.get('error')}",
                    )
                return response.get("result")
        raise WorkerUnavailable("unreachable")  # pragma: no cover

    def _teardown_locked(self) -> None:
        proc = self._proc
        self._proc = None
        if proc is not None:
            self._kill(proc)


class SandboxJediWorkerClient(BaseJediWorkerClient):
    """Owns one long-lived Jedi worker process inside a sandbox.

    Daytona's public ``process.exec`` API is request/response oriented,
    so the daemon listens on a Unix-domain socket. Each logical LSP
    request executes a tiny Python socket client in the sandbox, while
    Jedi itself remains hot in the daemon process.
    """

    def __init__(
        self,
        workspace_root: str,
        *,
        sandbox: Any,
        worker_script: str | None = None,
        request_timeout: float = 10.0,
        remote_dir: str | None = None,
    ) -> None:
        self._workspace_root = str(workspace_root or "")
        self._sandbox = sandbox
        self._worker_script = worker_script or WORKER_SCRIPT
        self._request_timeout = float(request_timeout)
        digest = uuid.uuid4().hex[:10]
        self._remote_dir = remote_dir or f"/tmp/eos_jedi_{digest}"
        socket_dir = self._remote_dir
        if len(f"{socket_dir}/sock".encode("utf-8")) >= 100:
            socket_dir = f"/tmp/eos_jw_{digest}"
        self._socket_dir = socket_dir
        self._remote_worker = f"{self._remote_dir}/worker.py"
        self._remote_rpc = f"{self._remote_dir}/rpc.py"
        self._socket_path = f"{self._socket_dir}/sock"
        self._pid_path = f"{self._remote_dir}/pid"
        self._log_path = f"{self._remote_dir}/worker.log"

        self._lock = threading.Lock()
        self._seq = 0
        self._installed = False
        self._started = False
        self._crashes_in_window: list[float] = []
        self._dead_until: float = 0.0

    def _exec(self, command: str, *, timeout: float | None = None) -> str:
        process = getattr(self._sandbox, "process", None)
        exec_fn = getattr(process, "exec", None) if process is not None else None
        if not callable(exec_fn):
            raise WorkerUnavailable("sandbox process.exec unavailable")
        try:
            response = run_sync(
                exec_fn(
                    _wrap_bash_command(command),
                    timeout=int(timeout or self._request_timeout),
                )
            )
        except Exception as exc:
            raise WorkerUnavailable(f"sandbox exec failed: {exc}") from exc
        stdout = getattr(response, "result", "") or ""
        cleaned, exit_code = _extract_exit_code(
            stdout,
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        if exit_code not in (0, None):
            raise WorkerUnavailable(cleaned or f"sandbox exec exit {exit_code}")
        return cleaned

    def _ensure_installed(self) -> None:
        if self._installed:
            return
        worker_source = Path(self._worker_script).read_text(encoding="utf-8")
        self._exec(
            " && ".join(
                [
                    f"mkdir -p {shlex.quote(self._remote_dir)}",
                    _build_write_text_file_command(self._remote_worker, worker_source),
                    _build_write_text_file_command(self._remote_rpc, _RPC_CLIENT_SOURCE),
                    f"chmod 700 {shlex.quote(self._remote_dir)}",
                ]
            ),
            timeout=15,
        )
        self._installed = True

    def _spawn(self) -> None:
        if self._dead_until > time.time():
            raise WorkerUnavailable("worker in crash-backoff")
        self._ensure_installed()
        cmd = f"""
set +e
mkdir -p {shlex.quote(self._remote_dir)}
mkdir -p {shlex.quote(self._socket_dir)}
if [ -f {shlex.quote(self._pid_path)} ] \
   && kill -0 "$(cat {shlex.quote(self._pid_path)})" 2>/dev/null \
   && [ -S {shlex.quote(self._socket_path)} ]; then
    echo READY=1
    exit 0
fi
rm -f {shlex.quote(self._socket_path)}
nohup python3 {shlex.quote(self._remote_worker)} --socket {shlex.quote(self._socket_path)} {shlex.quote(self._workspace_root)} > {shlex.quote(self._log_path)} 2>&1 &
echo "$!" > {shlex.quote(self._pid_path)}
for _i in $(seq 1 50); do
    if [ -S {shlex.quote(self._socket_path)} ]; then
        echo READY=1
        exit 0
    fi
    if ! kill -0 "$(cat {shlex.quote(self._pid_path)})" 2>/dev/null; then
        break
    fi
    sleep 0.1
done
echo READY=0
echo "--- worker log ---"
cat {shlex.quote(self._log_path)} || true
exit 0
"""
        startup = self._exec(cmd, timeout=15)
        if "READY=1" not in startup.splitlines():
            detail = startup.strip() or "worker socket was not created"
            raise WorkerUnavailable(detail)
        response = self._rpc_raw({"id": "ping", "op": "ping", "args": {}}, timeout=5)
        if not isinstance(response, dict) or not response.get("ok"):
            self._started = False
            raise WorkerUnavailable(f"worker bad ping response: {response}")
        self._started = True

    def _ensure_started(self) -> None:
        if self._started:
            return
        self._spawn()

    def _record_crash(self) -> None:
        now = time.time()
        window_start = now - _CRASH_BACKOFF_SEC
        self._crashes_in_window = [t for t in self._crashes_in_window if t >= window_start]
        self._crashes_in_window.append(now)
        if len(self._crashes_in_window) >= 3:
            self._dead_until = now + _CRASH_BACKOFF_SEC
            logger.warning(
                "sandbox jedi worker crashed %d times in %.0fs — backing off until %.0f",
                len(self._crashes_in_window), _CRASH_BACKOFF_SEC, self._dead_until,
            )

    def _rpc_raw(self, req: dict[str, Any], *, timeout: float | None = None) -> dict[str, Any]:
        payload = json.dumps(req, separators=(",", ":"))
        output = self._exec(
            (
                f"CI_JEDI_WORKER_RPC_TIMEOUT={shlex.quote(str(timeout or self._request_timeout))} "
                f"python3 {shlex.quote(self._remote_rpc)} "
                f"{shlex.quote(self._socket_path)} {shlex.quote(payload)}"
            ),
            timeout=(timeout or self._request_timeout) + 2,
        )
        try:
            return json.loads(output.strip())
        except Exception as exc:
            raise WorkerUnavailable(f"worker emitted non-JSON: {output!r}") from exc

    def request(self, op: str, args: dict[str, Any] | None = None) -> Any:
        if not is_enabled():
            raise WorkerUnavailable("worker disabled (CI_JEDI_WORKER_ENABLED != 1)")
        payload = {"args": args or {}}
        with self._lock:
            for attempt in (0, 1):
                try:
                    self._ensure_started()
                    self._seq += 1
                    req_id = f"{op}-{self._seq}"
                    response = self._rpc_raw({"id": req_id, "op": op, **payload})
                except WorkerUnavailable:
                    self._started = False
                    self._record_crash()
                    if attempt == 0:
                        continue
                    raise
                if not isinstance(response, dict):
                    self._started = False
                    if attempt == 0:
                        continue
                    raise WorkerUnavailable("malformed worker response")
                if not response.get("ok"):
                    raise RuntimeError(
                        f"worker op {op!r} failed: {response.get('error')}",
                    )
                return response.get("result")
        raise WorkerUnavailable("unreachable")  # pragma: no cover

    def shutdown(self) -> None:
        with self._lock:
            started = self._started
            self._started = False
        if not started:
            return
        try:
            self._rpc_raw({"id": "bye", "op": "shutdown", "args": {}}, timeout=2)
        except Exception:
            try:
                self._exec(
                    (
                        f"if [ -f {shlex.quote(self._pid_path)} ]; then "
                        f"kill \"$(cat {shlex.quote(self._pid_path)})\" 2>/dev/null || true; "
                        "fi"
                    ),
                    timeout=5,
                )
            except Exception:
                pass
