"""Local and sandbox script transport for language-server queries."""

from __future__ import annotations

import base64
import logging
import shlex
import subprocess
from typing import TYPE_CHECKING, Any

from sandbox.daytona_utils import (
    _extract_exit_code,
    _wrap_bash_command,
)

from sandbox.async_bridge import run_sync
from sandbox.code_intelligence.core.constants import LSP_QUERY_TIMEOUT
from sandbox.code_intelligence.language_server.utils import _format_transport_exception

logger = logging.getLogger("sandbox.code_intelligence.language_server.client")


class LspTransportMixin:
    _workspace_root: str
    _sandbox: Any

    if TYPE_CHECKING:
        def _record_success(self) -> None: ...
        def _record_error(self) -> None: ...
        def _record_script_run(self) -> None: ...
        def _record_script_success(self) -> None: ...
        def _record_script_error(self) -> None: ...

    def _run_python_script(self, script: str) -> str:
        """Run a Python script locally or in the sandbox.

        For sandbox execution, base64 transport avoids marker-collision and
        shell-quoting edge cases while keeping the query to one ``process.exec``.
        """
        self._record_script_run()
        try:
            if self._sandbox:
                payload = base64.b64encode(script.encode("utf-8")).decode("ascii")
                cmd = f"echo {shlex.quote(payload)} | base64 -d | python3 -"
                response = run_sync(
                    self._sandbox.process.exec(
                        _wrap_bash_command(cmd),
                        timeout=int(LSP_QUERY_TIMEOUT),
                    )
                )
                result = response.result or ""
                result, exit_code = _extract_exit_code(
                    result,
                    fallback_exit_code=getattr(response, "exit_code", None),
                )
                if exit_code not in (0, None):
                    raise RuntimeError(result or "sandbox python LSP query failed")
            else:
                proc = subprocess.run(
                    [__import__("sys").executable, "-c", script],
                    capture_output=True,
                    text=True,
                    timeout=LSP_QUERY_TIMEOUT,
                    cwd=self._workspace_root or None,
                )
                result = proc.stdout
            self._record_success()
            self._record_script_success()
            return result.strip()
        except Exception as e:
            self._record_error()
            self._record_script_error()
            logger.debug(
                "LSP Python query failed: %s operation=python lsp query "
                "timeout=%ss workspace_root=%r",
                _format_transport_exception(e),
                int(LSP_QUERY_TIMEOUT),
                self._workspace_root,
            )
            return ""

    # -- Backend availability -------------------------------------------------

    def _check_python_backend(self) -> bool:
        return self._check_backend(
            local_cmd=["python3", "-c", "import jedi"],
            sandbox_cmd="python3 -c 'import jedi'",
        )

    def _check_backend(self, *, local_cmd: list[str], sandbox_cmd: str) -> bool:
        try:
            if self._sandbox:
                exit_code = self._run_sandbox_command_exit_code(sandbox_cmd, timeout=10)
                return exit_code == 0
            proc = subprocess.run(
                local_cmd,
                capture_output=True,
                timeout=10,
            )
            return proc.returncode == 0
        except Exception:
            return False

    def _install_python_backend(self) -> bool:
        if not self._sandbox:
            return False
        return self._run_sandbox_install(
            "python3 -m pip install --quiet --no-cache-dir jedi",
        )

    def _run_sandbox_install(self, command: str) -> bool:
        try:
            exit_code = self._run_sandbox_command_exit_code(command, timeout=120)
            return exit_code == 0
        except Exception:
            logger.debug("LSP backend install failed: %s", command, exc_info=True)
            return False

    def _run_sandbox_command_exit_code(self, command: str, *, timeout: int) -> int:
        """Run a sandbox command and recover its shell exit code."""
        response = run_sync(
            self._sandbox.process.exec(
                _wrap_bash_command(command),
                timeout=timeout,
            )
        )
        result = str(getattr(response, "result", "") or "")
        _cleaned, exit_code = _extract_exit_code(
            result,
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return exit_code

