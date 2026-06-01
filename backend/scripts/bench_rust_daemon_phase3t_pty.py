#!/usr/bin/env python3
"""Live Docker Phase 3T command, PTY, and non-plugin deferred-gate benchmark."""

from __future__ import annotations

import argparse
import asyncio
import io
import json
import os
import platform
import shlex
import sys
import tarfile
import time
import uuid
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
BACKEND_SRC = ROOT / "backend" / "src"
SCRIPT_DIR = Path(__file__).resolve().parent
if str(BACKEND_SRC) not in sys.path:
    sys.path.insert(0, str(BACKEND_SRC))
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from bench_rust_daemon_phase2 import (  # noqa: E402
    LAYER_STACK_ROOT,
    WORKSPACE_ROOT,
    call_tcp,
    reset_runtime,
    require_success,
    tar_file_at_path,
    temporary_env,
    upload_artifact,
)
from bench_sandbox_e2e import (  # noqa: E402
    DEFAULT_DOCKER_IMAGE,
    DockerBench,
    elapsed_ms,
    summarize_samples,
)

AGENT_ID = "phase3t-pty-bench"
FINITE_TRUE_P95_MS = 60.0
PTY_TRUE_P95_MS = 100.0
PTY_PROGRESS_P95_MS = 20.0
PTY_WRITE_P95_MS = 100.0
PTY_CANCEL_P95_MS = 500.0
PTY_CANCEL_HARD_CLEANUP_MS = 2500.0
ISOLATED_AGENT_ID = "phase3t-isolated-bench"
ISOLATED_AUDIT_PATH = "/eos/isolated-workspace-audit.jsonl"
MIXED_LOAD_P95_MS = 500.0
DEFAULT_CACHE_ROOT_COUNT = 260


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    report = asyncio.run(run(args))
    out = Path(args.report)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2, sort_keys=True))
    print(
        f"wrote {out} "
        f"(gates={report['gate_pass']} finite={report['gates']['finite_true_p95']} "
        f"pty={report['gates']['pty_true_p95']} run_id={report['run_id']})"
    )
    return 0 if report["gate_pass"] else 1


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docker-image", default=DEFAULT_DOCKER_IMAGE)
    parser.add_argument("--container-id", default=None)
    parser.add_argument(
        "--artifact",
        type=Path,
        default=ROOT / "sandbox" / "dist" / "eosd-linux-amd64",
    )
    parser.add_argument(
        "--report",
        default=str(ROOT / "bench" / "phase3t-pty-command-docker.json"),
    )
    parser.add_argument("--samples", type=int, default=10)
    parser.add_argument("--load-concurrency", default="1,3,5,10")
    parser.add_argument("--load-rounds", type=int, default=5)
    parser.add_argument("--skip-isolated", action="store_true")
    parser.add_argument("--skip-mixed-load", action="store_true")
    parser.add_argument("--skip-cache-churn", action="store_true")
    parser.add_argument("--cache-root-count", type=int, default=DEFAULT_CACHE_ROOT_COUNT)
    parser.add_argument("--keep-container", action="store_true")
    parser.add_argument("--name-prefix", default="eos-phase3t-pty")
    return parser.parse_args(argv)


