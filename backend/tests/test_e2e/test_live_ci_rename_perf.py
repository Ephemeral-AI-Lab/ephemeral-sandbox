"""Live Daytona trace + performance test for Jedi/LSP-backed CI operations.

This test intentionally prints detailed trace logs. It is meant for
debugging live sandbox latency and behavior after changes to the Jedi/LSP
stack and the rename tools.

Run with:
    uv run pytest backend/tests/test_e2e/test_live_ci_rename_perf.py -m live -v -s
"""

from __future__ import annotations

import asyncio
import base64
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
from dataclasses import asdict, dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Callable, TypeVar

import pytest
from dotenv import load_dotenv

from code_intelligence.routing.service import CodeIntelligenceService
from code_intelligence.lsp._jedi_worker_client import (
    ENV_FLAG as JEDI_WORKER_ENV_FLAG,
)
from tools.ci_toolkit.rename_tool import ci_rename_symbol
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit._daytona_utils import _extract_exit_code, _wrap_bash_command
from tools.daytona_toolkit.codeact_tool import daytona_codeact
from tools.daytona_toolkit.edit_tool import daytona_edit_file
from tools.daytona_toolkit.tools import daytona_write_file

_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")

pytestmark = [pytest.mark.e2e, pytest.mark.live]

_T = TypeVar("_T")
_TERM_NOISE = re.compile(r"\x1b\[3J.*$", re.S)
_MAX_COMMAND_CHARS = 260


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


@dataclass
class TraceEvent:
    seq: int
    op: str
    duration_ms: float
    exit_code: int | None
    stdout_chars: int
    command: str
    error: str = ""


class TraceLog:
    """Collects sandbox IO events and prints compact debugging summaries."""

    def __init__(self) -> None:
        self.events: list[TraceEvent] = []
        self._lock = threading.Lock()

    def mark(self) -> int:
        with self._lock:
            return len(self.events)

    def record(
        self,
        *,
        op: str,
        command: Any,
        started_at: float,
        result: Any = None,
        error: BaseException | None = None,
    ) -> None:
        stdout = _clean_stdout(getattr(result, "result", "") or "") if result is not None else ""
        with self._lock:
            self.events.append(
                TraceEvent(
                    seq=len(self.events) + 1,
                    op=op,
                    duration_ms=round((time.perf_counter() - started_at) * 1000, 3),
                    exit_code=getattr(result, "exit_code", None) if result is not None else None,
                    stdout_chars=len(stdout),
                    command=_compact(str(command)),
                    error=f"{type(error).__name__}: {error}" if error is not None else "",
                )
            )

    def summary_since(self, start: int) -> dict[str, Any]:
        with self._lock:
            events = list(self.events[start:])
        durations = [event.duration_ms for event in events]
        slowest = sorted(events, key=lambda event: event.duration_ms, reverse=True)[:5]
        return {
            "exec_count": len(events),
            "exec_total_ms": round(sum(durations), 3),
            "exec_max_ms": round(max(durations), 3) if durations else 0.0,
            "exec_median_ms": round(statistics.median(durations), 3) if durations else 0.0,
            "slowest_exec": [asdict(event) for event in slowest],
        }

    def durations(self) -> list[float]:
        with self._lock:
            return [event.duration_ms for event in self.events]

    def print_all(self) -> None:
        with self._lock:
            events = list(self.events)
        for event in events:
            print("[ci-lsp-live-exec] " + json.dumps(asdict(event), sort_keys=True), flush=True)

    def print_summary(self) -> None:
        with self._lock:
            events = list(self.events)
        durations = [event.duration_ms for event in events]
        payload = {
            "total_exec_count": len(events),
            "total_exec_ms": round(sum(durations), 3),
            "median_exec_ms": round(statistics.median(durations), 3) if durations else 0.0,
            "p95_exec_ms": _percentile(durations, 95),
            "slowest_exec": [
                asdict(event)
                for event in sorted(events, key=lambda event: event.duration_ms, reverse=True)[:10]
            ],
        }
        print("[ci-lsp-live-summary] " + json.dumps(payload, sort_keys=True), flush=True)


class _TracingProcess:
    def __init__(self, real_process: Any, trace: TraceLog):
        self._real = real_process
        self._trace = trace

    def exec(self, command: str, *args: Any, **kwargs: Any) -> Any:
        started_at = time.perf_counter()
        try:
            result = self._real.exec(command, *args, **kwargs)
        except Exception as exc:
            self._trace.record(
                op="process.exec",
                command=command,
                started_at=started_at,
                error=exc,
            )
            raise
        self._trace.record(
            op="process.exec",
            command=command,
            started_at=started_at,
            result=result,
        )
        return result

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)


