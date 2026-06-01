#!/usr/bin/env python3
"""Live Phase 2 Rust daemon closeout for CP-3 and AV-2.

This is intentionally narrower than the Phase 3 publish/overlay harness. It
uploads a locally packaged ``eosd`` into a Docker sandbox, seeds a minimal
LayerStack read fixture, starts ``EOS_SANDBOX_RUNTIME=rust``, and proves:

* CP-3: daemon cold-start and idle RSS beat the checked-in CP-0 Python baseline.
* AV-2: stale TCP transport failure invalidates the cached endpoint, respawns
  the daemon, and the read path works after recovery.
* Phase 2 read surface: ``api.runtime.ready``, ``api.v1.heartbeat``,
  ``api.layer_metrics``, ``api.read_file``, and ``api.v1.read_file`` work over
  the real AF_UNIX/TCP transports.
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import io
import json
import os
import platform
import shlex
import sys
import tarfile
import time
import uuid
from collections.abc import Awaitable, Callable, Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
BACKEND_SRC = ROOT / "backend" / "src"
SCRIPT_DIR = Path(__file__).resolve().parent
if str(BACKEND_SRC) not in sys.path:
    sys.path.insert(0, str(BACKEND_SRC))
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from bench_sandbox_e2e import (  # noqa: E402
    DEFAULT_DOCKER_IMAGE,
    DockerBench,
    _combined_output,
    _exit_code,
    _text,
    collect_environment,
    elapsed_ms,
    summarize_samples,
    tar_file_at_path,
)

RUNTIME_ROOT = "/tmp/eos-sandbox-runtime"
EOSD_REMOTE_PATH = f"{RUNTIME_ROOT}/eosd"
LAYER_STACK_ROOT = "/eos-mount-scratch/eos-sandbox-runtime/layer-stack"
WORKSPACE_ROOT = "/testbed"
SOCKET_PATH = f"{RUNTIME_ROOT}/runtime.sock"
PID_PATH = f"{RUNTIME_ROOT}/runtime.pid"
README_CONTENT = "# README\nPhase 2 rust daemon read path\n"


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    report = asyncio.run(run_phase2(args))
    out = Path(args.report)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2, sort_keys=True))
    print(
        f"wrote {out} "
        f"(cp3={report['cp3']['gate_pass']} av2={report['av2']['gate_pass']} "
        f"all={report['gate_pass']} run_id={report['run_id']})"
    )
    return 0 if report["gate_pass"] else 1


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--docker-image",
        default=DEFAULT_DOCKER_IMAGE,
        help=f"Docker image for the live run (default: {DEFAULT_DOCKER_IMAGE}).",
    )
    parser.add_argument(
        "--container-id",
        default=None,
        help="Use an existing Docker container instead of creating one.",
    )
    parser.add_argument(
        "--artifact",
        type=Path,
        default=ROOT / "sandbox" / "dist" / "eosd-linux-amd64",
        help="Locally packaged amd64 eosd binary.",
    )
    parser.add_argument(
        "--baseline",
        type=Path,
        default=ROOT / "bench" / "baseline-amd64.json",
        help="CP-0 baseline report to compare CP-3 against.",
    )
    parser.add_argument(
        "--report",
        default=str(ROOT / "bench" / "phase2-rust-daemon-amd64.json"),
        help="JSON report path.",
    )
    parser.add_argument(
        "--ready-samples",
        type=int,
        default=5,
        help="Number of warm TCP heartbeat/readiness samples after daemon start.",
    )
    parser.add_argument(
        "--keep-container",
        action="store_true",
        help="Do not delete a container created by this script.",
    )
    parser.add_argument(
        "--name-prefix",
        default="eos-phase2-rust-daemon",
        help="Name prefix for created containers.",
    )
    return parser.parse_args(argv)


async def run_phase2(args: argparse.Namespace) -> dict[str, Any]:
    if not args.artifact.exists():
        raise SystemExit(f"missing eosd artifact: {args.artifact}")
    baseline = json.loads(args.baseline.read_text())
    bench = await DockerBench.create(
        image=args.docker_image,
        container_id=args.container_id,
        name_prefix=args.name_prefix,
    )
    try:
        report: dict[str, Any] = {
            "mode": "docker-phase2-rust-daemon",
            "run_id": os.environ.get("EOS_TIER_RUN_ID") or f"local-{uuid.uuid4().hex[:12]}",
            "sandbox_id": bench.sandbox_id,
            "created_container": bench.created,
            "host": {
                "platform": platform.platform(),
                "python": sys.version.split()[0],
            },
            "environment": await collect_environment(bench),
            "baseline_path": str(args.baseline),
        }
        await reset_runtime(bench)
        report["artifact"] = await upload_artifact(bench, args.artifact)
        await seed_layer_stack(bench)

        with temporary_env("EOS_SANDBOX_RUNTIME", "rust"):
            from sandbox.host import daemon_client

            daemon_client.invalidate_daemon_tcp_endpoint(bench.sandbox_id)
            report["cp3"] = await measure_cp3(
                bench,
                baseline=baseline,
                daemon_client=daemon_client,
            )
            report["transports"] = await prove_phase2_transports(
                bench,
                daemon_client=daemon_client,
                ready_samples=args.ready_samples,
            )
            report["av2"] = await prove_respawn_and_cache_invalidation(
                bench,
                daemon_client=daemon_client,
            )

        report["gate_pass"] = bool(
            report["artifact"]["gate_pass"]
            and report["cp3"]["gate_pass"]
            and report["transports"]["gate_pass"]
            and report["av2"]["gate_pass"]
        )
        return report
    finally:
        await bench.close(keep=args.keep_container)


@contextmanager
def temporary_env(key: str, value: str) -> Iterator[None]:
    previous = os.environ.get(key)
    os.environ[key] = value
    try:
        yield
    finally:
        if previous is None:
            os.environ.pop(key, None)
        else:
            os.environ[key] = previous


async def upload_artifact(bench: DockerBench, artifact: Path) -> dict[str, Any]:
    payload = artifact.read_bytes()
    expected_sha = hashlib.sha256(payload).hexdigest()
    started = time.perf_counter()
    await bench.adapter.put_archive(
        bench.sandbox_id,
        tar_stream=tar_file_at_path(EOSD_REMOTE_PATH, payload, mode=0o755),
        dest_dir="/",
    )
    upload_ms = elapsed_ms(started)
    remote_bytes, remote_mode = await bench.get_file_archive(EOSD_REMOTE_PATH, timeout=60)
    remote_sha = hashlib.sha256(remote_bytes).hexdigest()
    version = await bench.direct_exec([EOSD_REMOTE_PATH, "--version"], timeout=30)
    return {
        "source_path": str(artifact),
        "remote_path": EOSD_REMOTE_PATH,
        "size_bytes": len(payload),
        "upload_time_ms": upload_ms,
        "local_sha256": expected_sha,
        "remote_sha256": remote_sha,
        "hashes_match": remote_sha == expected_sha,
        "remote_mode": oct(remote_mode),
        "executable": bool(remote_mode & 0o111),
        "version": result_block(version),
        "gate_pass": (
            remote_sha == expected_sha
            and bool(remote_mode & 0o111)
            and _exit_code(version) == 0
        ),
    }


async def seed_layer_stack(bench: DockerBench) -> None:
    await bench.adapter.put_archive(
        bench.sandbox_id,
        tar_stream=seed_archive(),
        dest_dir="/",
    )


def seed_archive() -> bytes:
    manifest = {
        "schema_version": 1,
        "version": 1,
        "layers": [{"layer_id": "B000001-base", "path": "layers/B000001-base"}],
    }
    binding = {
        "workspace_root": WORKSPACE_ROOT,
        "layer_stack_root": LAYER_STACK_ROOT,
        "active_manifest_version": 1,
        "active_root_hash": "phase2-active-root",
        "base_manifest_version": 1,
        "base_root_hash": "phase2-base-root",
    }
    files = {
        f"{LAYER_STACK_ROOT}/manifest.json": json.dumps(
            manifest, indent=2, sort_keys=True
        ).encode(),
        f"{LAYER_STACK_ROOT}/workspace.json": json.dumps(
            binding, indent=2, sort_keys=True
        ).encode(),
        f"{LAYER_STACK_ROOT}/layers/B000001-base/README.md": README_CONTENT.encode(),
    }
    dirs = {
        RUNTIME_ROOT,
        LAYER_STACK_ROOT,
        f"{LAYER_STACK_ROOT}/layers",
        f"{LAYER_STACK_ROOT}/layers/B000001-base",
        f"{LAYER_STACK_ROOT}/staging",
        WORKSPACE_ROOT,
    }
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as tar:
        added_dirs: set[str] = set()
        for directory in sorted(dirs | parent_dirs(files)):
            add_dir(tar, directory, added_dirs)
        for path, payload in files.items():
            add_file(tar, path, payload)
    return raw.getvalue()


def parent_dirs(files: dict[str, bytes]) -> set[str]:
    out: set[str] = set()
    for path in files:
        current = Path(path.lstrip("/")).parent
        while str(current) not in {"", "."}:
            out.add(f"/{current.as_posix()}")
            current = current.parent
    return out


def add_dir(tar: tarfile.TarFile, path: str, added: set[str]) -> None:
    rel = path.strip("/")
    if not rel or rel in added:
        return
    info = tarfile.TarInfo(rel)
    info.type = tarfile.DIRTYPE
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mode = 0o755
    tar.addfile(info)
    added.add(rel)


def add_file(tar: tarfile.TarFile, path: str, payload: bytes) -> None:
    rel = path.strip("/")
    info = tarfile.TarInfo(rel)
    info.size = len(payload)
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mode = 0o644
    tar.addfile(info, io.BytesIO(payload))


async def reset_runtime(bench: DockerBench) -> None:
    command = f"""