async def run(args: argparse.Namespace) -> dict[str, Any]:
    if not args.artifact.exists():
        raise SystemExit(f"missing eosd artifact: {args.artifact}")
    bench = await DockerBench.create(
        image=args.docker_image,
        container_id=args.container_id,
        name_prefix=args.name_prefix,
    )
    try:
        report: dict[str, Any] = {
            "mode": "docker-phase3t-pty-command",
            "run_id": os.environ.get("EOS_TIER_RUN_ID") or f"local-{uuid.uuid4().hex[:12]}",
            "sandbox_id": bench.sandbox_id,
            "created_container": bench.created,
            "host": {
                "platform": platform.platform(),
                "python": sys.version.split()[0],
            },
            "samples_per_operation": args.samples,
            "load": {
                "concurrency_levels": parse_concurrency(args.load_concurrency),
                "rounds_per_concurrency": args.load_rounds,
            },
        }
        await reset_runtime(bench)
        report["artifact"] = await upload_artifact(bench, args.artifact)
        await configure_daemon_environment(bench)

        with temporary_env("EOS_SANDBOX_RUNTIME", "rust"):
            from sandbox.host import daemon_client

            daemon_client.invalidate_daemon_tcp_endpoint(bench.sandbox_id)
            started = time.perf_counter()
            await daemon_client.ensure_daemon_current(bench.sandbox_id)
            report["daemon_spawn_ms"] = elapsed_ms(started)
            endpoint = await daemon_client._resolve_daemon_tcp_endpoint(  # noqa: SLF001
                bench.adapter,
                bench.sandbox_id,
            )
            if endpoint is None:
                raise RuntimeError("Docker sandbox did not expose a daemon TCP endpoint")
            report["endpoint"] = {
                "host": endpoint.host,
                "port": endpoint.port,
                "internal_port": endpoint.internal_port,
                "auth_token_present": bool(endpoint.auth_token),
            }
            report["layer_stack_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.build_workspace_base",
                {"workspace_root": WORKSPACE_ROOT, "reset": True},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=180,
            )
            report["ready"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.runtime.ready",
                {},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )

            client = CommandClient(daemon_client, endpoint)
            report["correctness"] = await correctness_checks(bench, client)
            report["operations"] = {
                "finite_true": await measure_finite_true(client, args.samples),
                "pty_true": await measure_pty_true(client, args.samples),
                "pty_progress": await measure_pty_progress(client, args.samples),
                "pty_write_echo": await measure_pty_write_echo(client, args.samples),
                "pty_cancel": await measure_pty_cancel(bench, client, args.samples),
            }
            report["load"] = await measure_load_matrix(
                client,
                concurrencies=parse_concurrency(args.load_concurrency),
                rounds=max(0, args.load_rounds),
            )
            report["isolated_exit_inspection"] = (
                {"skipped": True, "gate_pass": True}
                if args.skip_isolated
                else await isolated_exit_inspection_checks(bench, client)
            )
            report["mixed_non_plugin_load"] = (
                {"skipped": True, "gate_pass": True}
                if args.skip_mixed_load
                else await measure_mixed_non_plugin_load(
                    client,
                    concurrencies=parse_concurrency(args.load_concurrency),
                    rounds=max(0, args.load_rounds),
                )
            )
            report["cache_churn"] = (
                {"skipped": True, "gate_pass": True}
                if args.skip_cache_churn
                else await measure_cache_churn(
                    bench,
                    client,
                    root_count=max(0, args.cache_root_count),
                )
            )

        report["gates"] = evaluate_gates(report)
        report["gate_pass"] = bool(
            report["artifact"]["gate_pass"]
            and report["ready"].get("ready") is True
            and report["correctness"]["gate_pass"]
            and all(report["gates"].values())
            and report["load"]["gate_pass"]
            and report["isolated_exit_inspection"]["gate_pass"]
            and report["mixed_non_plugin_load"]["gate_pass"]
            and report["cache_churn"]["gate_pass"]
        )
        return report
    finally:
        await bench.close(keep=args.keep_container)


class CommandClient:
    def __init__(self, daemon_client: Any, endpoint: Any) -> None:
        self.daemon_client = daemon_client
        self.endpoint = endpoint

    async def call(
        self,
        op: str,
        args: dict[str, Any],
        *,
        layer_stack_root: str = LAYER_STACK_ROOT,
        agent_id: str = AGENT_ID,
    ) -> tuple[dict[str, Any], float]:
        invocation_id = args.setdefault("invocation_id", f"phase3t-{uuid.uuid4().hex}")
        wire_args = {
            "layer_stack_root": layer_stack_root,
            "agent_id": agent_id,
            **args,
        }
        payload = json.dumps(
            {"op": op, "invocation_id": invocation_id, "args": wire_args},
            separators=(",", ":"),
        )
        started = time.perf_counter()
        response = await call_tcp(self.daemon_client, self.endpoint, payload)
        return response, elapsed_ms(started)

    async def exec_command(
        self,
        cmd: str,
        *,
        tty: bool,
        yield_time_ms: int = 1000,
        timeout: int = 30,
    ) -> tuple[dict[str, Any], float]:
        return await self.call(
            "api.v1.exec_command",
            {
                "cmd": cmd,
                "tty": tty,
                "yield_time_ms": yield_time_ms,
                "timeout": timeout,
            },
        )

    async def pty_write(
        self,
        pty_session_id: str,
        chars: str,
        *,
        yield_time_ms: int = 100,
        max_tokens: int = 1000,
    ) -> tuple[dict[str, Any], float]:
        return await self.call(
            "api.v1.pty.write_stdin",
            {
                "pty_session_id": pty_session_id,
                "chars": chars,
                "yield_time_ms": yield_time_ms,
                "max_tokens": max_tokens,
            },
        )

    async def pty_progress(
        self,
        pty_session_id: str,
        *,
        seconds: float = 1.0,
        max_tokens: int = 1000,
    ) -> tuple[dict[str, Any], float]:
        return await self.call(
            "api.v1.pty.progress",
            {
                "pty_session_id": pty_session_id,
                "time": seconds,
                "max_tokens": max_tokens,
            },
        )

    async def pty_cancel(self, pty_session_id: str) -> tuple[dict[str, Any], float]:
        return await self.call(
            "api.v1.pty.cancel",
            {"pty_session_id": pty_session_id},
        )


