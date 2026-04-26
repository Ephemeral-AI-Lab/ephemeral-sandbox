"""Live E2E coverage for ci_diagnostics against a real Daytona sandbox.

Run with:
    uv run pytest backend/tests/test_e2e/test_live_ci_diagnostics.py -m live -v -s
"""

from __future__ import annotations

import json
import shlex
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest

from code_intelligence.service import CodeIntelligenceService
from engine.testing.eval_agent import EvalAgent
from tools.ci_toolkit.ci_diagnostics import ci_diagnostics
from tools.core.base import ToolExecutionContextService, ToolResult
from sandbox.daytona_utils import _extract_exit_code, _wrap_bash_command
from tools.daytona_toolkit.write_file import write_file

pytestmark = [pytest.mark.e2e, pytest.mark.live]

_DASK_SWEEVO_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"
_DASK_SWEEVO_REPO_DIR = "/testbed"


@dataclass
class LiveCiDiagnosticsEnv:
    sandbox_id: str
    raw_sandbox: Any
    home: str
    root_dir: str

    def exec(self, command: str, *, timeout: int = 60) -> tuple[int, str]:
        response = self.raw_sandbox.process.exec(
            _wrap_bash_command(command),
            timeout=timeout,
        )
        output, exit_code = _extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return exit_code, output

    def make_ci_service(self) -> CodeIntelligenceService:
        return CodeIntelligenceService(
            sandbox_id=self.sandbox_id,
            workspace_root=self.root_dir,
            sandbox=self.raw_sandbox,
        )

    def make_ctx(self, ci_service: CodeIntelligenceService) -> ToolExecutionContextService:
        return ToolExecutionContextService(
            cwd=Path(self.root_dir),
            services={
                "sandbox_id": self.sandbox_id,
                "repo_root": self.root_dir,
                "ci_sandbox": self.raw_sandbox,
                "ci_service": ci_service,
                "agent_run_id": f"ci-diagnostics-{uuid.uuid4().hex[:8]}",
            },
        )


@pytest.fixture
def live_ci_diagnostics_env() -> LiveCiDiagnosticsEnv:
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials not configured")

    from sandbox.testing import create_test_sandbox, delete_test_sandbox, get_sandbox_service

    info = create_test_sandbox(name="ci-diagnostics-live")
    sandbox_id = info["id"]
    try:
        raw_sandbox = get_sandbox_service().get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        root_dir = f"{home}/ci_diagnostics_live_{uuid.uuid4().hex[:8]}"
        yield LiveCiDiagnosticsEnv(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            home=home,
            root_dir=root_dir,
        )
    finally:
        delete_test_sandbox(sandbox_id)


@pytest.fixture(scope="module")
def live_dask_sweevo_env() -> LiveCiDiagnosticsEnv:
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona credentials not configured")

    from benchmarks.sweevo.dataset import select_sweevo_instance
    from benchmarks.sweevo.models import _CONDA_ACTIVATE
    from benchmarks.sweevo.sandbox import create_sweevo_test_sandbox
    from sandbox.testing import delete_test_sandbox, get_sandbox_service

    instance = select_sweevo_instance(instance_id=_DASK_SWEEVO_INSTANCE_ID)
    sandbox_name = f"ci-diag-dask-10042-{uuid.uuid4().hex[:8]}"
    result = asyncio_run(
        create_sweevo_test_sandbox(
            instance,
            sandbox_name=sandbox_name,
            repo_dir=_DASK_SWEEVO_REPO_DIR,
        )
    )
    sandbox_id = str(result["sandbox_id"])
    try:
        raw_sandbox = get_sandbox_service().get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        env = LiveCiDiagnosticsEnv(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            home=home,
            root_dir=_DASK_SWEEVO_REPO_DIR,
        )
        exit_code, output = env.exec(
            f"{_CONDA_ACTIVATE} && cd {_DASK_SWEEVO_REPO_DIR} && python --version",
            timeout=60,
        )
        assert exit_code == 0, output
        yield env
    finally:
        delete_test_sandbox(sandbox_id)


def asyncio_run(coro: Any) -> Any:
    import asyncio

    return asyncio.run(coro)


def _json_output(result: ToolResult) -> dict[str, Any]:
    assert not result.is_error, result.output
    payload = json.loads(result.output)
    assert isinstance(payload, dict)
    return payload


