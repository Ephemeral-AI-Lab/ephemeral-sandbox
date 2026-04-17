"""Unit tests for the code intelligence LSP client."""

from __future__ import annotations

from types import SimpleNamespace

from code_intelligence.lsp._jedi_worker_client import (
    ENV_FLAG,
    RENAME_ENV_FLAG,
    WorkerUnavailable,
)
from code_intelligence.lsp.client import LspClient
from code_intelligence.types import SymbolKind


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


def test_sandbox_read_line_caches_until_invalidate(monkeypatch) -> None:
    monkeypatch.delenv(ENV_FLAG, raising=False)
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


def test_worker_telemetry_tracks_success_and_fallback(monkeypatch) -> None:
    monkeypatch.setenv(ENV_FLAG, "1")

    class _OkWorker:
        def request(self, op, args=None):
            return {"op": op, "args": args}

        def shutdown(self):
            pass

    class _UnavailableWorker:
        def request(self, op, args=None):
            raise WorkerUnavailable("dead")

        def shutdown(self):
            pass

    lsp = LspClient(workspace_root="/workspace")
    lsp._worker = _OkWorker()  # type: ignore[assignment]
    used, result = lsp._try_worker("ping", {"x": 1})
    assert used is True
    assert result == {"op": "ping", "args": {"x": 1}}
    assert lsp.telemetry.worker_successes == 1

    lsp._worker = _UnavailableWorker()  # type: ignore[assignment]
    used, result = lsp._try_worker("ping", {})
    assert used is False
    assert result is None
    assert lsp.telemetry.worker_fallbacks == 1


def test_python_rename_bypasses_worker_without_rename_flag(monkeypatch) -> None:
    monkeypatch.setenv(ENV_FLAG, "1")
    monkeypatch.delenv(RENAME_ENV_FLAG, raising=False)
    captured_scripts: list[str] = []

    class _UnexpectedWorker:
        def request(self, op, args=None):
            raise AssertionError("rename worker should be disabled by default")

        def shutdown(self):
            pass

    lsp = LspClient(workspace_root="/workspace")
    lsp._worker = _UnexpectedWorker()  # type: ignore[assignment]

    def _capture(script: str) -> str:
        captured_scripts.append(script)
        return "{}"

    monkeypatch.setattr(lsp, "_run_python_script", _capture)

    assert lsp._python_rename("/workspace/pkg/core.py", 5, 4, "beta_v2") == {}
    assert captured_scripts
    assert lsp.telemetry.worker_successes == 0


def test_resolve_path_prepends_workspace_root() -> None:
    lsp = LspClient(workspace_root="/testbed")
    assert lsp._resolve_path("dask/core.py") == "/testbed/dask/core.py"


def test_resolve_path_leaves_absolute_unchanged() -> None:
    lsp = LspClient(workspace_root="/testbed")
    assert lsp._resolve_path("/testbed/dask/core.py") == "/testbed/dask/core.py"


def test_resolve_path_no_workspace_root_keeps_relative() -> None:
    lsp = LspClient(workspace_root="")
    assert lsp._resolve_path("dask/core.py") == "dask/core.py"


def test_sandbox_exec_writes_script_and_runs_temp_file() -> None:
    """Verify _run_python_script writes to a temp file before sandbox execution."""
    captured_cmds: list[str] = []

    class _SandboxProcess:
        def exec(self, command: str, timeout: int = 0):
            captured_cmds.append(command)
            return SimpleNamespace(exit_code=0, result="[]")

    sandbox = SimpleNamespace(process=_SandboxProcess())
    lsp = LspClient(workspace_root="/testbed", sandbox=sandbox)

    lsp._run_python_script("print('hello')")

    assert len(captured_cmds) == 2
    assert "/tmp/_lsp_query_" in captured_cmds[0]
    assert captured_cmds[1].startswith("python3 /tmp/_lsp_query_")


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

    assert len(captured_cmds) == 2
    assert captured_cmds[1].startswith("python3 /tmp/_lsp_query_")
    assert "cd " not in captured_cmds[1]


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
            if command == "python3 -c 'import jedi'":
                return SimpleNamespace(exit_code=1, result="")
            if command == "npx tsc --version":
                return SimpleNamespace(exit_code=1, result="")
            if command == "pip install --quiet --no-cache-dir jedi":
                return SimpleNamespace(exit_code=0, result="")
            if "node -e \"require('typescript')\"" in command:
                return SimpleNamespace(exit_code=0, result="missing\n")
            if command == "npm install --global --quiet typescript":
                return SimpleNamespace(exit_code=0, result="")
            raise AssertionError(f"unexpected command: {command}")

    class _SandboxFs:
        def upload_file(self, content: bytes, path: str):
            pass

    sandbox = SimpleNamespace(process=_SandboxProcess(), fs=_SandboxFs())
    lsp = LspClient(workspace_root="/workspace", sandbox=sandbox)

    readiness = lsp.ensure_ready(install_missing=True)

    assert readiness == {"python": True, "typescript": True}