async def correctness_checks(bench: DockerBench, client: CommandClient) -> dict[str, Any]:
    separated, _ = await client.exec_command(
        "echo out; echo err >&2",
        tty=False,
        yield_time_ms=1000,
    )
    python_path, _ = await client.exec_command(
        "python - <<'PY'\nimport sys\nprint(sys.executable)\nPY",
        tty=False,
        yield_time_ms=1000,
    )
    write_path = f"phase3t-write-{uuid.uuid4().hex[:8]}.txt"
    write, _ = await client.exec_command(
        f"printf phase3t > {shlex.quote(write_path)}",
        tty=False,
        yield_time_ms=1000,
    )
    readback, _ = await client.call(
        "api.v1.read_file",
        {"path": f"/testbed/{write_path}"},
    )

    marker = f"eos_phase3t_nohup_{uuid.uuid4().hex[:8]}"
    nohup, _ = await client.exec_command(
        f"nohup bash -c 'exec -a {marker} sleep 60' >/tmp/{marker}.log 2>&1 &",
        tty=False,
        yield_time_ms=1000,
    )
    await asyncio.sleep(0.2)
    descendants = await process_marker_count(bench, marker)

    pty_marker = f"eos_phase3t_pty_nohup_{uuid.uuid4().hex[:8]}"
    pty_nohup, _ = await client.exec_command(
        f"nohup bash -c 'exec -a {pty_marker} sleep 60' "
        f">/tmp/{pty_marker}.log 2>&1 &",
        tty=True,
        yield_time_ms=1000,
    )
    await asyncio.sleep(0.2)
    pty_descendants = await process_marker_count(bench, pty_marker)

    checks = {
        "stdout_stderr_split": (
            separated.get("status") == "ok"
            and separated.get("output", {}).get("stdout") == "out\n"
            and separated.get("output", {}).get("stderr") == "err\n"
        ),
        "python_uses_testbed_env": "/opt/miniconda3/envs/testbed/bin/python"
        in separated_text(python_path),
        "finite_write_published": (
            write.get("status") == "ok" and readback.get("content") == "phase3t"
        ),
        "nohup_descendant_cleanup": nohup.get("status") == "ok" and descendants == 0,
        "pty_nohup_descendant_cleanup": (
            pty_nohup.get("status") == "ok" and pty_descendants == 0
        ),
    }
    return {
        "checks": checks,
        "responses": {
            "separated": trim_response(separated),
            "python_path": trim_response(python_path),
            "write": trim_response(write),
            "readback": {k: readback.get(k) for k in ("success", "content")},
            "nohup": trim_response(nohup),
            "nohup_marker_remaining": descendants,
            "pty_nohup": trim_response(pty_nohup),
            "pty_nohup_marker_remaining": pty_descendants,
        },
        "gate_pass": all(checks.values()),
    }


async def configure_daemon_environment(bench: DockerBench) -> None:
    lines = {
        "EOS_ISOLATED_WORKSPACE_ENABLED": "true",
        "EOS_ISOLATED_WORKSPACE_AUDIT_PATH": ISOLATED_AUDIT_PATH,
        "EOS_ISOLATED_WORKSPACE_SETUP_TIMEOUT_S": "30",
        "EOS_ISOLATED_WORKSPACE_EXIT_GRACE_S": "0.25",
    }
    payload = "\n".join(f"{key}={value}" for key, value in lines.items()) + "\n"
    await bench.exec(
        "cat >> /etc/environment <<'EOF'\n"
        f"{payload}"
        "EOF\n",
        timeout=15,
    )


async def isolated_exit_inspection_checks(
    bench: DockerBench,
    client: CommandClient,
) -> dict[str, Any]:
    enter, enter_ms = await client.call(
        "api.isolated_workspace.enter",
        {},
        agent_id=ISOLATED_AGENT_ID,
    )
    private_path = f"phase3t-isolated-{uuid.uuid4().hex[:8]}.txt"
    finite, finite_ms = await client.call(
        "api.v1.exec_command",
        {
            "cmd": f"printf isolated > {shlex.quote(private_path)}",
            "tty": False,
            "yield_time_ms": 1000,
            "timeout": 30,
        },
        agent_id=ISOLATED_AGENT_ID,
    )
    shared_read_during, _ = await client.call(
        "api.v1.read_file",
        {"path": f"/testbed/{private_path}"},
    )
    pty_start, pty_start_ms = await client.call(
        "api.v1.exec_command",
        {
            "cmd": "sleep 30",
            "tty": True,
            "yield_time_ms": 50,
            "timeout": 60,
        },
        agent_id=ISOLATED_AGENT_ID,
    )
    blocked_exit, blocked_exit_ms = await client.call(
        "api.isolated_workspace.exit",
        {"grace_s": 0.05},
        agent_id=ISOLATED_AGENT_ID,
    )
    forced_exit, forced_exit_ms = await client.call(
        "api.isolated_workspace.exit",
        {"force_cancel": True, "grace_s": 0.5},
        agent_id=ISOLATED_AGENT_ID,
    )
    status_after, _ = await client.call(
        "api.isolated_workspace.status",
        {},
        agent_id=ISOLATED_AGENT_ID,
    )
    shared_read_after, _ = await client.call(
        "api.v1.read_file",
        {"path": f"/testbed/{private_path}"},
    )
    audit_tail = await read_container_text(bench, ISOLATED_AUDIT_PATH)
    exit_audit = last_jsonl_event(audit_tail, "sandbox_isolated_workspace_exit")
    inspection = forced_exit.get("inspection") if isinstance(forced_exit, dict) else None

    checks = {
        "enter_opened": enter.get("success") is True,
        "finite_command_private": (
            finite.get("status") == "ok"
            and finite.get("workspace") == "isolated"
            and private_path in finite.get("changed_paths", [])
        ),
        "private_write_unpublished_during": shared_read_during.get("exists") is False,
        "active_pty_exit_blocked": (
            blocked_exit.get("success") is False
            and blocked_exit.get("error", {}).get("kind") == "active_pty_sessions"
        ),
        "force_exit_succeeded": forced_exit.get("success") is True,
        "force_cancel_cleared_ptys": forced_exit.get("active_pty_session_ids_after") == [],
        "status_closed_after": (
            status_after.get("success") is True and status_after.get("open") is False
        ),
        "private_write_unpublished_after": shared_read_after.get("exists") is False,
        "inspection_clean": inspection_is_clean(inspection),
        "audit_carries_matching_inspection": (
            isinstance(exit_audit, dict)
            and exit_audit.get("payload", {}).get("inspection") == inspection
        ),
    }
    return {
        "checks": checks,
        "latency_ms": {
            "enter": enter_ms,
            "finite": finite_ms,
            "pty_start": pty_start_ms,
            "blocked_exit": blocked_exit_ms,
            "forced_exit": forced_exit_ms,
        },
        "responses": {
            "enter": trim_isolated_response(enter),
            "finite": trim_response(finite),
            "shared_read_during": {
                "success": shared_read_during.get("success"),
                "exists": shared_read_during.get("exists"),
            },
            "pty_start": trim_response(pty_start),
            "blocked_exit": trim_isolated_response(blocked_exit),
            "forced_exit": trim_isolated_response(forced_exit),
            "status_after": status_after,
            "shared_read_after": {
                "success": shared_read_after.get("success"),
                "exists": shared_read_after.get("exists"),
            },
            "exit_audit": exit_audit,
        },
        "gate_pass": all(checks.values()),
    }