async def _write_file(
    ctx: ToolExecutionContextService,
    *,
    file_path: str,
    content: str,
) -> dict[str, Any]:
    result = await write_file.execute(
        write_file.input_model(file_path=file_path, content=content),
        ctx,
    )
    return _json_output(result)


@pytest.mark.asyncio
async def test_live_ci_diagnostics_healthy_module_imports_and_reports_clean(
    live_ci_diagnostics_env: LiveCiDiagnosticsEnv,
) -> None:
    svc = live_ci_diagnostics_env.make_ci_service()
    ctx = live_ci_diagnostics_env.make_ctx(svc)

    await _write_file(ctx, file_path="healthy/__init__.py", content="")
    await _write_file(
        ctx,
        file_path="healthy/checks.py",
        content=(
            "def compute(value: int) -> int:\n"
            "    return value * 2\n\n"
            "class HealthCheck:\n"
            "    def ok(self) -> bool:\n"
            "        return compute(3) == 6\n"
        ),
    )

    import_script = (
        "import sys; "
        f"sys.path.insert(0, {live_ci_diagnostics_env.root_dir!r}); "
        "from healthy.checks import HealthCheck; "
        "assert HealthCheck().ok(); "
        "print('healthy-ok')"
    )
    exit_code, import_output = live_ci_diagnostics_env.exec(
        f"python3 -c {shlex.quote(import_script)}",
        timeout=30,
    )
    assert exit_code == 0, import_output
    assert "healthy-ok" in import_output

    diagnostics_result = await ci_diagnostics.execute(
        ci_diagnostics.input_model(file_path="healthy/checks.py"),
        ctx,
    )
    diagnostics_payload = _json_output(diagnostics_result)

    assert diagnostics_payload["cwd"] == live_ci_diagnostics_env.root_dir
    assert diagnostics_payload["file_path"] == "healthy/checks.py"
    assert diagnostics_payload["clean"] is True
    assert diagnostics_payload["diagnostics"] == []


@pytest.mark.asyncio
async def test_live_dask_sweevo_config_imports_and_reports_clean(
    live_dask_sweevo_env: LiveCiDiagnosticsEnv,
) -> None:
    from benchmarks.sweevo.models import _CONDA_ACTIVATE

    svc = live_dask_sweevo_env.make_ci_service()
    ctx = live_dask_sweevo_env.make_ctx(svc)

    import_script = (
        "from dask import config; "
        "assert isinstance(config.config, dict); "
        "assert callable(config.get); "
        "print('dask-config-ok')"
    )
    exit_code, import_output = live_dask_sweevo_env.exec(
        f"{_CONDA_ACTIVATE} && cd {_DASK_SWEEVO_REPO_DIR} "
        f"&& python -c {shlex.quote(import_script)}",
        timeout=120,
    )
    assert exit_code == 0, import_output
    assert "dask-config-ok" in import_output

    diagnostics_result = await ci_diagnostics.execute(
        ci_diagnostics.input_model(file_path="dask/config.py"),
        ctx,
    )
    diagnostics_payload = _json_output(diagnostics_result)

    assert diagnostics_payload["cwd"] == _DASK_SWEEVO_REPO_DIR
    assert diagnostics_payload["file_path"] == "dask/config.py"
    assert diagnostics_payload["clean"] is True
    assert diagnostics_payload["diagnostics"] == []


