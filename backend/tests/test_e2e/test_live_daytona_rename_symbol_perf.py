"""Live performance coverage for ``daytona_rename_symbol``.

This test verifies the semantic rename path stays on the LSP/Jedi planning
path plus direct OCC commit path:

    daytona_rename_symbol -> rename_symbol_plan -> commit_rename_plan

It must not route through ``CodeIntelligenceService.cmd`` / daytona_shell's Git
workspace auditor.

Run with:
    uv run pytest backend/tests/test_e2e/test_live_daytona_rename_symbol_perf.py -m live -v -s
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import json
import os
import re
import shlex
import statistics
import threading
import time
import uuid
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from dotenv import load_dotenv

from code_intelligence.routing.service import CodeIntelligenceService
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit._daytona_utils import _extract_exit_code, _wrap_bash_command
from tools.daytona_toolkit.rename_tool import daytona_rename_symbol

_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")

pytestmark = [pytest.mark.e2e, pytest.mark.live]

_TERM_NOISE = re.compile(r"\x1b\[3J.*$", re.S)


def _load_settings() -> dict[str, Any]:
    settings_path = Path.home() / ".ephemeralos" / "settings.json"
    if settings_path.exists():
        return json.loads(settings_path.read_text())
    return {}


_SETTINGS = _load_settings()
HAS_DAYTONA = bool(
    (os.environ.get("DAYTONA_API_KEY") or _SETTINGS.get("daytona_api_key", ""))
    and (os.environ.get("DAYTONA_API_URL") or _SETTINGS.get("daytona_api_url", ""))
)


class _AsyncFs:
    def __init__(self, real_fs: Any) -> None:
        self._real = real_fs

    async def upload_file(self, *args: Any, **kwargs: Any) -> Any:
        return await asyncio.to_thread(self._real.upload_file, *args, **kwargs)

    async def download_file(self, *args: Any, **kwargs: Any) -> Any:
        return await asyncio.to_thread(self._real.download_file, *args, **kwargs)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)


class _AsyncProcess:
    def __init__(self, real_process: Any) -> None:
        self._real = real_process

    async def exec(self, *args: Any, **kwargs: Any) -> Any:
        response = await asyncio.to_thread(self._real.exec, *args, **kwargs)
        stdout = _TERM_NOISE.sub("", getattr(response, "result", "") or "")
        return SimpleNamespace(
            result=stdout,
            exit_code=getattr(response, "exit_code", None),
        )

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)


class _AsyncSandboxWrapper:
    def __init__(self, raw_sandbox: Any) -> None:
        self._raw = raw_sandbox
        self.fs = _AsyncFs(raw_sandbox.fs)
        self.process = _AsyncProcess(raw_sandbox.process)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._raw, name)


@dataclass
class LiveRenamePerfEnv:
    sandbox_id: str
    raw_sandbox: Any
    async_sandbox: Any
    home: str
    root_dir: str

    def exec(self, command: str, *, timeout: int = 180) -> tuple[int, str]:
        response = self.raw_sandbox.process.exec(_wrap_bash_command(command), timeout=timeout)
        raw = _TERM_NOISE.sub("", getattr(response, "result", "") or "")
        cleaned, exit_code = _extract_exit_code(
            raw,
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return exit_code, cleaned

    def exec_checked(self, command: str, *, timeout: int = 180) -> str:
        exit_code, stdout = self.exec(command, timeout=timeout)
        if exit_code != 0:
            detail = stdout.strip() or f"exit {exit_code}"
            raise AssertionError(f"Sandbox command failed: {detail}")
        return stdout

    def write_file(self, path: str, content: str) -> None:
        self.exec_checked(
            "python3 - <<'PY'\n"
            "from pathlib import Path\n"
            f"path = Path({path!r})\n"
            "path.parent.mkdir(parents=True, exist_ok=True)\n"
            f"path.write_text({content!r}, encoding='utf-8')\n"
            "PY"
        )

    def read_file(self, path: str) -> str:
        return self.exec_checked(f"cat {shlex.quote(path)}")


@pytest.fixture
def live_rename_perf_env() -> LiveRenamePerfEnv:
    if not HAS_DAYTONA:
        pytest.skip("Daytona credentials not configured")

    from sandbox.testing import create_test_sandbox, delete_test_sandbox, get_sandbox_service

    info = create_test_sandbox(name="rename-symbol-perf-live")
    sandbox_id = info["id"]
    try:
        sandbox_svc = get_sandbox_service()
        raw_sandbox = sandbox_svc.get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        env = LiveRenamePerfEnv(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            async_sandbox=_AsyncSandboxWrapper(raw_sandbox),
            home=home,
            root_dir=f"{home}/rename_symbol_perf_{uuid.uuid4().hex[:8]}",
        )
        env.exec_checked(f"mkdir -p {shlex.quote(env.root_dir)}")
        yield env
    finally:
        delete_test_sandbox(sandbox_id)


def _env_int(name: str, default: int, *, minimum: int = 1, maximum: int = 50) -> int:
    raw = os.environ.get(name, "").strip()
    if not raw:
        return default
    try:
        value = int(raw)
    except ValueError:
        return default
    return min(max(value, minimum), maximum)


def _percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = (len(ordered) - 1) * (pct / 100.0)
    lower = int(index)
    upper = min(lower + 1, len(ordered) - 1)
    if lower == upper:
        return round(ordered[lower], 6)
    weight = index - lower
    return round(ordered[lower] * (1 - weight) + ordered[upper] * weight, 6)


def _profile(values: list[float]) -> dict[str, float | int]:
    return {
        "count": len(values),
        "min": round(min(values), 6) if values else 0.0,
        "median": round(statistics.median(values), 6) if values else 0.0,
        "p95": _percentile(values, 95),
        "max": round(max(values), 6) if values else 0.0,
        "total": round(sum(values), 6),
    }


def _write_rename_project(env: LiveRenamePerfEnv, *, symbol_count: int) -> tuple[str, str]:
    pkg = f"{env.root_dir}/pkg"
    core_path = f"{pkg}/core.py"
    uses_path = f"{pkg}/uses.py"
    core_lines = ["# generated live rename perf module", ""]
    uses_lines = [
        "from pkg.core import (",
        *[f"    target_{idx}," for idx in range(symbol_count)],
        ")",
        "",
    ]
    for idx in range(symbol_count):
        core_lines.extend(
            [
                f"def target_{idx}(value):",
                f"    return value + {idx}",
                "",
            ]
        )
        uses_lines.extend(
            [
                f"def call_{idx}():",
                f"    return target_{idx}({idx})",
                "",
            ]
        )
    env.write_file(f"{pkg}/__init__.py", "")
    env.write_file(core_path, "\n".join(core_lines))
    env.write_file(uses_path, "\n".join(uses_lines))
    return core_path, uses_path


def _write_disjoint_rename_project(
    env: LiveRenamePerfEnv,
    *,
    symbol_count: int,
) -> dict[int, tuple[str, str]]:
    pkg = f"{env.root_dir}/pkg_concurrent"
    env.write_file(f"{pkg}/__init__.py", "")
    paths: dict[int, tuple[str, str]] = {}
    for idx in range(symbol_count):
        module_path = f"{pkg}/mod_{idx}.py"
        uses_path = f"{pkg}/use_{idx}.py"
        env.write_file(
            module_path,
            "\n".join(
                [
                    f"def target_concurrent_{idx}(value):",
                    f"    return value + {idx}",
                    "",
                ]
            ),
        )
        env.write_file(
            uses_path,
            "\n".join(
                [
                    f"from pkg_concurrent.mod_{idx} import target_concurrent_{idx}",
                    "",
                    f"def call_{idx}():",
                    f"    return target_concurrent_{idx}({idx})",
                    "",
                ]
            ),
        )
        paths[idx] = (module_path, uses_path)
    return paths


def _ctx(env: LiveRenamePerfEnv, svc: CodeIntelligenceService, *, agent_run_id: str) -> ToolExecutionContext:
    return ToolExecutionContext(
        cwd=Path(env.root_dir),
        metadata={
            "daytona_sandbox": env.async_sandbox,
            "daytona_cwd": env.root_dir,
            "repo_root": env.root_dir,
            "ci_service": svc,
            "agent_run_id": agent_run_id,
        },
    )


@pytest.mark.skipif(not HAS_DAYTONA, reason="Daytona credentials not configured")
def test_live_daytona_rename_symbol_lsp_plan_direct_occ_perf(
    live_rename_perf_env: LiveRenamePerfEnv,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Measure real rename planning and commit cost without using ``svc.cmd``."""
    env = live_rename_perf_env
    rename_count = _env_int("CI_RENAME_SYMBOL_PERF_RENAMES", 6, minimum=2, maximum=20)
    core_path, uses_path = _write_rename_project(env, symbol_count=rename_count)
    svc = CodeIntelligenceService(
        sandbox_id=env.sandbox_id,
        workspace_root=env.root_dir,
        sandbox=env.raw_sandbox,
    )

    phase_s: dict[str, list[float]] = {
        "rename_symbol_plan": [],
        "commit_rename_plan": [],
        "lsp_python_script": [],
    }
    cmd_calls: list[dict[str, Any]] = []

    original_plan = svc.rename_symbol_plan
    original_commit = svc.commit_rename_plan
    original_script = svc.lsp_client._run_python_script

    def timed_plan(*args: Any, **kwargs: Any) -> Any:
        started = time.perf_counter()
        try:
            return original_plan(*args, **kwargs)
        finally:
            phase_s["rename_symbol_plan"].append(time.perf_counter() - started)

    def timed_commit(*args: Any, **kwargs: Any) -> Any:
        started = time.perf_counter()
        try:
            return original_commit(*args, **kwargs)
        finally:
            phase_s["commit_rename_plan"].append(time.perf_counter() - started)

    def timed_script(*args: Any, **kwargs: Any) -> Any:
        started = time.perf_counter()
        try:
            return original_script(*args, **kwargs)
        finally:
            phase_s["lsp_python_script"].append(time.perf_counter() - started)

    async def forbidden_cmd(*args: Any, **kwargs: Any) -> Any:
        cmd_calls.append({"args": repr(args), "kwargs": repr(kwargs)})
        raise AssertionError("daytona_rename_symbol must not call CodeIntelligenceService.cmd")

    monkeypatch.setattr(svc, "rename_symbol_plan", timed_plan)
    monkeypatch.setattr(svc, "commit_rename_plan", timed_commit)
    monkeypatch.setattr(svc.lsp_client, "_run_python_script", timed_script)
    monkeypatch.setattr(svc, "cmd", forbidden_cmd)

    try:
        init_started = time.perf_counter()
        init_ok = svc.ensure_initialized(wait=True) and svc.lsp_client.ensure_ready(
            install_missing=True,
            languages=("python",),
        )["python"]
        init_s = time.perf_counter() - init_started
        if not init_ok:
            pytest.skip("Python LSP backend unavailable in live sandbox")

        results: list[dict[str, Any]] = []
        wall_started = time.perf_counter()
        for idx in range(rename_count):
            symbol = f"target_{idx}"
            new_name = f"renamed_{idx}"
            call_started = time.perf_counter()
            result = asyncio.run(
                daytona_rename_symbol.execute(
                    daytona_rename_symbol.input_model(
                        symbol=symbol,
                        new_name=new_name,
                    ),
                    _ctx(
                        env,
                        svc,
                        agent_run_id=f"rename-perf-{idx}-{uuid.uuid4().hex[:8]}",
                    ),
                )
            )
            elapsed_s = time.perf_counter() - call_started
            assert not result.is_error, result.output
            payload = json.loads(result.output)
            assert payload["status"] == "renamed"
            results.append(
                {
                    "symbol": symbol,
                    "new_name": new_name,
                    "elapsed_s": round(elapsed_s, 6),
                    "file_count": len(payload.get("files", [])),
                }
            )

        wall_s = time.perf_counter() - wall_started
        core_content = env.read_file(core_path)
        uses_content = env.read_file(uses_path)
        for idx in range(rename_count):
            assert f"def renamed_{idx}(" in core_content
            assert f"target_{idx}(" not in core_content
            assert f"renamed_{idx}(" in uses_content
            assert f"target_{idx}(" not in uses_content

        edit_counts = Counter(
            str(getattr(item, "edit_type", "") or "")
            for item in svc.arbiter.recent_edits(seconds=300)
        )
        call_s = [float(item["elapsed_s"]) for item in results]
        payload = {
            "label": "daytona_rename_symbol_lsp_plan_direct_occ_perf",
            "rename_count": rename_count,
            "init_s": round(init_s, 6),
            "wall_s": round(wall_s, 6),
            "per_call_s": _profile(call_s),
            "phase_s": {key: _profile(values) for key, values in phase_s.items()},
            "cmd_calls": len(cmd_calls),
            "results": results,
            "arbiter_edit_counts": dict(sorted(edit_counts.items())),
            "lsp": svc.status()["lsp"],
            "symbol_index_generation": svc.symbol_index.generation,
        }
        print("[rename-symbol-live-perf] " + json.dumps(payload, sort_keys=True), flush=True)

        assert cmd_calls == []
        assert edit_counts["rename_symbol"] >= rename_count
        assert len(phase_s["rename_symbol_plan"]) == rename_count
        assert len(phase_s["commit_rename_plan"]) == rename_count

        threshold_s = os.environ.get("CI_RENAME_SYMBOL_PERF_MAX_P95_S", "").strip()
        if threshold_s:
            assert payload["per_call_s"]["p95"] <= float(threshold_s)
    finally:
        svc.dispose()