async def measure_mixed_non_plugin_load(
    client: CommandClient,
    *,
    concurrencies: list[int],
    rounds: int,
) -> dict[str, Any]:
    total_slots = sum(max(0, concurrency) * max(0, rounds) for concurrency in concurrencies)
    edit_paths = [f"/testbed/phase3t-mixed-edit-{index:04d}.txt" for index in range(total_slots)]
    for path in edit_paths:
        await client.call("api.v1.write_file", {"path": path, "content": "old\n"})

    snapshot, _ = await client.call("api.audit.snapshot", {})
    after_seq = (
        snapshot.get("snapshot", {})
        .get("daemon", {})
        .get("next_seq", 0)
    ) - 1
    operations: dict[str, Any] = {}
    operation_counter = 0

    async def call_for(index: int) -> tuple[dict[str, Any], float, str]:
        operation = index % 8
        if operation == 0:
            response, wall_ms = await client.call(
                "api.v1.read_file",
                {"path": "/testbed/README.md"},
            )
            return response, wall_ms, "read"
        if operation == 1:
            path = f"/testbed/phase3t-mixed-write-{index:04d}.txt"
            response, wall_ms = await client.call(
                "api.v1.write_file",
                {"path": path, "content": f"write-{index}\n"},
            )
            return response, wall_ms, "write"
        if operation == 2:
            response, wall_ms = await client.call(
                "api.v1.edit_file",
                {
                    "path": edit_paths[index % len(edit_paths)],
                    "edits": [
                        {
                            "old_text": "old",
                            "new_text": f"new-{index}",
                            "replace_all": False,
                        }
                    ],
                },
            )
            return response, wall_ms, "edit"
        if operation == 3:
            response, wall_ms = await client.exec_command("true", tty=False)
            return response, wall_ms, "exec_command"
        if operation == 4:
            response, wall_ms = await client.exec_command("true", tty=True)
            return response, wall_ms, "pty_command"
        if operation == 5:
            response, wall_ms = await client.call(
                "api.v1.glob",
                {"pattern": "phase3t-mixed-*.txt", "path": "."},
            )
            return response, wall_ms, "glob"
        if operation == 6:
            response, wall_ms = await client.call(
                "api.v1.grep",
                {
                    "pattern": "README",
                    "path": ".",
                    "output_mode": "content",
                    "line_numbers": True,
                },
            )
            return response, wall_ms, "grep"
        response, wall_ms = await client.call(
            "api.layer_metrics",
            {},
        )
        return response, wall_ms, "layer_metrics"

    for concurrency in concurrencies:
        samples: list[dict[str, Any]] = []
        for round_index in range(max(0, rounds)):
            results = await asyncio.gather(
                *(call_for(operation_counter + slot) for slot in range(concurrency))
            )
            for slot, (response, wall_ms, name) in enumerate(results):
                samples.append(
                    sample(
                        operation_counter + slot,
                        response,
                        wall_ms,
                        mixed_operation_ok(name, response),
                        slot=slot,
                        round_index=round_index,
                    )
                    | {"operation": name}
                )
            operation_counter += concurrency
        operations[str(concurrency)] = summarize_samples_block(samples)

    audit_pull, audit_pull_ms = await client.call(
        "api.audit.pull",
        {"after_seq": after_seq, "limit": max(1000, operation_counter * 3)},
    )
    audit_events = audit_pull.get("events", [])
    audit_buffer = audit_pull.get("buffer", {})
    audit_checks = {
        "pull_success": audit_pull.get("success") is True,
        "no_dropped_events": audit_buffer.get("dropped_event_count") == 0,
        "no_cursor_loss": int(audit_buffer.get("lost_before_seq", 0)) <= max(after_seq + 1, 0),
        "enough_events_for_load": isinstance(audit_events, list)
        and len(audit_events) >= operation_counter,
    }
    gate_pass = (
        all(cell["all_samples_ok"] for cell in operations.values())
        and all(p95(cell) <= MIXED_LOAD_P95_MS for cell in operations.values())
        and all(audit_checks.values())
    )
    return {
        "operations": operations,
        "operation_count": operation_counter,
        "audit": {
            "checks": audit_checks,
            "pull_wall_ms": audit_pull_ms,
            "event_count": len(audit_events) if isinstance(audit_events, list) else 0,
            "cursor": audit_pull.get("cursor"),
            "buffer": audit_buffer,
            "sample_event_types": [
                event.get("type")
                for event in audit_events[:20]
                if isinstance(event, dict)
            ]
            if isinstance(audit_events, list)
            else [],
        },
        "gate_pass": gate_pass,
    }


