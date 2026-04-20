"""Unit tests for the code intelligence LSP client."""

from __future__ import annotations

import base64
import concurrent.futures
import logging
import re
import threading
import time
from types import SimpleNamespace

from code_intelligence.lsp.client import LspClient
from code_intelligence.types import SymbolKind


def _decode_sandbox_python_payload(command: str) -> str:
    match = re.search(
        r"echo (?P<payload>[A-Za-z0-9+/=]+) \| base64 -d \| python3 -",
        command,
    )
    assert match is not None, command
    return base64.b64decode(match.group("payload")).decode("utf-8")


def _sandbox_exit_result(exit_code: int, stdout: str = "") -> SimpleNamespace:
    return SimpleNamespace(
        exit_code=None,
        result=f"{stdout}\n__CODEX_EXIT_CODE__={exit_code}\n",
    )


def test_python_definitions_maps_known_symbol_kind(monkeypatch) -> None:
    lsp = LspClient(workspace_root="/workspace")
    monkeypatch.setattr(
        lsp,
        "_run_python_script",
        lambda script: (
            '[{"name":"demo","path":"/workspace/demo.py","line":7,"col":2,"type":"function"}]'
        ),
    )

    results = lsp._python_definitions("/workspace/demo.py", 7, 2)

    assert len(results) == 1
    assert results[0].kind is SymbolKind.FUNCTION


def test_python_definitions_preserves_unknown_types(monkeypatch) -> None:
    lsp = LspClient(workspace_root="/workspace")
    monkeypatch.setattr(
        lsp,
        "_run_python_script",
        lambda script: (
            '[{"name":"demo","path":"/workspace/demo.py","line":7,"col":2,"type":"statement"}]'
        ),
    )

    results = lsp._python_definitions("/workspace/demo.py", 7, 2)

    assert len(results) == 1
    assert results[0].kind is SymbolKind.UNKNOWN


def test_python_definitions_follows_imports_in_subprocess_path(monkeypatch) -> None:
    captured_scripts: list[str] = []
    lsp = LspClient(workspace_root="/workspace")

    def _capture(script: str) -> str:
        captured_scripts.append(script)
        return "[]"

    monkeypatch.setattr(lsp, "_run_python_script", _capture)

    lsp._python_definitions("/workspace/pkg/uses.py", 4, 11)

    assert captured_scripts
    assert "follow_imports=True" in captured_scripts[0]


def test_reset_backend_availability_clears_cached_readiness() -> None:
    lsp = LspClient(workspace_root="/workspace")
    lsp._py_available = False
    lsp._ts_available = True

    lsp.reset_backend_availability()

    assert lsp._py_available is None
    assert lsp._ts_available is None


def test_cached_query_singleflights_concurrent_misses() -> None:
    lsp = LspClient(workspace_root="/workspace")
    calls = 0
    calls_lock = threading.Lock()

    def loader() -> str:
        nonlocal calls
        with calls_lock:
            calls += 1
        time.sleep(0.05)
        return "resolved"

    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
        results = list(
            executor.map(lambda _: lsp._run_cached_query("same-key", loader), range(8))
        )

    assert results == ["resolved"] * 8
    assert calls == 1


def test_sandbox_read_line_caches_until_invalidate() -> None:
    calls: list[str] = []

    class _SandboxProcess:
        def exec(self, command: str, timeout: int = 0):
            calls.append(command)
            return SimpleNamespace(exit_code=0, result="def alpha(value):\n")

    sandbox = SimpleNamespace(process=_SandboxProcess())
    lsp = LspClient(workspace_root="/workspace", sandbox=sandbox)

    assert lsp._read_line("/workspace/pkg/core.py", 1) == "def alpha(value):\n"
    assert lsp._read_line("/workspace/pkg/core.py", 1) == "def alpha(value):\n"
    assert len(calls) == 1

    lsp.invalidate("/workspace/pkg/core.py")

    assert lsp._read_line("/workspace/pkg/core.py", 1) == "def alpha(value):\n"
    assert len(calls) == 2


def test_resolve_path_prepends_workspace_root() -> None:
    lsp = LspClient(workspace_root="/testbed")
    assert lsp._resolve_path("dask/core.py") == "/testbed/dask/core.py"


def test_resolve_path_leaves_absolute_unchanged() -> None:
    lsp = LspClient(workspace_root="/testbed")
    assert lsp._resolve_path("/testbed/dask/core.py") == "/testbed/dask/core.py"


def test_resolve_path_no_workspace_root_keeps_relative() -> None:
    lsp = LspClient(workspace_root="")
    assert lsp._resolve_path("dask/core.py") == "dask/core.py"


def test_sandbox_exec_runs_script_with_base64_pipe() -> None:
    """Verify _run_python_script preserves newlines without a temp file write."""
    captured_cmds: list[str] = []

    class _SandboxProcess:
        def exec(self, command: str, timeout: int = 0):
            captured_cmds.append(command)
            return SimpleNamespace(exit_code=0, result="[]")

    sandbox = SimpleNamespace(process=_SandboxProcess())
    lsp = LspClient(workspace_root="/testbed", sandbox=sandbox)

    lsp._run_python_script("print('hello')")

    assert len(captured_cmds) == 1
    assert "base64 -d | python3 -" in captured_cmds[0]
    assert _decode_sandbox_python_payload(captured_cmds[0]) == "print('hello')"
    assert lsp.telemetry.script_runs == 1
    assert lsp.telemetry.script_successes == 1


