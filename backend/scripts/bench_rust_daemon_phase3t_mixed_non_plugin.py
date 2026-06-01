#!/usr/bin/env python3
"""Live Docker CP-4/AV-4 mixed non-plugin Rust daemon gate."""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
import platform
import sys
import time
import uuid
from collections import Counter
from collections.abc import Callable
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
    temporary_env,
    upload_artifact,
)
from bench_sandbox_e2e import (  # noqa: E402
    DEFAULT_DOCKER_IMAGE,
    DockerBench,
    collect_environment,
    elapsed_ms,
    summarize_samples,
)

AGENT_ID = "phase3t-cp4-av4-bench"
REPORT_STEM = "phase3t-mixed-non-plugin-cp4-av4-20260601"
CP4_DIR = "cp4-mixed"
TRANSIENT_DAEMON_IO_MARKERS = (
    "EOS_DAEMON_IO_FAILED:empty_response",
    "EOS_DAEMON_IO_FAILED:asyncio.TimeoutError",
)
DAEMON_RETRY_DELAYS_S = (0.2, 0.5, 1.0, 2.0)
RETRYABLE_DAEMON_OPS = {
    "api.audit.pull",
    "api.audit.snapshot",
    "api.layer_metrics",
    "api.runtime.ready",
    "api.v1.glob",
    "api.v1.grep",
    "api.v1.read_file",
}


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    report = asyncio.run(run(args))
    report_path = Path(args.report)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True))
    audit_path = Path(args.audit_jsonl)
    audit_path.write_text(
        "".join(json.dumps(event, sort_keys=True) + "\n" for event in report["audit"]["events"])
    )
    performance_path = Path(args.performance_json)
    performance_path.write_text(json.dumps(performance_report(report), indent=2, sort_keys=True))
    markdown_path = Path(args.performance_md)
    markdown_path.write_text(performance_markdown(report))
    print(
        f"wrote {report_path} (gate={report['gate_pass']} "
        f"cp4={report['cp4']['gate_pass']} av4={report['av4']['gate_pass']} "
        f"run_id={report['run_id']})"
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
    parser.add_argument("--report", default=str(ROOT / "bench" / f"{REPORT_STEM}.json"))
    parser.add_argument(
        "--audit-jsonl",
        default=str(ROOT / "bench" / f"{REPORT_STEM}.sandbox_events.jsonl"),
    )
    parser.add_argument(
        "--performance-json",
        default=str(ROOT / "bench" / f"{REPORT_STEM}.performance_report.json"),
    )
    parser.add_argument(
        "--performance-md",
        default=str(ROOT / "bench" / f"{REPORT_STEM}.performance_report.md"),
    )
    parser.add_argument("--load-concurrency", default="1,3,5,10")
    parser.add_argument("--load-rounds", type=int, default=3)
    parser.add_argument(
        "--operations",
        default=None,
        help="Comma-separated operation subset for debugging; defaults to the full CP-4 matrix.",
    )
    parser.add_argument("--keep-container", action="store_true")
    parser.add_argument("--name-prefix", default="eos-phase3t-cp4-av4")
    return parser.parse_args(argv)


async def run(args: argparse.Namespace) -> dict[str, Any]:
    if not args.artifact.exists():
        raise SystemExit(f"missing eosd artifact: {args.artifact}")
    concurrencies = parse_concurrency(args.load_concurrency)
    selected_operations = parse_operations(args.operations)
    rounds = max(0, args.load_rounds)
    bench = await DockerBench.create(
        image=args.docker_image,
        container_id=args.container_id,
        name_prefix=args.name_prefix,
    )
    try:
        report: dict[str, Any] = {
            "mode": "docker-phase3t-mixed-non-plugin-cp4-av4",
            "run_id": os.environ.get("EOS_TIER_RUN_ID") or f"local-{uuid.uuid4().hex[:12]}",
            "sandbox_id": bench.sandbox_id,
            "created_container": bench.created,
            "host": {"platform": platform.platform(), "python": sys.version.split()[0]},
            "environment": await collect_environment(bench),
            "load": {
                "concurrency_levels": concurrencies,
                "rounds_per_concurrency": rounds,
                "selected_operations": selected_operations,
            },
            "artifact_paths": {
                "report": args.report,
                "sandbox_events_jsonl": args.audit_jsonl,
                "performance_report_json": args.performance_json,
                "performance_report_md": args.performance_md,
            },
        }
        await reset_runtime(bench)
        report["artifact"] = await upload_artifact(bench, args.artifact)

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
            client = DaemonClient(daemon_client, endpoint)
            report["endpoint"] = {
                "host": endpoint.host,
                "port": endpoint.port,
                "internal_port": endpoint.internal_port,
                "auth_token_present": bool(endpoint.auth_token),
            }
            report["layer_stack_seed"] = await call_daemon_api_retry(
                daemon_client,
                bench.sandbox_id,
                "api.build_workspace_base",
                {"workspace_root": WORKSPACE_ROOT, "reset": True},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=180,
            )
            report["ready"] = await call_daemon_api_retry(
                daemon_client,
                bench.sandbox_id,
                "api.runtime.ready",
                {},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["setup"] = await setup_fixtures(client, concurrencies, rounds)
            baseline_audit = await client.call("api.audit.pull", {"after_seq": -1, "limit": 10_000})
            report["audit_baseline_cursor"] = baseline_audit.get("cursor", {})
            after_seq = int(report["audit_baseline_cursor"].get("after_seq", -1))
            before_metrics = await client.call("api.layer_metrics")
            report["before_metrics"] = before_metrics
            load = await run_load_matrix(
                client,
                concurrencies=concurrencies,
                rounds=rounds,
                selected_operations=selected_operations,
            )
            after_metrics = await client.call("api.layer_metrics")
            audit = await collect_audit(client, after_seq=after_seq)
            final_state = await collect_final_state(client, load)
            report["load"] = {**report["load"], **load}
            report["after_metrics"] = after_metrics
            report["audit"] = audit
            report["final_state"] = final_state

        report["cp4"] = evaluate_cp4(report)
        report["av4"] = evaluate_av4(report)
        report["gate_pass"] = bool(
            report["artifact"]["gate_pass"]
            and report["ready"].get("ready") is True
            and report["cp4"]["gate_pass"]
            and report["av4"]["gate_pass"]
        )
        return report
    finally:
        await bench.close(keep=args.keep_container)


async def call_daemon_api_retry(
    daemon_client: Any,
    sandbox_id: str,
    op: str,
    args: dict[str, Any],
    *,
    timeout: int,
    layer_stack_root: str,
) -> dict[str, Any]:
    """Retry bootstrap/read-only daemon calls after transient empty TCP output."""
    last_exc: Exception | None = None
    for delay_s in (*DAEMON_RETRY_DELAYS_S, None):
        try:
            return await daemon_client.call_daemon_api(
                sandbox_id,
                op,
                args,
                timeout=timeout,
                layer_stack_root=layer_stack_root,
            )
        except Exception as exc:
            if not is_transient_daemon_io(exc):
                raise
            last_exc = exc
            if delay_s is None:
                break
            await asyncio.sleep(delay_s)
    assert last_exc is not None
    raise last_exc


async def call_tcp_retry(
    daemon_client: Any,
    endpoint: Any,
    payload: str,
    *,
    retryable: bool,
) -> dict[str, Any]:
    last_exc: Exception | None = None
    for delay_s in (*DAEMON_RETRY_DELAYS_S, None):
        try:
            return await call_tcp(daemon_client, endpoint, payload)
        except Exception as exc:
            if not retryable or not is_transient_daemon_io(exc):
                raise
            last_exc = exc
            if delay_s is None:
                break
            await asyncio.sleep(delay_s)
    assert last_exc is not None
    raise last_exc


def is_transient_daemon_io(exc: Exception) -> bool:
    message = str(exc)
    return any(marker in message for marker in TRANSIENT_DAEMON_IO_MARKERS)


class DaemonClient:
    def __init__(self, daemon_client: Any, endpoint: Any) -> None:
        self.daemon_client = daemon_client
        self.endpoint = endpoint

    async def call(self, op: str, args: dict[str, Any] | None = None) -> dict[str, Any]:
        invocation_id = (args or {}).get("invocation_id") or f"phase3t-cp4-{uuid.uuid4().hex}"
        payload = daemon_request(op, args or {}, str(invocation_id))
        return await call_tcp_retry(
            self.daemon_client,
            self.endpoint,
            payload,
            retryable=op in RETRYABLE_DAEMON_OPS,
        )

    async def read_file(self, path: str) -> dict[str, Any]:
        return await self.call("api.v1.read_file", {"path": f"{WORKSPACE_ROOT}/{path}"})

    async def write_file(self, path: str, content: str, *, overwrite: bool = True) -> dict[str, Any]:
        return await self.call(
            "api.v1.write_file",
            {"path": f"{WORKSPACE_ROOT}/{path}", "content": content, "overwrite": overwrite},
        )

    async def edit_file(self, path: str, old: str, new: str) -> dict[str, Any]:
        return await self.call(
            "api.v1.edit_file",
            {
                "path": f"{WORKSPACE_ROOT}/{path}",
                "edits": [{"old_text": old, "new_text": new, "replace_all": False}],
            },
        )

    async def exec_command(
        self,
        cmd: str,
        *,
        tty: bool,
        yield_time_ms: int = 1000,
        timeout: int = 30,
    ) -> dict[str, Any]:
        return await self.call(
            "api.v1.exec_command",
            {"cmd": cmd, "tty": tty, "yield_time_ms": yield_time_ms, "timeout": timeout},
        )

    async def pty_write(self, pty_session_id: str, chars: str) -> dict[str, Any]:
        return await self.call(
            "api.v1.pty.write_stdin",
            {
                "pty_session_id": pty_session_id,
                "chars": chars,
                "yield_time_ms": 100,
                "max_tokens": 2000,
            },
        )

    async def pty_progress(self, pty_session_id: str) -> dict[str, Any]:
        return await self.call(
            "api.v1.pty.progress",
            {
                "pty_session_id": pty_session_id,
                "time": 0.05,
                "max_tokens": 2000,
            },
        )

    async def pty_cancel(self, pty_session_id: str) -> dict[str, Any]:
        return await self.call("api.v1.pty.cancel", {"pty_session_id": pty_session_id})


async def setup_fixtures(
    client: DaemonClient,
    concurrencies: list[int],
    rounds: int,
) -> dict[str, Any]:
    max_index = max(concurrencies or [1]) * max(rounds, 1) + 10
    writes = [
        await client.write_file(f"{CP4_DIR}/read-target.txt", "read-target\n"),
        await client.write_file(f"{CP4_DIR}/grep-target.txt", "needle one\nneedle two\n"),
        await client.write_file(f"{CP4_DIR}/conflict.txt", "base-conflict\n"),
    ]
    for index in range(max_index):
        writes.append(await client.write_file(f"{CP4_DIR}/edit-{index:04d}.txt", "base\n"))
        writes.append(
            await client.write_file(f"{CP4_DIR}/mixed-edit-{index:04d}.txt", "mixed-base\n")
        )
    return {
        "seed_write_count": len(writes),
        "all_seed_writes_ok": all(response.get("success") is True for response in writes),
        "max_index": max_index,
    }


async def run_load_matrix(
    client: DaemonClient,
    *,
    concurrencies: list[int],
    rounds: int,
    selected_operations: list[str],
) -> dict[str, Any]:
    builders = operation_builders()
    operations: dict[str, dict[str, Any]] = {key: {} for key in selected_operations}
    expected_paths: dict[str, str] = {}
    for concurrency in concurrencies:
        for name in selected_operations:
            builder = builders[name]
            block = await measure_concurrent(
                client,
                name=name,
                concurrency=concurrency,
                rounds=rounds,
                build=builder,
            )
            operations[name][str(concurrency)] = block
            expected_paths.update(block.get("expected_paths", {}))
    return {
        "operations": operations,
        "expected_paths": expected_paths,
    }


def operation_builders() -> dict[str, Callable[[int], LoadCall]]:
    return {
        "read_heavy": lambda index: LoadCall(
            "api.v1.read_file",
            {"path": f"{WORKSPACE_ROOT}/{CP4_DIR}/read-target.txt"},
            expect_read,
        ),
        "write_heavy": lambda index: write_call(f"{CP4_DIR}/write-{index:04d}.txt", f"write-{index}\n"),
        "edit_heavy": lambda index: edit_call(
            f"{CP4_DIR}/edit-{index:04d}.txt", "base", f"edited-{index}"
        ),
        "conflict_heavy": lambda index: edit_call(
            f"{CP4_DIR}/conflict.txt", "base-conflict", f"conflict-winner-{index}"
        ),
        "exec_tty_false": lambda index: exec_call(
            f"printf exec-false-{index} > {CP4_DIR}/exec-false-{index:04d}.txt",
            tty=False,
            path=f"{CP4_DIR}/exec-false-{index:04d}.txt",
            content=f"exec-false-{index}",
        ),
        "exec_tty_true": lambda index: exec_call(
            f"printf exec-true-{index} > {CP4_DIR}/exec-true-{index:04d}.txt",
            tty=True,
            path=f"{CP4_DIR}/exec-true-{index:04d}.txt",
            content=f"exec-true-{index}",
        ),
        "glob": lambda _index: LoadCall(
            "api.v1.glob",
            {"pattern": "cp4-mixed/*.txt", "path": "."},
            lambda response: response.get("success") is True and int(response.get("num_files") or 0) > 0,
        ),
        "grep": lambda _index: LoadCall(
            "api.v1.grep",
            {
                "pattern": "needle",
                "path": ".",
                "output_mode": "content",
                "offset": 0,
                "case_insensitive": False,
                "line_numbers": True,
                "multiline": False,
            },
            lambda response: response.get("success") is True and int(response.get("num_files") or 0) > 0,
        ),
        "pty_input": lambda index: LoadCall(
            "pty_input",
            {"index": index},
            lambda response: response.get("status") in {"ok", "running"} and "echo:input-" in text_out(response),
        ),
        "pty_long_session": lambda index: LoadCall(
            "pty_long_session",
            {"index": index},
            lambda response: response.get("status") == "cancelled",
        ),
        "mixed_shared": mixed_call,
    }


class LoadCall:
    def __init__(
        self,
        op: str,
        args: dict[str, Any],
        expect: Callable[[dict[str, Any]], bool],
        *,
        expected_path: str | None = None,
        expected_content: str | None = None,
        accepts_conflict: bool = False,
    ) -> None:
        self.op = op
        self.args = args
        self.expect = expect
        self.expected_path = expected_path
        self.expected_content = expected_content
        self.accepts_conflict = accepts_conflict


def write_call(path: str, content: str) -> LoadCall:
    return LoadCall(
        "api.v1.write_file",
        {"path": f"{WORKSPACE_ROOT}/{path}", "content": content, "overwrite": True},
        expect_publish,
        expected_path=path,
        expected_content=content,
    )


def edit_call(path: str, old: str, new: str) -> LoadCall:
    return LoadCall(
        "api.v1.edit_file",
        {
            "path": f"{WORKSPACE_ROOT}/{path}",
            "edits": [{"old_text": old, "new_text": new, "replace_all": False}],
        },
        expect_publish_or_conflict,
        expected_path=path,
        expected_content=f"{new}\n",
        accepts_conflict=True,
    )


def exec_call(
    cmd: str,
    *,
    tty: bool,
    path: str,
    content: str,
) -> LoadCall:
    return LoadCall(
        "api.v1.exec_command",
        {"cmd": cmd, "tty": tty, "yield_time_ms": 1000, "timeout": 30},
        lambda response: response.get("status") == "ok" and response.get("exit_code") == 0,
        expected_path=path,
        expected_content=content,
    )


def mixed_call(index: int) -> LoadCall:
    choices = [
        operation_builders()["read_heavy"],
        operation_builders()["write_heavy"],
        operation_builders()["edit_heavy"],
        operation_builders()["exec_tty_false"],
        operation_builders()["exec_tty_true"],
        operation_builders()["glob"],
        operation_builders()["grep"],
    ]
    return choices[index % len(choices)](index)


async def measure_concurrent(
    client: DaemonClient,
    *,
    name: str,
    concurrency: int,
    rounds: int,
    build: Callable[[int], LoadCall],
) -> dict[str, Any]:
    samples: list[dict[str, Any]] = []
    waves: list[float] = []
    for round_index in range(rounds):
        started = time.perf_counter()
        wave = await asyncio.gather(
            *(run_load_call(client, build(round_index * concurrency + slot), round_index, slot) for slot in range(concurrency))
        )
        waves.append(elapsed_ms(started))
        samples.extend(wave)
    expected_paths = {
        sample["expected_path"]: sample["expected_content"]
        for sample in samples
        if sample.get("ok")
        and sample.get("expected_path")
        and sample.get("expected_content") is not None
        and not sample.get("conflict")
    }
    conflicts = sum(1 for sample in samples if sample.get("conflict"))
    block = summarize_samples_block(samples)
    block.update(
        {
            "concurrency": concurrency,
            "rounds": rounds,
            "wave_wall_ms": summarize_samples(waves),
            "conflict_count": conflicts,
            "expected_paths": expected_paths,
        }
    )
    return block


async def run_load_call(
    client: DaemonClient,
    call: LoadCall,
    round_index: int,
    slot: int,
) -> dict[str, Any]:
    started = time.perf_counter()
    if call.op == "pty_input":
        response = await run_pty_input(client, int(call.args["index"]))
    elif call.op == "pty_long_session":
        response = await run_pty_long_session(client)
    else:
        response = await client.call(call.op, call.args)
    host_wall_ms = elapsed_ms(started)
    conflict = response.get("conflict") not in (None, {})
    ok = call.expect(response)
    if call.accepts_conflict and conflict:
        ok = True
    return {
        "round": round_index,
        "slot": slot,
        "op": call.op,
        "host_wall_ms": host_wall_ms,
        "ok": ok,
        "conflict": conflict,
        "status": response.get("status"),
        "success": response.get("success"),
        "exit_code": response.get("exit_code"),
        "expected_path": call.expected_path,
        "expected_content": call.expected_content,
        "timings_ms": timing_ms(response),
        "response": trim_response(response),
    }


async def run_pty_input(client: DaemonClient, index: int) -> dict[str, Any]:
    expected = f"echo:input-{index}"
    start = await client.exec_command(
        "python3 -c 'import sys; print(\"ready\", flush=True); line=sys.stdin.readline(); print(\"echo:\" + line.strip(), flush=True)'",
        tty=True,
        yield_time_ms=50,
        timeout=30,
    )
    session_id = str(start.get("pty_session_id") or "")
    if not session_id:
        return start
    write = await client.pty_write(session_id, f"input-{index}\n")
    write["start_response"] = trim_response(start)
    collected = text_out(write)
    progress_polls = 0
    while expected not in collected and progress_polls < 10:
        progress = await client.pty_progress(session_id)
        progress_polls += 1
        collected += text_out(progress)
        if progress.get("status") != "running":
            break
    if collected != text_out(write):
        write["output"] = {"stdout": collected, "stderr": ""}
    write["progress_polls"] = progress_polls
    return write


async def run_pty_long_session(client: DaemonClient) -> dict[str, Any]:
    start = await client.exec_command("sleep 30", tty=True, yield_time_ms=50, timeout=60)
    session_id = str(start.get("pty_session_id") or "")
    if not session_id:
        return start
    cancel = await client.pty_cancel(session_id)
    cancel["start_response"] = trim_response(start)
    return cancel


async def collect_audit(client: DaemonClient, *, after_seq: int) -> dict[str, Any]:
    events: list[dict[str, Any]] = []
    cursor = after_seq
    for _pull_index in range(20):
        pulled = await client.call("api.audit.pull", {"after_seq": cursor, "limit": 10_000})
        batch = pulled.get("events", [])
        if isinstance(batch, list):
            events.extend(event for event in batch if isinstance(event, dict))
        next_cursor = int(pulled.get("cursor", {}).get("after_seq", cursor))
        if not batch or next_cursor <= cursor:
            snapshot = await client.call("api.audit.snapshot")
            return summarize_audit(events, pulled, snapshot)
        cursor = next_cursor
    raise RuntimeError("audit pull did not converge after 20 batches")


def summarize_audit(
    events: list[dict[str, Any]],
    last_pull: dict[str, Any],
    snapshot: dict[str, Any],
) -> dict[str, Any]:
    counts = Counter(str(event.get("type")) for event in events)
    buffer = last_pull.get("buffer", {}) if isinstance(last_pull.get("buffer"), dict) else {}
    required_types = [
        "tool_call.completed",
        "occ.publish",
        "occ.conflict",
        "layer_stack.lease_released",
        "overlay_workspace.cleanup",
        "layer_stack.maintenance",
        "background_tool.started",
        "background_tool.input",
        "background_tool.cancelled",
    ]
    return {
        "schema": last_pull.get("schema"),
        "event_count": len(events),
        "event_type_counts": dict(sorted(counts.items())),
        "required_event_types": required_types,
        "missing_required_event_types": [kind for kind in required_types if counts[kind] == 0],
        "events": events,
        "last_pull_buffer": buffer,
        "snapshot_buffer": snapshot.get("buffer", {}),
        "drop_free": buffer.get("dropped_event_count") == 0
        and buffer.get("lost_before_seq") == 0,
        "buffer_pressure_ok": float(buffer.get("pressure") or 0.0) < 0.8,
        "artifact_event_bytes": sum(len(json.dumps(event, sort_keys=True)) + 1 for event in events),
    }


async def collect_final_state(client: DaemonClient, load: dict[str, Any]) -> dict[str, Any]:
    expected = load.get("expected_paths", {})
    contents: dict[str, str] = {}
    mismatches: dict[str, dict[str, str]] = {}
    for path, expected_content in sorted(expected.items()):
        read = await client.read_file(path)
        content = str(read.get("content", ""))
        contents[path] = content
        if content != expected_content:
            mismatches[path] = {"expected": expected_content, "actual": content}
    conflict_read = await client.read_file(f"{CP4_DIR}/conflict.txt")
    conflict_content = str(conflict_read.get("content", ""))
    digest = hashlib.sha256(
        json.dumps(contents, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    return {
        "checked_path_count": len(expected),
        "sha256": digest,
        "mismatches": mismatches,
        "conflict_file_content": conflict_content,
        "conflict_file_has_single_winner": conflict_content.startswith("conflict-winner-")
        or conflict_content == "base-conflict\n",
        "gate_pass": not mismatches
        and (
            conflict_content.startswith("conflict-winner-")
            or conflict_content == "base-conflict\n"
        ),
    }


def evaluate_cp4(report: dict[str, Any]) -> dict[str, Any]:
    operations = report["load"]["operations"]
    failed_cells = [
        f"{op}/{concurrency}"
        for op, cells in operations.items()
        for concurrency, block in cells.items()
        if not block.get("all_samples_ok")
    ]
    conflict_cells = operations.get("conflict_heavy", {})
    conflict_gate = all(
        block.get("conflict_count", 0) > 0
        for concurrency, block in conflict_cells.items()
        if int(concurrency) > 1
    )
    timing_keys = sorted(
        {
            key
            for cells in operations.values()
            for block in cells.values()
            for sample in block.get("samples", [])
            for key in sample.get("timings_ms", {})
        }
    )
    required_timing_fragments = [
        "layer_stack",
        "mount",
        "tool",
        "capture",
        "occ",
        "cleanup",
        "release",
    ]
    timing_coverage = {
        fragment: any(fragment in key for key in timing_keys) for fragment in required_timing_fragments
    }
    audit_counts = report.get("audit", {}).get("event_type_counts", {})
    timing_coverage["cleanup"] = timing_coverage["cleanup"] or bool(
        audit_counts.get("overlay_workspace.cleanup")
    )
    timing_coverage["release"] = timing_coverage["release"] or bool(
        audit_counts.get("layer_stack.lease_released")
    )
    return {
        "failed_cells": failed_cells,
        "conflict_gate": conflict_gate,
        "final_state_gate": report["final_state"]["gate_pass"],
        "timing_keys": timing_keys,
        "timing_coverage": timing_coverage,
        "gate_pass": not failed_cells
        and conflict_gate
        and report["final_state"]["gate_pass"]
        and all(timing_coverage.values()),
    }


def evaluate_av4(report: dict[str, Any]) -> dict[str, Any]:
    audit = report["audit"]
    return {
        "schema_ok": audit.get("schema") == "sandbox.daemon.audit.pull.v1",
        "drop_free": audit.get("drop_free") is True,
        "buffer_pressure_ok": audit.get("buffer_pressure_ok") is True,
        "required_event_types_present": not audit.get("missing_required_event_types"),
        "artifact_bytes": audit.get("artifact_event_bytes"),
        "gate_pass": audit.get("schema") == "sandbox.daemon.audit.pull.v1"
        and audit.get("drop_free") is True
        and audit.get("buffer_pressure_ok") is True
        and not audit.get("missing_required_event_types"),
    }


def performance_report(report: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema": "phase3t.mixed_non_plugin.performance_report.v1",
        "run_id": report["run_id"],
        "gate_pass": report["gate_pass"],
        "cp4": report["cp4"],
        "av4": report["av4"],
        "artifact": report["artifact"],
        "load": {
            "concurrency_levels": report["load"]["concurrency_levels"],
            "rounds_per_concurrency": report["load"]["rounds_per_concurrency"],
            "operations": {
                op: {
                    concurrency: {
                        "sample_count": block["sample_count"],
                        "success_count": block["success_count"],
                        "host_wall_ms": block["host_wall_ms"],
                        "conflict_count": block["conflict_count"],
                    }
                    for concurrency, block in cells.items()
                }
                for op, cells in report["load"]["operations"].items()
            },
        },
        "audit": {
            key: report["audit"][key]
            for key in (
                "schema",
                "event_count",
                "event_type_counts",
                "missing_required_event_types",
                "last_pull_buffer",
                "snapshot_buffer",
                "drop_free",
                "buffer_pressure_ok",
                "artifact_event_bytes",
            )
        },
        "final_state": report["final_state"],
    }


def performance_markdown(report: dict[str, Any]) -> str:
    lines = [
        "# Phase 3T Mixed Non-Plugin CP-4/AV-4 Report",
        "",
        f"- run_id: `{report['run_id']}`",
        f"- gate_pass: `{report['gate_pass']}`",
        f"- cp4_gate: `{report['cp4']['gate_pass']}`",
        f"- av4_gate: `{report['av4']['gate_pass']}`",
        f"- audit_events: `{report['audit']['event_count']}`",
        f"- audit_drop_free: `{report['audit']['drop_free']}`",
        "",
        "## Operation Cells",
        "",
    ]
    for op, cells in report["load"]["operations"].items():
        for concurrency, block in cells.items():
            lines.append(
                f"- {op} c={concurrency}: {block['success_count']}/{block['sample_count']} "
                f"ok, p95={block['host_wall_ms'].get('p95')} ms, conflicts={block['conflict_count']}"
            )
    lines.extend(
        [
            "",
            "## Audit Event Types",
            "",
            json.dumps(report["audit"]["event_type_counts"], indent=2, sort_keys=True),
            "",
        ]
    )
    return "\n".join(lines)


def daemon_request(op: str, args: dict[str, Any], invocation_id: str) -> str:
    wire_args = {"layer_stack_root": LAYER_STACK_ROOT, "agent_id": AGENT_ID, **args}
    wire_args.setdefault("invocation_id", invocation_id)
    return json.dumps({"op": op, "invocation_id": invocation_id, "args": wire_args}, separators=(",", ":"))


def expect_read(response: dict[str, Any]) -> bool:
    return response.get("success") is True and response.get("exists") is True


def expect_publish(response: dict[str, Any]) -> bool:
    return response.get("success") is True and response.get("status") in {"ok", "committed"}


def expect_publish_or_conflict(response: dict[str, Any]) -> bool:
    return expect_publish(response) or response.get("conflict") not in (None, {})


def summarize_samples_block(samples: list[dict[str, Any]]) -> dict[str, Any]:
    timing_keys = sorted({key for sample in samples for key in sample.get("timings_ms", {})})
    return {
        "sample_count": len(samples),
        "success_count": sum(1 for sample in samples if sample["ok"]),
        "all_samples_ok": bool(samples) and all(sample["ok"] for sample in samples),
        "host_wall_ms": summarize_samples([sample["host_wall_ms"] for sample in samples]),
        "phase_timing_ms": {
            key: summarize_samples(
                [sample["timings_ms"][key] for sample in samples if key in sample["timings_ms"]]
            )
            for key in timing_keys
        },
        "samples": samples,
    }


def timing_ms(response: dict[str, Any]) -> dict[str, float]:
    timings = response.get("timings")
    if not isinstance(timings, dict):
        return {}
    return {
        key: float(value) * 1000.0
        for key, value in timings.items()
        if key.endswith("_s") and isinstance(value, int | float)
    }


def trim_response(response: dict[str, Any]) -> dict[str, Any]:
    keys = (
        "success",
        "status",
        "exit_code",
        "workspace",
        "pty_session_id",
        "changed_paths",
        "conflict",
        "conflict_reason",
        "exists",
        "num_files",
        "error",
    )
    trimmed = {key: response.get(key) for key in keys if key in response}
    output = response.get("output")
    if isinstance(output, dict):
        trimmed["output"] = {
            "stdout": str(output.get("stdout", ""))[-300:],
            "stderr": str(output.get("stderr", ""))[-300:],
        }
    return trimmed


def text_out(response: dict[str, Any]) -> str:
    output = response.get("output")
    if not isinstance(output, dict):
        return ""
    return f"{output.get('stdout', '')}{output.get('stderr', '')}"


def parse_concurrency(raw: str) -> list[int]:
    levels = [int(item.strip()) for item in raw.split(",") if item.strip()]
    return levels or [1]


def parse_operations(raw: str | None) -> list[str]:
    available = operation_builders()
    if raw is None or not raw.strip():
        return list(available)
    selected = [item.strip() for item in raw.split(",") if item.strip()]
    unknown = sorted(set(selected) - set(available))
    if unknown:
        names = ", ".join(unknown)
        allowed = ", ".join(sorted(available))
        raise SystemExit(f"unknown --operations entries: {names}; allowed: {allowed}")
    return selected


if __name__ == "__main__":
    raise SystemExit(main())