async def measure_cache_churn(
    bench: DockerBench,
    client: CommandClient,
    *,
    root_count: int,
) -> dict[str, Any]:
    roots = await seed_cache_churn_roots(bench, root_count)
    samples: list[dict[str, Any]] = []
    for index, root in enumerate(roots):
        response, wall_ms = await client.call(
            "api.v1.write_file",
            {
                "path": f"/testbed/phase3t-cache-churn-{index:04d}.txt",
                "content": f"cache-root-{index}\n",
            },
            layer_stack_root=root,
        )
        samples.append(sample(index, response, wall_ms, response.get("success") is True))
    reuse_response: dict[str, Any] = {}
    reuse_ms = 0.0
    if roots:
        reuse_response, reuse_ms = await client.call(
            "api.v1.write_file",
            {
                "path": "/testbed/phase3t-cache-reuse.txt",
                "content": "reuse\n",
            },
            layer_stack_root=roots[-1],
        )
    metrics, metrics_ms = await client.call(
        "api.layer_metrics",
        {},
        layer_stack_root=roots[-1] if roots else LAYER_STACK_ROOT,
    )
    cache = metrics.get("occ_runtime_service_cache", {})
    required_evictions = max(0, root_count - 256)
    checks = {
        "samples_ok": bool(samples) and all(item["ok"] for item in samples),
        "reuse_hit": timing_value(reuse_response, "occ.runtime_service.cache_hit") == 1.0,
        "cache_bounded": int(cache.get("size", 0)) <= 256,
        "evicted_after_churn": int(cache.get("evictions_total", 0)) >= required_evictions,
        "metrics_reported": "lock_wait_s_total" in cache,
    }
    return {
        "root_count": root_count,
        "sample_summary": summarize_samples_block(samples),
        "reuse": {
            "wall_ms": reuse_ms,
            "response": trim_response(reuse_response),
            "runtime_service_timings": runtime_service_timings(reuse_response),
        },
        "metrics_wall_ms": metrics_ms,
        "cache": cache,
        "checks": checks,
        "gate_pass": all(checks.values()),
    }


async def measure_finite_true(client: CommandClient, count: int) -> dict[str, Any]:
    return await measure_series(
        count,
        lambda _i: client.exec_command("true", tty=False, yield_time_ms=1000),
        expect=lambda response: response.get("status") == "ok" and response.get("exit_code") == 0,
    )


async def measure_pty_true(client: CommandClient, count: int) -> dict[str, Any]:
    return await measure_series(
        count,
        lambda _i: client.exec_command("true", tty=True, yield_time_ms=1000),
        expect=lambda response: (
            response.get("status") == "ok"
            and response.get("exit_code") == 0
            and response.get("pty_session_id") is None
        ),
    )


async def measure_pty_progress(client: CommandClient, count: int) -> dict[str, Any]:
    start, _ = await client.exec_command(
        "while true; do echo phase3t-progress; sleep 0.05; done",
        tty=True,
        yield_time_ms=50,
        timeout=30,
    )
    session_id = str(start.get("pty_session_id") or "")
    samples: list[dict[str, Any]] = []
    if session_id:
        for index in range(max(0, count)):
            response, wall_ms = await client.pty_progress(session_id, seconds=1.0)
            samples.append(sample(index, response, wall_ms, "phase3t-progress" in separated_text(response)))
        await client.pty_cancel(session_id)
    return summarize_samples_block(samples, extra={"start": trim_response(start)})