set -eu
if [ -f {shlex.quote(PID_PATH)} ]; then
  kill "$(cat {shlex.quote(PID_PATH)})" 2>/dev/null || true
fi
rm -f {shlex.quote(PID_PATH)} {shlex.quote(SOCKET_PATH)} {shlex.quote(RUNTIME_ROOT)}/runtime.env {shlex.quote(RUNTIME_ROOT)}/runtime.log
rm -rf {shlex.quote(LAYER_STACK_ROOT)}
"""
    result = await bench.exec(command, timeout=15)
    require_success(result, "reset rust runtime")


async def measure_cp3(
    bench: DockerBench,
    *,
    baseline: dict[str, Any],
    daemon_client: Any,
) -> dict[str, Any]:
    cp0 = baseline["cp0"]
    started = time.perf_counter()
    await daemon_client.ensure_daemon_current(bench.sandbox_id)
    spawn_ms = elapsed_ms(started)
    await wait_for(
        lambda: pid_exists(bench),
        timeout_s=5,
        message="wait for rust daemon pid file",
    )
    ready_started = time.perf_counter()
    ready = await daemon_client.call_daemon_api(
        bench.sandbox_id,
        "api.runtime.ready",
        {},
        layer_stack_root=LAYER_STACK_ROOT,
        timeout=30,
    )
    ready_ms = elapsed_ms(ready_started)
    rss_kb = await read_daemon_rss_kb(bench)
    baseline_cold_ms = float(cp0["daemon_cold_start_ms"])
    baseline_rss_kb = int(cp0["daemon_idle_rss_kb"])
    return {
        "baseline": {
            "daemon_cold_start_ms": baseline_cold_ms,
            "daemon_idle_rss_kb": baseline_rss_kb,
        },
        "rust_daemon_spawn_ms": spawn_ms,
        "rust_runtime_ready_ms": ready_ms,
        "rust_daemon_idle_rss_kb": rss_kb,
        "cold_start_no_slower_than_cp0": spawn_ms <= baseline_cold_ms,
        "rss_no_more_than_half_cp0": rss_kb is not None and rss_kb <= baseline_rss_kb / 2,
        "ready": ready,
        "gate_pass": bool(
            ready.get("ready") is True
            and spawn_ms <= baseline_cold_ms
            and rss_kb is not None
            and rss_kb <= baseline_rss_kb / 2
        ),
    }


async def prove_phase2_transports(
    bench: DockerBench,
    *,
    daemon_client: Any,
    ready_samples: int,
) -> dict[str, Any]:
    endpoint = await daemon_client._resolve_daemon_tcp_endpoint(  # noqa: SLF001
        bench.adapter,
        bench.sandbox_id,
    )
    if endpoint is None:
        raise RuntimeError("Docker sandbox did not expose a daemon TCP endpoint")

    unix_ready = await call_unix(bench, request("api.runtime.ready", {}))
    unix_read_alias = await call_unix(bench, request("api.read_file", {"path": "README.md"}))
    tcp_ready = await call_tcp(daemon_client, endpoint, request("api.runtime.ready", {}))
    tcp_read = await call_tcp(
        daemon_client,
        endpoint,
        request("api.v1.read_file", {"path": f"{WORKSPACE_ROOT}/README.md"}),
    )
    tcp_heartbeat = await call_tcp(
        daemon_client,
        endpoint,
        request("api.v1.heartbeat", {"invocation_ids": ["phase2-live"]}),
    )
    tcp_metrics = await call_tcp(daemon_client, endpoint, request("api.layer_metrics", {}))

    warm_ready_ms: list[float] = []
    for _ in range(max(0, ready_samples)):
        started = time.perf_counter()
        sample = await call_tcp(
            daemon_client,
            endpoint,
            request("api.v1.heartbeat", {"invocation_ids": []}),
        )
        if sample.get("success") is not True:
            raise RuntimeError(f"warm heartbeat failed: {sample!r}")
        warm_ready_ms.append(elapsed_ms(started))

    gate_pass = all(
        [
            unix_ready.get("ready") is True,
            unix_read_alias.get("content") == README_CONTENT,
            tcp_ready.get("ready") is True,
            tcp_read.get("content") == README_CONTENT,
            tcp_heartbeat.get("touched") == 1,
            tcp_metrics.get("manifest_depth") == 1,
        ]
    )
    return {
        "endpoint": {
            "host": endpoint.host,
            "port": endpoint.port,
            "internal_port": endpoint.internal_port,
            "auth_token_present": bool(endpoint.auth_token),
        },
        "unix_ready": unix_ready,
        "unix_read_alias": trim_read_response(unix_read_alias),
        "tcp_ready": tcp_ready,
        "tcp_read": trim_read_response(tcp_read),
        "tcp_heartbeat": tcp_heartbeat,
        "tcp_layer_metrics": tcp_metrics,
        "warm_heartbeat_ms": summarize_samples(warm_ready_ms),
        "gate_pass": gate_pass,
    }


async def prove_respawn_and_cache_invalidation(
    bench: DockerBench,
    *,
    daemon_client: Any,
) -> dict[str, Any]:
    daemon_client.invalidate_daemon_tcp_endpoint(bench.sandbox_id)
    endpoint = await daemon_client._resolve_daemon_tcp_endpoint(  # noqa: SLF001
        bench.adapter,
        bench.sandbox_id,
    )
    if endpoint is None:
        raise RuntimeError("Docker sandbox did not expose a daemon TCP endpoint")
    cache_was_populated = bench.sandbox_id in daemon_client._tcp_endpoint_cache  # noqa: SLF001
    old_pid = await read_pid(bench)
    await kill_daemon(bench)
    stale_tcp_probe = await daemon_client._call_tcp_daemon(  # noqa: SLF001
        endpoint,
        request("api.v1.heartbeat", {"invocation_ids": []}),
        timeout=5,
    )
    stale_tcp_failed = _exit_code(stale_tcp_probe) == 97 or (
        _exit_code(stale_tcp_probe) == 98
        and _text(stale_tcp_probe, "stderr") == "EOS_DAEMON_IO_FAILED:empty_response"
    )
    read_after_respawn = await daemon_client.call_daemon_api(
        bench.sandbox_id,
        "api.v1.read_file",
        {"path": f"{WORKSPACE_ROOT}/README.md"},
        layer_stack_root=LAYER_STACK_ROOT,
        timeout=30,
    )
    cache_invalidated = bench.sandbox_id not in daemon_client._tcp_endpoint_cache  # noqa: SLF001
    second_read = await daemon_client.call_daemon_api(
        bench.sandbox_id,
        "api.v1.read_file",
        {"path": "README.md"},
        layer_stack_root=LAYER_STACK_ROOT,
        timeout=30,
    )
    cache_repopulated = bench.sandbox_id in daemon_client._tcp_endpoint_cache  # noqa: SLF001
    new_pid = await read_pid(bench)
    processes = await eosd_processes(bench)
    mount_entries = await mount_entries_for_runtime(bench)

    gate_pass = all(
        [
            cache_was_populated,
            stale_tcp_failed,
            read_after_respawn.get("content") == README_CONTENT,
            cache_invalidated,
            second_read.get("content") == README_CONTENT,
            old_pid is not None,
            new_pid is not None,
            old_pid != new_pid,
            processes["daemon_count"] == 1,
            mount_entries["count"] == 0,
        ]
    )
    return {
        "cache_was_populated": cache_was_populated,
        "stale_tcp_transport_failure": stale_tcp_failed,
        "stale_tcp_probe": result_block(stale_tcp_probe),
        "cache_invalidated_after_stale_tcp_failure": cache_invalidated,
        "cache_repopulated_on_next_call": cache_repopulated,
        "old_pid": old_pid,
        "new_pid": new_pid,
        "read_after_respawn": trim_read_response(read_after_respawn),
        "second_read_after_cache_repopulate": trim_read_response(second_read),
        "processes": processes,
        "runtime_mount_entries": mount_entries,
        "gate_pass": gate_pass,
    }


def request(op: str, args: dict[str, Any]) -> str:
    return json.dumps(
        {
            "op": op,
            "invocation_id": f"phase2-{uuid.uuid4().hex}",
            "args": {"layer_stack_root": LAYER_STACK_ROOT, **args},
        },
        separators=(",", ":"),
    )


async def call_unix(bench: DockerBench, payload: str) -> dict[str, Any]:
    result = await retry_result(
        lambda: bench.exec(
            " ".join(
                shlex.quote(part)
                for part in (
                    EOSD_REMOTE_PATH,
                    "daemon",
                    "--client",
                    SOCKET_PATH,
                    payload,
                )
            ),
            timeout=15,
        ),
        ok=lambda result: _exit_code(result) == 0,
        attempts=8,
        delay_s=0.1,
    )
    require_success(result, "AF_UNIX daemon client")
    return decode_stdout(result)


async def call_tcp(daemon_client: Any, endpoint: Any, payload: str) -> dict[str, Any]:
    result = await daemon_client._call_tcp_daemon(endpoint, payload, timeout=15)  # noqa: SLF001
    require_success(result, "TCP daemon client")
    return decode_stdout(result)


async def wait_for(
    probe: Callable[[], Awaitable[bool]],
    *,
    timeout_s: float,
    message: str,
) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if await probe():
            return
        await asyncio.sleep(0.05)
    raise TimeoutError(message)


async def retry_result(
    call: Callable[[], Awaitable[Any]],
    *,
    ok: Callable[[Any], bool],
    attempts: int,
    delay_s: float,
) -> Any:
    last: Any = None
    for _ in range(attempts):
        last = await call()
        if ok(last):
            return last
        await asyncio.sleep(delay_s)
    return last


async def pid_exists(bench: DockerBench) -> bool:
    return await read_pid(bench) is not None


async def read_pid(bench: DockerBench) -> int | None:
    result = await bench.exec(
        f"test -f {shlex.quote(PID_PATH)} && cat {shlex.quote(PID_PATH)}",
        timeout=5,
    )
    if _exit_code(result) != 0:
        return None
    try:
        return int(_text(result, "stdout").strip())
    except ValueError:
        return None


async def read_daemon_rss_kb(bench: DockerBench) -> int | None:
    result = await bench.exec(
        f"pid=$(cat {shlex.quote(PID_PATH)}); ps -o rss= -p \"$pid\"",
        timeout=15,
    )
    if _exit_code(result) != 0:
        return None
    try:
        return int(_text(result, "stdout").strip())
    except ValueError:
        return None


async def kill_daemon(bench: DockerBench) -> None:
    result = await bench.exec(
        f"pid=$(cat {shlex.quote(PID_PATH)}); kill -9 \"$pid\"; sleep 0.1",
        timeout=10,
    )
    require_success(result, "kill rust daemon")


async def eosd_processes(bench: DockerBench) -> dict[str, Any]:
    result = await bench.exec(
        "ps -eo pid=,args= | awk '/eosd daemon/ && !/awk/ {print}'",
        timeout=10,
    )
    text = _text(result, "stdout").strip()
    lines = [redact_auth_token(line) for line in text.splitlines() if line.strip()]
    return {"daemon_count": len(lines), "lines": lines}


def redact_auth_token(line: str) -> str:
    parts = line.split()
    for index, part in enumerate(parts[:-1]):
        if part == "--auth-token":
            parts[index + 1] = "<redacted>"
    return " ".join(parts)


async def mount_entries_for_runtime(bench: DockerBench) -> dict[str, Any]:
    result = await bench.exec(
        "grep -F 'eos-sandbox-runtime' /proc/self/mountinfo || true",
        timeout=10,
    )
    text = _text(result, "stdout").strip()
    lines = [line for line in text.splitlines() if line.strip()]
    return {"count": len(lines), "lines": lines}


def decode_stdout(result: Any) -> dict[str, Any]:
    text = _text(result, "stdout").strip()
    try:
        decoded = json.loads(text)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"daemon returned invalid JSON: {text!r}") from exc
    if not isinstance(decoded, dict):
        raise RuntimeError(f"daemon returned non-object JSON: {decoded!r}")
    error = decoded.get("error")
    if error is not None:
        raise RuntimeError(f"daemon returned error: {error!r}")
    return decoded


def trim_read_response(response: dict[str, Any]) -> dict[str, Any]:
    return {
        "success": response.get("success"),
        "exists": response.get("exists"),
        "content_sha256": hashlib.sha256(str(response.get("content", "")).encode()).hexdigest(),
        "content": response.get("content"),
        "workspace": response.get("workspace"),
    }


def result_block(result: Any) -> dict[str, Any]:
    return {
        "exit_code": _exit_code(result),
        "stdout": _text(result, "stdout").strip(),
        "stderr": _text(result, "stderr").strip(),
    }


def require_success(result: Any, message: str) -> None:
    if _exit_code(result) != 0:
        raise RuntimeError(f"{message}: {_combined_output(result)}")


if __name__ == "__main__":
    raise SystemExit(main())