class _TracingFs:
    def __init__(self, real_fs: Any, trace: TraceLog):
        self._real = real_fs
        self._trace = trace

    def download_file(self, *args: Any, **kwargs: Any) -> Any:
        started_at = time.perf_counter()
        try:
            result = self._real.download_file(*args, **kwargs)
        except Exception as exc:
            self._trace.record(
                op="fs.download_file",
                command=args[0] if args else "",
                started_at=started_at,
                error=exc,
            )
            raise
        self._trace.record(
            op="fs.download_file",
            command=args[0] if args else "",
            started_at=started_at,
            result=SimpleNamespace(result=result, exit_code=0),
        )
        return result

    def upload_file(self, *args: Any, **kwargs: Any) -> Any:
        started_at = time.perf_counter()
        try:
            result = self._real.upload_file(*args, **kwargs)
        except Exception as exc:
            self._trace.record(
                op="fs.upload_file",
                command=args[-1] if args else "",
                started_at=started_at,
                error=exc,
            )
            raise
        self._trace.record(
            op="fs.upload_file",
            command=args[-1] if args else "",
            started_at=started_at,
            result=SimpleNamespace(result=result, exit_code=0),
        )
        return result

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)


class _TracingSandboxWrapper:
    def __init__(self, raw_sandbox: Any, trace: TraceLog):
        self._raw = raw_sandbox
        self.process = _TracingProcess(raw_sandbox.process, trace)
        self.fs = _TracingFs(raw_sandbox.fs, trace)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._raw, name)


class _AsyncFs:
    def __init__(self, real_fs: Any):
        self._real = real_fs

    async def upload_file(self, *args: Any, **kwargs: Any) -> Any:
        return self._real.upload_file(*args, **kwargs)

    async def download_file(self, *args: Any, **kwargs: Any) -> Any:
        return self._real.download_file(*args, **kwargs)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)


class _AsyncProcess:
    def __init__(self, real_process: Any):
        self._real = real_process

    async def exec(self, *args: Any, **kwargs: Any) -> Any:
        response = self._real.exec(*args, **kwargs)
        stdout = _clean_stdout(getattr(response, "result", "") or "")
        return SimpleNamespace(result=stdout, exit_code=getattr(response, "exit_code", None))

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)


class _AsyncSandboxWrapper:
    def __init__(self, raw_sandbox: Any):
        self._raw = raw_sandbox
        self.fs = _AsyncFs(raw_sandbox.fs)
        self.process = _AsyncProcess(raw_sandbox.process)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._raw, name)


@dataclass
class LiveRenameEnv:
    sandbox_id: str
    raw_sandbox: Any
    traced_sandbox: Any
    async_sandbox: Any
    trace: TraceLog
    home: str
    root_dir: str

    def exec(self, command: str, *, timeout: int = 180) -> tuple[int, str]:
        response = self.raw_sandbox.process.exec(_wrap_bash_command(command), timeout=timeout)
        raw = _clean_stdout(getattr(response, "result", "") or "")
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
        payload = base64.b64encode(content.encode("utf-8")).decode("ascii")
        self.exec_checked(
            f"mkdir -p {shlex.quote(os.path.dirname(path) or '.')} && "
            f"echo {shlex.quote(payload)} | base64 -d > {shlex.quote(path)}"
        )

    def read_file(self, path: str) -> str:
        return self.exec_checked(f"cat {shlex.quote(path)}")


@pytest.fixture
def live_rename_env() -> LiveRenameEnv:
    if not HAS_DAYTONA:
        pytest.skip("Daytona credentials not configured")

    from sandbox.testing import create_test_sandbox, delete_test_sandbox, get_sandbox_service

    info = create_test_sandbox(name="ci-lsp-perf-live")
    sandbox_id = info["id"]
    try:
        sandbox_svc = get_sandbox_service()
        raw_sandbox = sandbox_svc.get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        trace = TraceLog()
        traced = _TracingSandboxWrapper(raw_sandbox, trace)
        env = LiveRenameEnv(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            traced_sandbox=traced,
            async_sandbox=_AsyncSandboxWrapper(traced),
            trace=trace,
            home=home,
            root_dir=f"{home}/ci_lsp_perf_{uuid.uuid4().hex[:8]}",
        )
        env.exec_checked(f"mkdir -p {shlex.quote(env.root_dir)}")
        yield env
    finally:
        delete_test_sandbox(sandbox_id)


def _write_perf_project(env: LiveRenameEnv, root_dir: str) -> tuple[str, str, str]:
    pkg = f"{root_dir}/pkg"
    core_path = f"{pkg}/core.py"
    uses_path = f"{pkg}/uses.py"
    more_path = f"{pkg}/more.py"
    env.write_file(f"{pkg}/__init__.py", "")
    env.write_file(
        core_path,
        "\n".join(
            [
                "def alpha(value):",
                '    """Primary function used for LSP definition/reference probes."""',
                "    return value + 1",
                "",
                "def beta(value):",
                "    return alpha(value) * 2",
                "",
                "def delta(value):",
                "    return beta(value) + 5",
                "",
                "class Runner:",
                "    def run(self, value):",
                "        return delta(value)",
                "",
            ]
        ),
    )
    env.write_file(
        uses_path,
        "\n".join(
            [
                "from pkg.core import alpha, beta, delta, Runner",
                "",
                "def call_alpha():",
                "    return alpha(1)",
                "",
                "def call_beta():",
                "    return beta(2)",
                "",
                "def call_delta():",
                "    return delta(3)",
                "",
                "def call_runner():",
                "    return Runner().run(4)",
                "",
            ]
        ),
    )
    env.write_file(
        more_path,
        "\n".join(
            [
                "from pkg.core import alpha, beta, delta",
                "",
                "def combine():",
                "    return alpha(10) + beta(20) + delta(30)",
                "",
            ]
        ),
    )
    return core_path, uses_path, more_path