async def measure_pty_write_echo(client: CommandClient, count: int) -> dict[str, Any]:
    samples: list[dict[str, Any]] = []
    for index in range(max(0, count)):
        expected = f"echo:hello-{index}"
        start, _ = await client.exec_command(
            "python -c 'import sys; print(\"ready\", flush=True); line=sys.stdin.readline(); print(\"echo:\" + line.strip(), flush=True)'",
            tty=True,
            yield_time_ms=50,
            timeout=30,
        )
        session_id = str(start.get("pty_session_id") or "")
        if not session_id:
            samples.append(sample(index, start, 0.0, False))
            continue

        write_response, write_ms = await client.pty_write(
            session_id,
            f"hello-{index}\n",
            yield_time_ms=50,
        )
        collected = separated_text(write_response)
        progress_polls = 0
        while expected not in collected and progress_polls < 10:
            progress, _ = await client.pty_progress(session_id, seconds=0.05)
            progress_polls += 1
            collected += separated_text(progress)
            if progress.get("status") != "running":
                break

        ok = expected in collected and write_response.get("status") in {"running", "ok"}
        response = dict(write_response)
        response["output"] = {"stdout": collected, "stderr": ""}
        entry = sample(index, response, write_ms, ok)
        entry["progress_polls"] = progress_polls
        samples.append(entry)

    return summarize_samples_block(samples)


async def measure_pty_cancel(
    bench: DockerBench,
    client: CommandClient,
    count: int,
) -> dict[str, Any]:
    samples: list[dict[str, Any]] = []
    cleanup_ms: list[float] = []
    for index in range(max(0, count)):
        marker = f"eos_phase3t_cancel_{uuid.uuid4().hex[:8]}"
        start, _ = await client.exec_command(
            f"exec -a {marker} sleep 60",
            tty=True,
            yield_time_ms=50,
            timeout=120,
        )
        session_id = str(start.get("pty_session_id") or "")
        if not session_id:
            samples.append(sample(index, start, 0.0, False))
            continue
        response, wall_ms = await client.pty_cancel(session_id)
        cleanup_started = time.perf_counter()
        gone = await wait_for_process_gone(bench, marker, timeout_s=2.5)
        cleanup_ms.append(elapsed_ms(cleanup_started))
        samples.append(sample(index, response, wall_ms, response.get("status") == "cancelled" and gone))
    return summarize_samples_block(samples, extra={"cleanup_ms": summarize_samples(cleanup_ms)})


async def measure_load_matrix(
    client: CommandClient,
    *,
    concurrencies: list[int],
    rounds: int,
) -> dict[str, Any]:
    operations: dict[str, Any] = {"finite_true": {}, "finite_write": {}, "pty_true": {}}
    for concurrency in concurrencies:
        operations["finite_true"][str(concurrency)] = await measure_concurrent(
            concurrency,
            rounds,
            lambda _index: client.exec_command("true", tty=False, yield_time_ms=1000),
            expect=lambda response: response.get("status") == "ok",
        )
        operations["finite_write"][str(concurrency)] = await measure_concurrent(
            concurrency,
            rounds,
            lambda index: client.exec_command(
                f"printf x > phase3t-load-{concurrency}-{index}.txt",
                tty=False,
                yield_time_ms=1000,
            ),
            expect=lambda response: response.get("status") == "ok",
        )
        operations["pty_true"][str(concurrency)] = await measure_concurrent(
            concurrency,
            rounds,
            lambda _index: client.exec_command("true", tty=True, yield_time_ms=1000),
            expect=lambda response: response.get("status") == "ok",
        )
    gate_pass = all(
        cell["all_samples_ok"]
        for op in operations.values()
        for cell in op.values()
    )
    return {
        "operations": operations,
        "gate_pass": gate_pass,
    }


async def measure_series(
    count: int,
    call: Callable[[int], Awaitable[tuple[dict[str, Any], float]]],
    *,
    expect: Callable[[dict[str, Any]], bool],
) -> dict[str, Any]:
    samples = []
    for index in range(max(0, count)):
        response, wall_ms = await call(index)
        samples.append(sample(index, response, wall_ms, expect(response)))
    return summarize_samples_block(samples)


async def measure_concurrent(
    concurrency: int,
    rounds: int,
    call: Callable[[int], Awaitable[tuple[dict[str, Any], float]]],
    *,
    expect: Callable[[dict[str, Any]], bool],
) -> dict[str, Any]:
    samples: list[dict[str, Any]] = []
    waves: list[float] = []
    for round_index in range(max(0, rounds)):
        started = time.perf_counter()
        results = await asyncio.gather(
            *(call(round_index * concurrency + slot) for slot in range(concurrency))
        )
        waves.append(elapsed_ms(started))
        for slot, (response, wall_ms) in enumerate(results):
            samples.append(
                sample(
                    round_index * concurrency + slot,
                    response,
                    wall_ms,
                    expect(response),
                    slot=slot,
                    round_index=round_index,
                )
            )
    block = summarize_samples_block(samples)
    block["wave_wall_ms"] = summarize_samples(waves)
    block["concurrency"] = concurrency
    block["rounds"] = rounds
    return block