def test_sandbox_exec_logs_empty_daytona_exception_context(caplog) -> None:
    class _SandboxProcess:
        def exec(self, command: str, timeout: int = 0):
            raise RuntimeError("Failed to execute command: ")

    sandbox = SimpleNamespace(process=_SandboxProcess())
    lsp = LspClient(workspace_root="/testbed", sandbox=sandbox)
    caplog.set_level(logging.DEBUG, logger="code_intelligence.lsp.client")

    assert lsp._run_python_script("print('hello')") == ""

    assert lsp.telemetry.script_errors == 1
    assert (
        "Failed to execute command: (no additional detail from Daytona SDK)"
        in caplog.text
    )
    assert "[exception_type=RuntimeError]" in caplog.text
    assert "operation=python lsp query" in caplog.text
    assert "workspace_root='/testbed'" in caplog.text


def test_sandbox_exec_no_cd_without_workspace_root() -> None:
    """Without workspace_root, no cd prefix is added."""
    captured_cmds: list[str] = []

    class _SandboxProcess:
        def exec(self, command: str, timeout: int = 0):
            captured_cmds.append(command)
            return SimpleNamespace(exit_code=0, result="[]")

    sandbox = SimpleNamespace(process=_SandboxProcess())
    lsp = LspClient(workspace_root="", sandbox=sandbox)

    lsp._run_python_script("print('hello')")

    assert len(captured_cmds) == 1
    assert "base64 -d | python3 -" in captured_cmds[0]
    assert _decode_sandbox_python_payload(captured_cmds[0]) == "print('hello')"
    assert "cd " not in captured_cmds[0]


def test_ensure_ready_can_probe_only_python_backend() -> None:
    class _SandboxProcess:
        def __init__(self) -> None:
            self.calls: list[str] = []

        def exec(self, command: str, timeout: int = 0):
            self.calls.append(command)
            if "import jedi" in command:
                return _sandbox_exit_result(0)
            raise AssertionError(f"unexpected command: {command}")

    process = _SandboxProcess()
    lsp = LspClient(workspace_root="/workspace", sandbox=SimpleNamespace(process=process))

    readiness = lsp.ensure_ready(languages=("python",))

    assert readiness == {"python": True, "typescript": False}
    assert len(process.calls) == 1
    assert "import jedi" in process.calls[0]
    assert "__CODEX_EXIT_CODE__" in process.calls[0]


def test_connected_does_not_probe_typescript_backend() -> None:
    class _SandboxProcess:
        def __init__(self) -> None:
            self.calls: list[str] = []

        def exec(self, command: str, timeout: int = 0):
            self.calls.append(command)
            if "import jedi" in command:
                return _sandbox_exit_result(0)
            raise AssertionError(f"unexpected command: {command}")

    process = _SandboxProcess()
    lsp = LspClient(workspace_root="/workspace", sandbox=SimpleNamespace(process=process))

    assert lsp.connected is True
    assert len(process.calls) == 1
    assert "import jedi" in process.calls[0]
    assert "__CODEX_EXIT_CODE__" in process.calls[0]


def test_python_hover_uses_resolved_path(monkeypatch) -> None:
    """Verify hover resolves relative path before injecting into Jedi script."""
    captured_scripts: list[str] = []
    lsp = LspClient(workspace_root="/testbed")

    def _capture(script: str) -> str:
        captured_scripts.append(script)
        return "null"

    monkeypatch.setattr(lsp, "_run_python_script", _capture)

    lsp._python_hover("dask/groupby.py", 100, 0)

    assert len(captured_scripts) == 1
    assert "/testbed/dask/groupby.py" in captured_scripts[0]
    assert "path='dask/groupby.py'" not in captured_scripts[0]


def test_ensure_ready_installs_missing_sandbox_deps() -> None:
    class _SandboxProcess:
        def __init__(self) -> None:
            self.calls: list[str] = []

        def exec(self, command: str, timeout: int = 0):
            self.calls.append(command)
            if "import jedi" in command:
                return _sandbox_exit_result(1)
            if "npx tsc --version" in command:
                return _sandbox_exit_result(1)
            if "python3 -m pip install --quiet --no-cache-dir jedi" in command:
                return _sandbox_exit_result(0)
            if "node -e \"require('typescript')\"" in command:
                return SimpleNamespace(exit_code=0, result="missing\n")
            if "npm install --global --quiet typescript" in command:
                return _sandbox_exit_result(0)
            raise AssertionError(f"unexpected command: {command}")

    class _SandboxFs:
        def upload_file(self, content: bytes, path: str):
            pass

    sandbox = SimpleNamespace(process=_SandboxProcess(), fs=_SandboxFs())
    lsp = LspClient(workspace_root="/workspace", sandbox=sandbox)

    readiness = lsp.ensure_ready(install_missing=True)

    assert readiness == {"python": True, "typescript": True}
    assert any(
        "python3 -m pip install --quiet --no-cache-dir jedi" in command
        and "__CODEX_EXIT_CODE__" in command
        for command in sandbox.process.calls
    )