@pytest.mark.asyncio
async def test_live_dask_sweevo_complex_module_clean_then_broken(
    live_dask_sweevo_env: LiveCiDiagnosticsEnv,
) -> None:
    from benchmarks.sweevo.models import _CONDA_ACTIVATE

    svc = live_dask_sweevo_env.make_ci_service()
    ctx = live_dask_sweevo_env.make_ctx(svc)
    file_path = "dask/_ci_diagnostics_complex.py"

    clean_content = (
        "from __future__ import annotations\n\n"
        "from dataclasses import dataclass\n"
        "from typing import Callable, Iterable\n\n"
        "def normalize(items: Iterable[int], transform: Callable[[int], int]) -> list[int]:\n"
        "    return [transform(item) for item in items if item >= 0]\n\n"
        "@dataclass\n"
        "class DiagnosticProbe:\n"
        "    values: tuple[int, ...]\n\n"
        "    @property\n"
        "    def total(self) -> int:\n"
        "        return sum(normalize(self.values, lambda value: value * 2))\n\n"
        "def build_probe() -> DiagnosticProbe:\n"
        "    return DiagnosticProbe(values=(1, -1, 3))\n"
    )
    await _write_file(ctx, file_path=file_path, content=clean_content)

    clean_import_script = (
        "from dask._ci_diagnostics_complex import build_probe; "
        "assert build_probe().total == 8; "
        "print('dask-complex-ok')"
    )
    exit_code, import_output = live_dask_sweevo_env.exec(
        f"{_CONDA_ACTIVATE} && cd {_DASK_SWEEVO_REPO_DIR} "
        f"&& python -c {shlex.quote(clean_import_script)}",
        timeout=120,
    )
    assert exit_code == 0, import_output
    assert "dask-complex-ok" in import_output

    clean_result = await ci_diagnostics.execute(
        ci_diagnostics.input_model(file_path=file_path),
        ctx,
    )
    clean_payload = _json_output(clean_result)
    assert clean_payload["clean"] is True
    assert clean_payload["diagnostics"] == []

    broken_content = (
        "from __future__ import annotations\n\n"
        "def broken_scheduler_options():\n"
        "    return {\n"
        "        'scheduler': ('threads', 'processes'],\n"
        "        'optimizations': ['fuse', 'inline'],\n"
        "    }\n"
    )
    await _write_file(ctx, file_path=file_path, content=broken_content)

    broken_import_script = "import dask._ci_diagnostics_complex"
    exit_code, import_output = live_dask_sweevo_env.exec(
        f"{_CONDA_ACTIVATE} && cd {_DASK_SWEEVO_REPO_DIR} "
        f"&& python -c {shlex.quote(broken_import_script)}",
        timeout=120,
    )
    assert exit_code != 0
    assert "SyntaxError" in import_output
    assert "_ci_diagnostics_complex.py" in import_output

    broken_result = await ci_diagnostics.execute(
        ci_diagnostics.input_model(file_path=file_path),
        ctx,
    )
    broken_payload = _json_output(broken_result)

    assert broken_payload["cwd"] == _DASK_SWEEVO_REPO_DIR
    assert broken_payload["file_path"] == file_path
    assert broken_payload["clean"] is False
    assert len(broken_payload["diagnostics"]) == 1
    diagnostic = broken_payload["diagnostics"][0]
    assert diagnostic["line"] >= 1
    assert diagnostic["source"] == "python"
    assert diagnostic["severity"] == "error"
    assert diagnostic["message"]


@pytest.mark.asyncio
async def test_live_ci_diagnostics_reports_sandbox_syntax_error_after_tool_write(
    live_ci_diagnostics_env: LiveCiDiagnosticsEnv,
) -> None:
    svc = live_ci_diagnostics_env.make_ci_service()
    ctx = live_ci_diagnostics_env.make_ctx(svc)

    await _write_file(ctx, file_path="dask/__init__.py", content="")
    await _write_file(ctx, file_path="dask/config.py", content="VALUE = 1\n")

    clean_result = await ci_diagnostics.execute(
        ci_diagnostics.input_model(file_path="dask/config.py"),
        ctx,
    )
    clean_payload = _json_output(clean_result)
    assert clean_payload["clean"] is True
    assert clean_payload["diagnostics"] == []

    await _write_file(ctx, file_path="dask/config.py", content="def broken(:\n    pass\n")

    import_script = (
        "import sys; "
        f"sys.path.insert(0, {live_ci_diagnostics_env.root_dir!r}); "
        "import dask.config"
    )
    exit_code, import_output = live_ci_diagnostics_env.exec(
        f"python3 -c {shlex.quote(import_script)}",
        timeout=30,
    )
    assert exit_code != 0
    assert "SyntaxError" in import_output
    assert "config.py" in import_output

    broken_result = await ci_diagnostics.execute(
        ci_diagnostics.input_model(file_path="dask/config.py"),
        ctx,
    )
    broken_payload = _json_output(broken_result)

    assert broken_payload["cwd"] == live_ci_diagnostics_env.root_dir
    assert broken_payload["file_path"] == "dask/config.py"
    assert broken_payload["clean"] is False
    assert len(broken_payload["diagnostics"]) == 1
    diagnostic = broken_payload["diagnostics"][0]
    assert diagnostic["line"] == 1
    assert diagnostic["source"] == "python"
    assert diagnostic["severity"] == "error"
    assert "invalid syntax" in diagnostic["message"]