def sample(
    index: int,
    response: dict[str, Any],
    wall_ms: float,
    ok: bool,
    *,
    slot: int | None = None,
    round_index: int | None = None,
) -> dict[str, Any]:
    out = {
        "index": index,
        "host_wall_ms": wall_ms,
        "ok": ok,
        "status": response.get("status"),
        "exit_code": response.get("exit_code"),
        "pty_session_id_present": bool(response.get("pty_session_id")),
        "response": trim_response(response),
    }
    if slot is not None:
        out["slot"] = slot
    if round_index is not None:
        out["round"] = round_index
    return out


def summarize_samples_block(
    samples: list[dict[str, Any]],
    *,
    extra: dict[str, Any] | None = None,
) -> dict[str, Any]:
    block = {
        "sample_count": len(samples),
        "success_count": sum(1 for item in samples if item["ok"]),
        "all_samples_ok": bool(samples) and all(item["ok"] for item in samples),
        "host_wall_ms": summarize_samples([float(item["host_wall_ms"]) for item in samples]),
        "samples": samples,
    }
    if extra:
        block.update(extra)
    return block


def evaluate_gates(report: dict[str, Any]) -> dict[str, bool]:
    ops = report["operations"]
    pty_cancel = ops["pty_cancel"]
    return {
        "operation_samples_ok": all(block["all_samples_ok"] for block in ops.values()),
        "finite_true_p95": p95(ops["finite_true"]) <= FINITE_TRUE_P95_MS,
        "pty_true_p95": p95(ops["pty_true"]) <= PTY_TRUE_P95_MS,
        "pty_progress_p95": p95(ops["pty_progress"]) <= PTY_PROGRESS_P95_MS,
        "pty_write_echo_p95": p95(ops["pty_write_echo"]) <= PTY_WRITE_P95_MS,
        "pty_cancel_p95": p95(pty_cancel) <= PTY_CANCEL_P95_MS,
        "pty_cancel_hard_cleanup": p95_summary(pty_cancel.get("cleanup_ms", {}))
        <= PTY_CANCEL_HARD_CLEANUP_MS,
    }


def p95(block: dict[str, Any]) -> float:
    return p95_summary(block.get("host_wall_ms", {}))


def p95_summary(summary: dict[str, Any]) -> float:
    value = summary.get("p95")
    return float(value) if isinstance(value, int | float) else float("inf")


async def process_marker_count(bench: DockerBench, marker: str) -> int:
    result = await bench.exec(
        f"ps -eo args | grep {shlex.quote(marker)} | grep -v grep | wc -l",
        timeout=15,
    )
    try:
        return int(getattr(result, "stdout", "0").strip() or "0")
    except ValueError:
        return -1


async def wait_for_process_gone(
    bench: DockerBench,
    marker: str,
    *,
    timeout_s: float,
) -> bool:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if await process_marker_count(bench, marker) == 0:
            return True
        await asyncio.sleep(0.05)
    return await process_marker_count(bench, marker) == 0


def inspection_is_clean(inspection: Any) -> bool:
    if not isinstance(inspection, dict):
        return False
    exact = {
        "handle_registered_after": False,
        "agent_registered_after": False,
        "open_handle_count_after": 0,
        "open_agent_count_after": 0,
        "lease_released": True,
        "active_leases_after": 0,
        "scratch_exists_after": False,
        "upperdir_exists_after": False,
        "workdir_exists_after": False,
    }
    for key, expected in exact.items():
        if inspection.get(key) != expected:
            return False
    if inspection.get("holder_kill_error") is not None:
        return False
    if inspection.get("cgroup_exists_after") not in {False, None}:
        return False
    mount_refs = inspection.get("mountinfo_reference_count_after")
    return mount_refs in {0, None}


async def read_container_text(bench: DockerBench, path: str) -> str:
    result = await bench.exec(
        f"if [ -r {shlex.quote(path)} ]; then cat {shlex.quote(path)}; fi",
        timeout=15,
    )
    return getattr(result, "stdout", "")


def last_jsonl_event(raw: str, event_type: str) -> dict[str, Any] | None:
    matched = None
    for line in raw.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict) and event.get("type") == event_type:
            matched = event
    return matched


def mixed_operation_ok(name: str, response: dict[str, Any]) -> bool:
    if name == "read":
        return response.get("success") is True and response.get("exists") is True
    if name in {"write", "edit"}:
        return response.get("success") is True and response.get("status") == "committed"
    if name in {"exec_command", "pty_command"}:
        return response.get("status") == "ok" and response.get("exit_code") == 0
    if name in {"glob", "grep"}:
        return response.get("success") is True or response.get("status") == "ok"
    if name == "layer_metrics":
        return response.get("success") is True and "occ_runtime_service_cache" in response
    return False