@pytest.mark.skipif(not HAS_DAYTONA, reason="Daytona credentials not configured")
def test_live_daytona_rename_symbol_concurrent_disjoint_perf(
    live_rename_perf_env: LiveRenamePerfEnv,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Measure concurrent disjoint semantic renames through direct OCC commits."""
    env = live_rename_perf_env
    rename_count = _env_int(
        "CI_RENAME_SYMBOL_CONCURRENT_RENAMES",
        8,
        minimum=2,
        maximum=24,
    )
    paths = _write_disjoint_rename_project(env, symbol_count=rename_count)
    svc = CodeIntelligenceService(
        sandbox_id=f"{env.sandbox_id}-concurrent",
        workspace_root=env.root_dir,
        sandbox=env.raw_sandbox,
    )

    phase_s: dict[str, list[float]] = {
        "rename_symbol_plan": [],
        "rename_symbol_plans_many": [],
        "commit_rename_plan": [],
        "commit_rename_plans_many": [],
        "lsp_python_script": [],
    }
    phase_lock = threading.Lock()
    cmd_calls: list[dict[str, Any]] = []

    def record_phase(name: str, started: float) -> None:
        with phase_lock:
            phase_s[name].append(time.perf_counter() - started)

    original_plan = svc.rename_symbol_plan
    original_plans_many = svc.rename_symbol_plans_many
    original_commit = svc.commit_rename_plan
    original_commits_many = svc.commit_rename_plans_many
    original_script = svc.lsp_client._run_python_script

    def timed_plan(*args: Any, **kwargs: Any) -> Any:
        started = time.perf_counter()
        try:
            return original_plan(*args, **kwargs)
        finally:
            record_phase("rename_symbol_plan", started)

    def timed_plans_many(*args: Any, **kwargs: Any) -> Any:
        started = time.perf_counter()
        try:
            return original_plans_many(*args, **kwargs)
        finally:
            record_phase("rename_symbol_plans_many", started)

    def timed_commit(*args: Any, **kwargs: Any) -> Any:
        started = time.perf_counter()
        try:
            return original_commit(*args, **kwargs)
        finally:
            record_phase("commit_rename_plan", started)

    def timed_commits_many(*args: Any, **kwargs: Any) -> Any:
        started = time.perf_counter()
        try:
            return original_commits_many(*args, **kwargs)
        finally:
            record_phase("commit_rename_plans_many", started)

    def timed_script(*args: Any, **kwargs: Any) -> Any:
        started = time.perf_counter()
        try:
            return original_script(*args, **kwargs)
        finally:
            record_phase("lsp_python_script", started)

    async def forbidden_cmd(*args: Any, **kwargs: Any) -> Any:
        cmd_calls.append({"args": repr(args), "kwargs": repr(kwargs)})
        raise AssertionError("daytona_rename_symbol must not call CodeIntelligenceService.cmd")

    monkeypatch.setattr(svc, "rename_symbol_plan", timed_plan)
    monkeypatch.setattr(svc, "rename_symbol_plans_many", timed_plans_many)
    monkeypatch.setattr(svc, "commit_rename_plan", timed_commit)
    monkeypatch.setattr(svc, "commit_rename_plans_many", timed_commits_many)
    monkeypatch.setattr(svc.lsp_client, "_run_python_script", timed_script)
    monkeypatch.setattr(svc, "cmd", forbidden_cmd)

    try:
        init_started = time.perf_counter()
        init_ok = svc.ensure_initialized(wait=True) and svc.lsp_client.ensure_ready(
            install_missing=True,
            languages=("python",),
        )["python"]
        init_s = time.perf_counter() - init_started
        if not init_ok:
            pytest.skip("Python LSP backend unavailable in live sandbox")

        async def run_one(idx: int) -> dict[str, Any]:
            symbol = f"target_concurrent_{idx}"
            new_name = f"renamed_concurrent_{idx}"
            started = time.perf_counter()
            result = await daytona_rename_symbol.execute(
                daytona_rename_symbol.input_model(
                    symbol=symbol,
                    new_name=new_name,
                ),
                _ctx(
                    env,
                    svc,
                    agent_run_id=f"rename-concurrent-{idx}-{uuid.uuid4().hex[:8]}",
                ),
            )
            elapsed_s = time.perf_counter() - started
            if result.is_error:
                return {
                    "idx": idx,
                    "symbol": symbol,
                    "new_name": new_name,
                    "ok": False,
                    "elapsed_s": round(elapsed_s, 6),
                    "error": result.output,
                }
            payload = json.loads(result.output)
            return {
                "idx": idx,
                "symbol": symbol,
                "new_name": new_name,
                "ok": payload.get("status") == "renamed",
                "elapsed_s": round(elapsed_s, 6),
                "file_count": len(payload.get("files", [])),
                "status": payload.get("status"),
            }

        async def run_all() -> list[dict[str, Any]]:
            return await asyncio.gather(*(run_one(idx) for idx in range(rename_count)))

        wall_started = time.perf_counter()
        results = asyncio.run(run_all())
        wall_s = time.perf_counter() - wall_started
        failures = [item for item in results if not item["ok"]]
        assert not failures, json.dumps(failures, sort_keys=True)

        def read_pair(idx: int) -> tuple[int, str, str]:
            module_path, uses_path = paths[idx]
            return idx, env.read_file(module_path), env.read_file(uses_path)

        with concurrent.futures.ThreadPoolExecutor(max_workers=min(rename_count, 8)) as pool:
            contents = list(pool.map(read_pair, range(rename_count)))

        for idx, module_content, uses_content in contents:
            assert f"def renamed_concurrent_{idx}(" in module_content
            assert f"target_concurrent_{idx}(" not in module_content
            assert f"renamed_concurrent_{idx}(" in uses_content
            assert f"target_concurrent_{idx}(" not in uses_content

        edit_counts = Counter(
            str(getattr(item, "edit_type", "") or "")
            for item in svc.arbiter.recent_edits(seconds=300)
        )
        call_s = [float(item["elapsed_s"]) for item in results]
        sum_call_s = sum(call_s)
        payload = {
            "label": "daytona_rename_symbol_concurrent_disjoint_perf",
            "rename_count": rename_count,
            "init_s": round(init_s, 6),
            "wall_s": round(wall_s, 6),
            "sum_call_s": round(sum_call_s, 6),
            "effective_parallelism": round(sum_call_s / wall_s, 3) if wall_s > 0 else 0.0,
            "per_call_s": _profile(call_s),
            "phase_s": {key: _profile(values) for key, values in phase_s.items()},
            "cmd_calls": len(cmd_calls),
            "results": sorted(results, key=lambda item: int(item["idx"])),
            "arbiter_edit_counts": dict(sorted(edit_counts.items())),
            "lsp": svc.status()["lsp"],
            "symbol_index_generation": svc.symbol_index.generation,
        }
        print(
            "[rename-symbol-live-concurrent-perf] "
            + json.dumps(payload, sort_keys=True),
            flush=True,
        )

        assert cmd_calls == []
        assert edit_counts["rename_symbol"] >= rename_count
        assert len(phase_s["rename_symbol_plans_many"]) >= 1 or len(
            phase_s["rename_symbol_plan"]
        ) == rename_count
        assert len(phase_s["commit_rename_plans_many"]) >= 1 or len(
            phase_s["commit_rename_plan"]
        ) == rename_count

        threshold_s = os.environ.get("CI_RENAME_SYMBOL_CONCURRENT_MAX_WALL_S", "").strip()
        if threshold_s:
            assert payload["wall_s"] <= float(threshold_s)
    finally:
        svc.dispose()