def test_live_ci_lsp_jedi_tool_traces_and_perf(
    live_rename_env: LiveRenameEnv,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Trace direct LSP calls and rename tool calls inside a real Daytona sandbox."""
    monkeypatch.setenv(JEDI_WORKER_ENV_FLAG, "0")
    all_stats: list[dict[str, Any]] = []
    env = live_rename_env
    core_path, uses_path, more_path = _write_perf_project(env, env.root_dir)

    svc = CodeIntelligenceService(
        sandbox_id=env.sandbox_id,
        workspace_root=env.root_dir,
        sandbox=env.traced_sandbox,
    )
    ctx = ToolExecutionContext(
        cwd=Path(env.root_dir),
        metadata={
            "daytona_sandbox": env.async_sandbox,
            "daytona_cwd": env.root_dir,
            "repo_root": env.root_dir,
            "ci_service": svc,
            "agent_run_id": f"ci-lsp-live-{uuid.uuid4().hex[:8]}",
        },
    )

    try:
        init_ok = _measure(
            "service.ensure_initialized",
            env.trace,
            svc,
            lambda: (
                svc.ensure_initialized(wait=True)
                and svc.lsp_client.ensure_ready(
                    install_missing=True,
                    languages=("python",),
                )["python"]
            ),
            stats=all_stats,
        )
        assert init_ok is True
        assert svc.symbol_index.ensure_built(wait=True, timeout=60.0) is True
        _print_service_health("baseline.after_init", svc, env)

        definitions = _measure(
            "lsp.goto_definition(alpha usage)",
            env.trace,
            svc,
            lambda: svc.lsp_client.goto_definition(uses_path, 4, 11),
            stats=all_stats,
        )
        assert any(item.file_path == core_path and item.name == "alpha" for item in definitions)

        refs = _measure(
            "lsp.find_references(alpha cold)",
            env.trace,
            svc,
            lambda: svc.lsp_client.find_references(core_path, 1, 0),
            stats=all_stats,
        )
        assert len(refs) >= 3

        cache_before = svc.lsp_client.telemetry.cache_hits
        refs_cached = _measure(
            "lsp.find_references(alpha cached)",
            env.trace,
            svc,
            lambda: svc.lsp_client.find_references(core_path, 1, 0),
            stats=all_stats,
        )
        assert len(refs_cached) == len(refs)
        assert svc.lsp_client.telemetry.cache_hits > cache_before

        hover = _measure(
            "lsp.hover(alpha)",
            env.trace,
            svc,
            lambda: svc.lsp_client.hover(core_path, 1, 0),
            stats=all_stats,
        )
        assert hover is not None
        assert "Primary function" in hover.content

        dry_symbol = _measure(
            "tool.ci_rename_symbol(beta dry_run)",
            env.trace,
            svc,
            lambda: asyncio.run(
                ci_rename_symbol.execute(
                    ci_rename_symbol.input_model(
                        symbol="beta",
                        new_name="beta_v2",
                        dry_run=True,
                    ),
                    ctx,
                )
            ),
            stats=all_stats,
        )
        assert not dry_symbol.is_error, dry_symbol.output
        dry_symbol_data = json.loads(dry_symbol.output)
        assert dry_symbol_data["status"] == "dry_run"
        assert len(dry_symbol_data["files"]) >= 3

        commit_symbol = _measure(
            "tool.ci_rename_symbol(beta commit)",
            env.trace,
            svc,
            lambda: asyncio.run(
                ci_rename_symbol.execute(
                    ci_rename_symbol.input_model(
                        symbol="beta",
                        new_name="beta_v2",
                    ),
                    ctx,
                )
            ),
            stats=all_stats,
        )
        assert not commit_symbol.is_error, commit_symbol.output
        commit_symbol_data = json.loads(commit_symbol.output)
        assert commit_symbol_data["status"] == "renamed"
        assert len(commit_symbol_data["files"]) >= 3

        dry_facade = _measure(
            "tool.ci_rename_symbol(delta dry_run)",
            env.trace,
            svc,
            lambda: asyncio.run(
                ci_rename_symbol.execute(
                    ci_rename_symbol.input_model(symbol="delta", new_name="delta_v2", dry_run=True),
                    ctx,
                )
            ),
            stats=all_stats,
        )
        assert not dry_facade.is_error, dry_facade.output
        dry_facade_data = json.loads(dry_facade.output)
        assert dry_facade_data["status"] == "dry_run"
        assert len(dry_facade_data["files"]) >= 3

        commit_facade = _measure(
            "tool.ci_rename_symbol(delta commit)",
            env.trace,
            svc,
            lambda: asyncio.run(
                ci_rename_symbol.execute(
                    ci_rename_symbol.input_model(symbol="delta", new_name="delta_v2"),
                    ctx,
                )
            ),
            stats=all_stats,
        )
        assert not commit_facade.is_error, commit_facade.output
        commit_facade_data = json.loads(commit_facade.output)
        assert commit_facade_data["status"] == "renamed"
        assert len(commit_facade_data["files"]) >= 3

        core_new = env.read_file(core_path)
        uses_new = env.read_file(uses_path)
        more_new = env.read_file(more_path)
        assert "def beta_v2" in core_new and "def beta(" not in core_new
        assert "def delta_v2" in core_new and "def delta(" not in core_new
        assert "beta_v2" in uses_new and "delta_v2" in uses_new
        assert "beta_v2" in more_new and "delta_v2" in more_new
        assert "import alpha, beta," not in uses_new
        assert "import alpha, beta," not in more_new
        assert not re.search(r"(?<![A-Za-z0-9_])beta\(", uses_new)
        assert not re.search(r"(?<![A-Za-z0-9_])delta\(", uses_new)
        assert not re.search(r"(?<![A-Za-z0-9_])beta\(", more_new)
        assert not re.search(r"(?<![A-Za-z0-9_])delta\(", more_new)

        final_status = svc.status()
        print(
            "[ci-lsp-live-final-status] "
            + json.dumps(
                {
                    "sandbox_id": env.sandbox_id,
                    "workspace_root": env.root_dir,
                    "symbol_index": final_status["symbol_index"],
                    "tree_cache": final_status["tree_cache"],
                    "rename_preview_cache": final_status["rename_preview_cache"],
                    "arbiter": final_status["arbiter"],
                    "lsp": final_status["lsp"],
                    "jedi_worker_env": os.environ.get("CI_JEDI_WORKER_ENABLED", ""),
                    "jedi_worker_used": svc.lsp_client.worker_active,
                },
                sort_keys=True,
            ),
            flush=True,
        )

        svc.dispose()

        monkeypatch.setenv(JEDI_WORKER_ENV_FLAG, "1")
        worker_root = f"{env.root_dir}/worker_compare"
        worker_core, worker_uses, _worker_more = _write_perf_project(env, worker_root)
        worker_svc = CodeIntelligenceService(
            sandbox_id=f"{env.sandbox_id}-worker",
            workspace_root=worker_root,
            sandbox=env.traced_sandbox,
        )
        worker_ctx = ToolExecutionContext(
            cwd=Path(worker_root),
            metadata={
                "daytona_sandbox": env.async_sandbox,
                "daytona_cwd": worker_root,
                "repo_root": worker_root,
                "ci_service": worker_svc,
                "agent_run_id": f"ci-lsp-worker-{uuid.uuid4().hex[:8]}",
            },
        )
        try:
            worker_init_ok = _measure(
                "worker.service.ensure_initialized",
                env.trace,
                worker_svc,
                lambda: (
                    worker_svc.ensure_initialized(wait=True)
                    and worker_svc.lsp_client.ensure_ready(
                        install_missing=True,
                        languages=("python",),
                    )["python"]
                ),
                stats=all_stats,
            )
            assert worker_init_ok is True
            _print_service_health("worker.after_init", worker_svc, env)

            worker_defs = _measure(
                "worker.lsp.goto_definition(alpha usage)",
                env.trace,
                worker_svc,
                lambda: worker_svc.lsp_client.goto_definition(worker_uses, 4, 11),
                stats=all_stats,
            )
            assert any(
                item.file_path == worker_core and item.name == "alpha"
                for item in worker_defs
            )

            worker_refs = _measure(
                "worker.lsp.find_references(alpha cold)",
                env.trace,
                worker_svc,
                lambda: worker_svc.lsp_client.find_references(worker_core, 1, 0),
                stats=all_stats,
            )
            assert len(worker_refs) >= 3

            worker_hover = _measure(
                "worker.lsp.hover(alpha)",
                env.trace,
                worker_svc,
                lambda: worker_svc.lsp_client.hover(worker_core, 1, 0),
                stats=all_stats,
            )
            assert worker_hover is not None
            assert "Primary function" in worker_hover.content

            worker_dry = _measure(
                "worker.tool.ci_rename_symbol(beta dry_run)",
                env.trace,
                worker_svc,
                lambda: asyncio.run(
                    ci_rename_symbol.execute(
                        ci_rename_symbol.input_model(
                            symbol="beta",
                            new_name="beta_v2",
                            dry_run=True,
                        ),
                        worker_ctx,
                    )
                ),
                stats=all_stats,
            )
            assert not worker_dry.is_error, worker_dry.output
            assert json.loads(worker_dry.output)["status"] == "dry_run"
            worker_tel = worker_svc.lsp_client.telemetry
            assert worker_tel.worker_successes >= 3
            assert worker_tel.worker_fallbacks == 0
            assert worker_tel.worker_errors == 0
            assert worker_svc.lsp_client.worker_active is True

            _exercise_tree_cache(worker_svc, worker_core, env.read_file(worker_core))

            _print_mode_comparison(all_stats)
            _run_concurrent_ci_load(
                env=env,
                svc=worker_svc,
                ctx=worker_ctx,
                core_path=worker_core,
                uses_path=worker_uses,
                stats=all_stats,
            )
            _run_occ_capture_checks(env=env, stats=all_stats)
        finally:
            worker_svc.dispose()

        _apply_optional_thresholds(all_stats, env.trace)
    finally:
        env.trace.print_all()
        env.trace.print_summary()
        svc.dispose()


def _measure(
    label: str,
    trace: TraceLog,
    svc: CodeIntelligenceService,
    func: Callable[[], _T],
    *,
    stats: list[dict[str, Any]] | None = None,
) -> _T:
    event_start = trace.mark()
    telemetry_before = svc.lsp_client.telemetry
    tree_before = svc.tree_cache.stats
    index_before = svc.symbol_index.generation
    arbiter_before = svc.arbiter.generation
    started_at = time.perf_counter()
    error = ""
    try:
        result = func()
        return result
    except Exception as exc:
        error = f"{type(exc).__name__}: {exc}"
        raise
    finally:
        duration_ms = round((time.perf_counter() - started_at) * 1000, 3)
        telemetry_after = svc.lsp_client.telemetry
        payload = {
            "label": label,
            "duration_ms": duration_ms,
            "error": error,
            "lsp_queries_delta": telemetry_after.queries - telemetry_before.queries,
            "lsp_successes_delta": telemetry_after.successes - telemetry_before.successes,
            "lsp_errors_delta": telemetry_after.errors - telemetry_before.errors,
            "lsp_cache_hits_delta": telemetry_after.cache_hits - telemetry_before.cache_hits,
            "jedi_script_runs_delta": telemetry_after.script_runs - telemetry_before.script_runs,
            "jedi_script_successes_delta": (
                telemetry_after.script_successes - telemetry_before.script_successes
            ),
            "jedi_script_errors_delta": (
                telemetry_after.script_errors - telemetry_before.script_errors
            ),
            "worker_successes_delta": (
                telemetry_after.worker_successes - telemetry_before.worker_successes
            ),
            "worker_fallbacks_delta": (
                telemetry_after.worker_fallbacks - telemetry_before.worker_fallbacks
            ),
            "worker_errors_delta": telemetry_after.worker_errors - telemetry_before.worker_errors,
            "tree_cache_hits_delta": svc.tree_cache.stats["hits"] - tree_before["hits"],
            "tree_cache_misses_delta": svc.tree_cache.stats["misses"] - tree_before["misses"],
            "tree_cache_stat_calls_delta": (
                svc.tree_cache.stats["stat_calls"] - tree_before["stat_calls"]
            ),
            "rename_preview_cache": svc.status()["rename_preview_cache"],
            "symbol_index_generation_delta": svc.symbol_index.generation - index_before,
            "arbiter_generation_delta": svc.arbiter.generation - arbiter_before,
            **trace.summary_since(event_start),
        }
        if stats is not None:
            stats.append(dict(payload))
        print("[ci-lsp-live-trace] " + json.dumps(payload, sort_keys=True), flush=True)


def _service_health_payload(
    svc: CodeIntelligenceService,
    env: LiveRenameEnv,
) -> dict[str, Any]:
    status = svc.status()
    return {
        "sandbox_id": env.sandbox_id,
        "workspace_root": svc.workspace_root,
        "symbol_index": status["symbol_index"],
        "tree_cache": status["tree_cache"],
        "rename_preview_cache": status["rename_preview_cache"],
        "arbiter": status["arbiter"],
        "lsp": status["lsp"],
    }


def _print_service_health(
    label: str,
    svc: CodeIntelligenceService,
    env: LiveRenameEnv,
) -> None:
    payload = {"label": label, **_service_health_payload(svc, env)}
    print("[ci-lsp-live-health] " + json.dumps(payload, sort_keys=True), flush=True)


def _exercise_tree_cache(
    svc: CodeIntelligenceService,
    file_path: str,
    content: str,
) -> None:
    before = svc.tree_cache.stats
    first = svc.tree_cache.get_tree(file_path, content=content)
    second = svc.tree_cache.get_tree(file_path, content=content)
    after = svc.tree_cache.stats
    payload = {
        "label": "worker.tree_cache_direct_reuse",
        "file_path": file_path,
        "before": before,
        "after": after,
        "hits_delta": after["hits"] - before["hits"],
        "misses_delta": after["misses"] - before["misses"],
        "size_delta": after["size"] - before["size"],
    }
    print("[ci-lsp-live-cache] " + json.dumps(payload, sort_keys=True), flush=True)
    assert first is not None
    assert second is not None
    assert after["hits"] - before["hits"] >= 1


def _run_occ_capture_checks(
    *,
    env: LiveRenameEnv,
    stats: list[dict[str, Any]],
) -> None:
    """Verify public write/edit/rename/codeact tools all land in the audit ledger."""
    occ_root = f"{env.root_dir}/occ_capture_{uuid.uuid4().hex[:8]}"
    occ_core, _occ_uses, _occ_more = _write_perf_project(env, occ_root)
    env.exec_checked(
        " && ".join(
            [
                f"git -C {shlex.quote(occ_root)} init -q",
                f"git -C {shlex.quote(occ_root)} config user.email live-ci@example.invalid",
                f"git -C {shlex.quote(occ_root)} config user.name live-ci",
                f"git -C {shlex.quote(occ_root)} add .",
                f"git -C {shlex.quote(occ_root)} commit -q -m init",
            ]
        ),
        timeout=180,
    )

    occ_svc = CodeIntelligenceService(
        sandbox_id=f"{env.sandbox_id}-occ",
        workspace_root=occ_root,
        sandbox=env.traced_sandbox,
    )
    occ_ctx = ToolExecutionContext(
        cwd=Path(occ_root),
        metadata={
            "daytona_sandbox": env.async_sandbox,
            "daytona_cwd": occ_root,
            "repo_root": occ_root,
            "ci_service": occ_svc,
            "arbiter": occ_svc.arbiter,
            "agent_run_id": f"ci-lsp-occ-agent-{uuid.uuid4().hex[:8]}",
            "agent_name": "developer",
            "team_run_id": f"ci-lsp-occ-team-{uuid.uuid4().hex[:8]}",
            "work_item_id": "occ-live-capture",
            "work_item_started_at": time.time(),
        },
    )

    try:
        init_ok = _measure(
            "occ.service.ensure_initialized",
            env.trace,
            occ_svc,
            lambda: (
                occ_svc.ensure_initialized(wait=True)
                and occ_svc.lsp_client.ensure_ready(
                    install_missing=True,
                    languages=("python",),
                )["python"]
            ),
            stats=stats,
        )
        assert init_ok is True

        write_path = f"{occ_root}/pkg/written.py"
        write_result = _measure(
            "occ.daytona_write_file",
            env.trace,
            occ_svc,
            lambda: asyncio.run(
                daytona_write_file.execute(
                    daytona_write_file.input_model(
                        file_path=write_path,
                        content="value = 1\n",
                    ),
                    occ_ctx,
                )
            ),
            stats=stats,
        )
        assert not write_result.is_error, write_result.output
        write_data = json.loads(write_result.output)
        assert write_data["ci_sync"] is True

        edit_result = _measure(
            "occ.daytona_edit_file",
            env.trace,
            occ_svc,
            lambda: asyncio.run(
                daytona_edit_file.execute(
                    daytona_edit_file.input_model(
                        file_path=write_path,
                        old_text="value = 1",
                        new_text="value = 2",
                        description="live occ edit capture",
                    ),
                    occ_ctx,
                )
            ),
            stats=stats,
        )
        assert not edit_result.is_error, edit_result.output
        edit_data = json.loads(edit_result.output)
        assert edit_data["status"] == "edited"

        rename_result = _measure(
            "occ.ci_rename_symbol(alpha commit)",
            env.trace,
            occ_svc,
            lambda: asyncio.run(
                ci_rename_symbol.execute(
                    ci_rename_symbol.input_model(
                        symbol="alpha",
                        new_name="alpha_occ",
                    ),
                    occ_ctx,
                )
            ),
            stats=stats,
        )
        assert not rename_result.is_error, rename_result.output
        assert json.loads(rename_result.output)["status"] == "renamed"

        codeact_result = _measure(
            "occ.daytona_codeact_python_write",
            env.trace,
            occ_svc,
            lambda: asyncio.run(
                daytona_codeact.execute(
                    daytona_codeact.input_model(
                        mode="python",
                        code='write("pkg/codeact_file.py", "codeact_value = 4\\n")',
                        timeout=180,
                    ),
                    occ_ctx,
                )
            ),
            stats=stats,
        )
        assert not codeact_result.is_error, codeact_result.output
        codeact_data = json.loads(codeact_result.output)
        assert codeact_data["status"] == "ok"
        assert codeact_data["files_written"] >= 1

        edits = occ_svc.arbiter.recent_edits(seconds=300)
        counts = Counter(str(getattr(item, "edit_type", "") or "") for item in edits)
        paths_by_type: dict[str, list[str]] = {}
        for item in edits:
            paths_by_type.setdefault(str(getattr(item, "edit_type", "") or ""), []).append(
                str(getattr(item, "file_path", "") or "")
            )
        payload = {
            "label": "occ.capture_write_edit_rename_codeact",
            "audit_path": "exec_ci_process_operation_for_write_edit_rename_codeact",
            "write_audited": counts.get("write", 0) >= 1,
            "edit_audited": counts.get("edit", 0) >= 1,
            "rename_audited": counts.get("rename", 0) >= 1,
            "codeact_audited": counts.get("codeact", 0) >= 1,
            "counts": dict(sorted(counts.items())),
            "paths_by_type": {key: sorted(value) for key, value in paths_by_type.items()},
            "arbiter_generation": occ_svc.arbiter.generation,
            "total_edits": occ_svc.arbiter.metrics.total_edits,
            "health": _service_health_payload(occ_svc, env),
        }
        print("[ci-lsp-live-occ] " + json.dumps(payload, sort_keys=True), flush=True)
        assert {"write", "edit", "rename", "codeact"}.issubset(counts)
        assert payload["write_audited"] is True
        assert payload["edit_audited"] is True
        assert payload["rename_audited"] is True
        assert payload["codeact_audited"] is True
        assert occ_svc.arbiter.metrics.total_edits >= 4
    finally:
        occ_svc.dispose()


def _run_concurrent_ci_load(
    *,
    env: LiveRenameEnv,
    svc: CodeIntelligenceService,
    ctx: ToolExecutionContext,
    core_path: str,
    uses_path: str,
    stats: list[dict[str, Any]],
) -> None:
    """Run 20 concurrent mixed code-intelligence operations against one service."""
    trace_start = env.trace.mark()
    telemetry_before = svc.lsp_client.telemetry
    tree_before = svc.tree_cache.stats
    index_before = svc.symbol_index.generation
    arbiter_before = svc.arbiter.generation
    started_at = time.perf_counter()

    def _one(index: int) -> dict[str, Any]:
        op_started = time.perf_counter()
        op = index % 5
        try:
            if op == 0:
                result = svc.lsp_client.goto_definition(uses_path, 4, 11)
                assert result
                name = "goto_definition"
                detail = len(result)
            elif op == 1:
                result = svc.lsp_client.find_references(core_path, 1, 0)
                assert len(result) >= 3
                name = "find_references"
                detail = len(result)
            elif op == 2:
                result = svc.lsp_client.hover(core_path, 1, 0)
                assert result is not None
                name = "hover"
                detail = len(result.content)
            elif op == 3:
                result = asyncio.run(
                    ci_rename_symbol.execute(
                        ci_rename_symbol.input_model(
                            symbol="beta",
                            new_name=f"beta_load_{index}",
                            dry_run=True,
                        ),
                        ctx,
                    )
                )
                assert not result.is_error, result.output
                name = "ci_rename_symbol_dry_run"
                detail = len(json.loads(result.output)["files"])
            else:
                result = asyncio.run(
                    ci_rename_symbol.execute(
                        ci_rename_symbol.input_model(
                            symbol="delta",
                            new_name=f"delta_load_{index}",
                            dry_run=True,
                        ),
                        ctx,
                    )
                )
                assert not result.is_error, result.output
                name = "ci_rename_symbol_delta_dry_run"
                detail = len(json.loads(result.output)["files"])
            return {
                "index": index,
                "op": name,
                "ok": True,
                "detail": detail,
                "duration_ms": round((time.perf_counter() - op_started) * 1000, 3),
            }
        except Exception as exc:
            return {
                "index": index,
                "op": str(op),
                "ok": False,
                "error": f"{type(exc).__name__}: {exc}",
                "duration_ms": round((time.perf_counter() - op_started) * 1000, 3),
            }

    with concurrent.futures.ThreadPoolExecutor(max_workers=20) as executor:
        results = list(executor.map(_one, range(20)))

    telemetry_after = svc.lsp_client.telemetry
    failures = [item for item in results if not item["ok"]]
    status = _service_health_payload(svc, env)
    cache_loaded = {
        "lsp_result_cache_hits_delta": telemetry_after.cache_hits - telemetry_before.cache_hits,
        "tree_cache_size": status["tree_cache"]["size"],
        "tree_cache_hits_total": status["tree_cache"]["hits"],
        "rename_preview_cache_size": status["rename_preview_cache"]["entries"],
    }
    payload = {
        "label": "worker.concurrent_mixed_ci_load_20",
        "intent": "read_only_lsp_queries_plus_rename_dry_runs_no_occ_commits",
        "occ_expected": False,
        "cache_loaded": cache_loaded,
        "duration_ms": round((time.perf_counter() - started_at) * 1000, 3),
        "command_count": len(results),
        "failure_count": len(failures),
        "results": results,
        "lsp_queries_delta": telemetry_after.queries - telemetry_before.queries,
        "lsp_successes_delta": telemetry_after.successes - telemetry_before.successes,
        "lsp_errors_delta": telemetry_after.errors - telemetry_before.errors,
        "lsp_cache_hits_delta": telemetry_after.cache_hits - telemetry_before.cache_hits,
        "jedi_script_runs_delta": telemetry_after.script_runs - telemetry_before.script_runs,
        "jedi_script_successes_delta": (
            telemetry_after.script_successes - telemetry_before.script_successes
        ),
        "jedi_script_errors_delta": (
            telemetry_after.script_errors - telemetry_before.script_errors
        ),
        "worker_successes_delta": (
            telemetry_after.worker_successes - telemetry_before.worker_successes
        ),
        "worker_fallbacks_delta": (
            telemetry_after.worker_fallbacks - telemetry_before.worker_fallbacks
        ),
        "worker_errors_delta": telemetry_after.worker_errors - telemetry_before.worker_errors,
        "tree_cache_hits_delta": svc.tree_cache.stats["hits"] - tree_before["hits"],
        "tree_cache_misses_delta": svc.tree_cache.stats["misses"] - tree_before["misses"],
        "tree_cache_stat_calls_delta": (
            svc.tree_cache.stats["stat_calls"] - tree_before["stat_calls"]
        ),
        "status": status,
        "symbol_index_generation_delta": svc.symbol_index.generation - index_before,
        "arbiter_generation_delta": svc.arbiter.generation - arbiter_before,
        **env.trace.summary_since(trace_start),
    }
    stats.append(dict(payload))
    print("[ci-lsp-live-load] " + json.dumps(payload, sort_keys=True), flush=True)
    assert not failures, json.dumps(failures, sort_keys=True)
    assert payload["worker_fallbacks_delta"] == 0
    assert payload["worker_errors_delta"] == 0
    assert payload["lsp_cache_hits_delta"] >= 8
    assert payload["status"]["lsp"]["connected"] is True
    assert payload["status"]["lsp"]["worker_active"] is True
    assert payload["arbiter_generation_delta"] == 0
    assert cache_loaded["tree_cache_size"] >= 1
    assert cache_loaded["tree_cache_hits_total"] >= 1
    assert cache_loaded["rename_preview_cache_size"] >= 2


def _print_mode_comparison(stats: list[dict[str, Any]]) -> None:
    by_label = {str(item["label"]): item for item in stats}
    pairs = [
        ("lsp.goto_definition(alpha usage)", "worker.lsp.goto_definition(alpha usage)"),
        ("lsp.find_references(alpha cold)", "worker.lsp.find_references(alpha cold)"),
        ("lsp.hover(alpha)", "worker.lsp.hover(alpha)"),
        ("tool.ci_rename_symbol(beta dry_run)", "worker.tool.ci_rename_symbol(beta dry_run)"),
    ]
    rows = []
    for base_label, worker_label in pairs:
        base = by_label.get(base_label)
        worker = by_label.get(worker_label)
        if not base or not worker:
            continue
        base_ms = float(base["duration_ms"])
        worker_ms = float(worker["duration_ms"])
        rows.append(
            {
                "baseline": base_label,
                "worker": worker_label,
                "baseline_ms": base_ms,
                "worker_ms": worker_ms,
                "delta_ms": round(worker_ms - base_ms, 3),
                "baseline_exec_count": base["exec_count"],
                "worker_exec_count": worker["exec_count"],
            }
        )
    print("[ci-lsp-live-mode-comparison] " + json.dumps(rows, sort_keys=True), flush=True)


def _apply_optional_thresholds(stats: list[dict[str, Any]], trace: TraceLog) -> None:
    summary = trace.summary_since(0)
    checks = {
        "CI_LSP_LIVE_MAX_OP_MS": max((float(item["duration_ms"]) for item in stats), default=0.0),
        "CI_LSP_LIVE_MAX_TOTAL_EXEC_MS": float(summary["exec_total_ms"]),
        "CI_LSP_LIVE_MAX_EXEC_COUNT": float(summary["exec_count"]),
        "CI_LSP_LIVE_MAX_P95_EXEC_MS": _percentile(trace.durations(), 95),
        "CI_LSP_LIVE_MAX_LOAD_MS": max(
            (
                float(item["duration_ms"])
                for item in stats
                if item.get("label") == "worker.concurrent_mixed_ci_load_20"
            ),
            default=0.0,
        ),
    }
    for env_name, observed in checks.items():
        raw_limit = os.environ.get(env_name)
        if raw_limit is None or not raw_limit.strip():
            continue
        limit = float(raw_limit)
        print(
            "[ci-lsp-live-threshold] "
            + json.dumps(
                {"name": env_name, "observed": observed, "limit": limit},
                sort_keys=True,
            ),
            flush=True,
        )
        assert observed <= limit, f"{env_name}: observed {observed} > limit {limit}"


def _percentile(values: list[float], pct: int) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, round((pct / 100) * (len(ordered) - 1)))
    return round(ordered[index], 3)


def _clean_stdout(stdout: str) -> str:
    return _TERM_NOISE.sub("", stdout)


def _compact(value: str, *, limit: int = _MAX_COMMAND_CHARS) -> str:
    text = value.replace("\n", "\\n")
    if len(text) > limit:
        return text[:limit] + f"... ({len(text)} chars)"
    return text