async def seed_cache_churn_roots(bench: DockerBench, root_count: int) -> list[str]:
    roots = [f"/eos/cache-churn/root-{index:04d}" for index in range(root_count)]
    if not roots:
        return roots
    raw = io.BytesIO()
    added_dirs: set[str] = set()
    with tarfile.open(fileobj=raw, mode="w") as tar:
        add_tar_dir(tar, "/eos/cache-churn", added_dirs)
        for root in roots:
            add_tar_dir(tar, root, added_dirs)
            add_tar_dir(tar, f"{root}/layers", added_dirs)
            add_tar_dir(tar, f"{root}/layers/B000001-base", added_dirs)
            add_tar_dir(tar, f"{root}/staging", added_dirs)
            add_tar_file(
                tar,
                f"{root}/manifest.json",
                json.dumps(
                    {
                        "schema_version": 1,
                        "version": 1,
                        "layers": [
                            {
                                "layer_id": "B000001-base",
                                "path": "layers/B000001-base",
                            }
                        ],
                    },
                    separators=(",", ":"),
                ).encode(),
                added_dirs,
            )
            add_tar_file(
                tar,
                f"{root}/workspace.json",
                json.dumps(
                    {
                        "workspace_root": WORKSPACE_ROOT,
                        "layer_stack_root": root,
                        "active_manifest_version": 1,
                        "active_root_hash": f"phase3t-cache-root-{root_count}",
                        "base_manifest_version": 1,
                        "base_root_hash": f"phase3t-cache-base-{root_count}",
                    },
                    separators=(",", ":"),
                ).encode(),
                added_dirs,
            )
            add_tar_file(
                tar,
                f"{root}/layers/B000001-base/README.md",
                b"# cache churn\n",
                added_dirs,
            )
    staging_dir = f"/tmp/eos-cache-churn-{uuid.uuid4().hex}"
    staging_tar = f"{staging_dir}/cache-roots.tar"
    require_success(
        await bench.exec(f"mkdir -p {shlex.quote(staging_dir)}", timeout=30),
        "create cache churn staging dir",
    )
    await bench.adapter.put_archive(
        bench.sandbox_id,
        tar_stream=tar_file_at_path("cache-roots.tar", raw.getvalue(), mode=0o644),
        dest_dir=staging_dir,
    )
    require_success(
        await bench.exec(
            f"tar -xf {shlex.quote(staging_tar)} -C / && "
            f"rm -rf {shlex.quote(staging_dir)}",
            timeout=120,
        ),
        "extract cache churn roots",
    )
    return roots


def add_tar_dir(tar: tarfile.TarFile, path: str, added: set[str]) -> None:
    name = path.strip("/")
    if not name or name in added:
        return
    parent = str(Path(name).parent)
    if parent and parent != ".":
        add_tar_dir(tar, f"/{parent}", added)
    info = tarfile.TarInfo(name)
    info.type = tarfile.DIRTYPE
    info.mode = 0o755
    info.uid = 0
    info.gid = 0
    info.mtime = 0
    tar.addfile(info)
    added.add(name)


def add_tar_file(
    tar: tarfile.TarFile,
    path: str,
    payload: bytes,
    added: set[str],
) -> None:
    name = path.strip("/")
    parent = str(Path(name).parent)
    if parent and parent != ".":
        add_tar_dir(tar, f"/{parent}", added)
    info = tarfile.TarInfo(name)
    info.size = len(payload)
    info.mode = 0o644
    info.uid = 0
    info.gid = 0
    info.mtime = 0
    tar.addfile(info, io.BytesIO(payload))


def timing_value(response: dict[str, Any], key: str) -> float | None:
    timings = response.get("timings")
    if not isinstance(timings, dict):
        return None
    value = timings.get(key)
    return float(value) if isinstance(value, int | float) else None


def runtime_service_timings(response: dict[str, Any]) -> dict[str, float]:
    timings = response.get("timings")
    if not isinstance(timings, dict):
        return {}
    return {
        key: float(value)
        for key, value in timings.items()
        if key.startswith("occ.runtime_service.") and isinstance(value, int | float)
    }


def trim_isolated_response(response: dict[str, Any]) -> dict[str, Any]:
    return {
        key: response.get(key)
        for key in (
            "success",
            "workspace_handle_id",
            "manifest_version",
            "manifest_root_hash",
            "inspection",
            "force_cancel_requested",
            "force_cancelled_pty_session_ids",
            "stale_pty_session_ids",
            "active_pty_session_ids_after",
            "error",
        )
        if key in response
    }


def trim_response(response: dict[str, Any]) -> dict[str, Any]:
    trimmed = {
        key: response.get(key)
        for key in (
            "success",
            "status",
            "exit_code",
            "pty_session_id",
            "changed_paths",
            "conflict",
            "conflict_reason",
            "error",
        )
        if key in response
    }
    output = response.get("output")
    if isinstance(output, dict):
        trimmed["output"] = {
            "stdout": str(output.get("stdout", ""))[-500:],
            "stderr": str(output.get("stderr", ""))[-500:],
        }
    return trimmed


def separated_text(response: dict[str, Any]) -> str:
    output = response.get("output")
    if not isinstance(output, dict):
        return ""
    return f"{output.get('stdout', '')}{output.get('stderr', '')}"


def parse_concurrency(raw: str) -> list[int]:
    levels = []
    for item in raw.split(","):
        item = item.strip()
        if item:
            levels.append(int(item))
    return levels or [1]


if __name__ == "__main__":
    raise SystemExit(main())
