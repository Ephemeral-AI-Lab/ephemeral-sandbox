#!/usr/bin/env python3
"""Live Phase 3 Rust daemon benchmark for CP-4s command/search hot paths.

The harness uploads a locally packaged ``eosd`` into the Docker sandbox, seeds a
LayerStack fixture from the image's real workspace, starts the Rust daemon, then
measures:

* ``api.v1.exec_command`` no-op latency for the current non-login Bash string form.
* ``api.v1.exec_command`` small-write publish latency.
* ``api.v1.glob`` and ``api.v1.grep`` read-only overlay search latency.
* 1/3/5/10 concurrent shell-string ``api.v1.exec_command`` load for
  no-op and unique write commands.
* daemon memory before load, between operation groups, and after drain.

It intentionally records gate failures instead of smoothing them over. CP-4s is
only closed when the generated report passes the thresholds encoded here and is
captured in the target Linux/Docker image.
"""

from __future__ import annotations

import argparse
import asyncio
import copy
import hashlib
import io
import json
import os
import platform
import sys
import tarfile
import time
import uuid
from collections.abc import Callable
from pathlib import Path, PurePosixPath
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
    RUNTIME_ROOT,
    WORKSPACE_ROOT,
    call_tcp,
    read_pid,
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

AGENT_ID = "phase3-cp4s-bench"
BASE_LAYER_ID = "B000001-base"
ROSETTA_ACTIVE_MEMORY_HEADROOM_KB = 2048
TARGETS: dict[str, dict[str, Any]] = {
    "amd64": {
        "platform": "linux/amd64",
        "image": DEFAULT_DOCKER_IMAGE,
        "artifact": ROOT / "sandbox" / "dist" / "eosd-linux-amd64",
        "phase1_baseline": ROOT / "bench" / "phase1-ns-runner-amd64.json",
        "phase0_baseline": ROOT / "bench" / "baseline-amd64.json",
        "report": ROOT / "bench" / "phase3-rust-daemon-amd64.json",
        "container_arch": "amd64",
    },
    "arm64": {
        "platform": "linux/arm64",
        "image": None,
        "artifact": ROOT / "sandbox" / "dist" / "eosd-linux-arm64",
        "phase1_baseline": ROOT / "bench" / "phase1-ns-runner-arm64.json",
        "phase0_baseline": ROOT / "bench" / "baseline-arm64.json",
        "report": ROOT / "bench" / "phase3-rust-daemon-arm64.json",
        "container_arch": "arm64",
    },
}


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    report = asyncio.run(run_phase3(args))
    out = Path(args.report)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2, sort_keys=True))
    print(
        f"wrote {out} "
        f"(cp4s={report['cp4s']['gate_pass']} all={report['gate_pass']} "
        f"shell_noop={report['cp4s']['gates'].get('shell_noop_70pct_faster_than_phase1')} "
        f"run_id={report['run_id']})"
    )
    return 0 if report["gate_pass"] else 1


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--arch",
        choices=sorted(TARGETS),
        default="amd64",
        help="Target artifact/baseline architecture.",
    )
    parser.add_argument(
        "--docker-platform",
        default=None,
        help="Docker platform to request, e.g. linux/amd64 or linux/arm64.",
    )
    parser.add_argument(
        "--docker-image",
        default=None,
        help="Docker image for the live run. Defaults by --arch when available.",
    )
    parser.add_argument(
        "--container-id",
        default=None,
        help="Use an existing Docker container instead of creating one.",
    )
    parser.add_argument(
        "--artifact",
        type=Path,
        default=None,
        help="Locally packaged eosd binary. Defaults by --arch.",
    )
    parser.add_argument(
        "--phase1-baseline",
        type=Path,
        default=None,
        help="Phase 1 direct-runner report used for shell latency thresholds.",
    )
    parser.add_argument(
        "--phase0-baseline",
        type=Path,
        default=None,
        help="Phase 0 baseline report used for daemon memory fallback thresholds.",
    )
    parser.add_argument(
        "--report",
        default=None,
        help="JSON report path.",
    )
    parser.add_argument(
        "--samples",
        type=int,
        default=10,
        help="Samples per operation group.",
    )
    parser.add_argument(
        "--drain-seconds",
        type=float,
        default=0.5,
        help="Seconds to wait before the final idle memory sample.",
    )
    parser.add_argument(
        "--load-concurrency",
        default="1,3,5,10",
        help="Comma-separated concurrent command counts for the shell-string load matrix.",
    )
    parser.add_argument(
        "--load-rounds",
        type=int,
        default=10,
        help="Number of concurrent waves to run per load concurrency level.",
    )
    parser.add_argument(
        "--skip-load",
        action="store_true",
        help="Skip the concurrent shell-string load matrix.",
    )
    parser.add_argument(
        "--keep-container",
        action="store_true",
        help="Do not delete a container created by this script.",
    )
    parser.add_argument(
        "--name-prefix",
        default="eos-phase3-rust-daemon",
        help="Name prefix for created containers.",
    )
    args = parser.parse_args(argv)
    target = TARGETS[args.arch]
    args.docker_platform = args.docker_platform or target["platform"]
    if args.docker_image is None:
        args.docker_image = target["image"]
    if args.docker_image is None and args.container_id is None:
        parser.error(
            f"--docker-image is required for --arch {args.arch}; no default "
            "Phase 3 workspace image is checked in for that target"
        )
    args.artifact = args.artifact or target["artifact"]
    args.phase1_baseline = args.phase1_baseline or target["phase1_baseline"]
    args.phase0_baseline = args.phase0_baseline or target["phase0_baseline"]
    args.report = str(args.report or target["report"])
    return args


def validate_target_inputs(args: argparse.Namespace) -> dict[str, Any] | None:
    if not args.artifact.exists():
        raise SystemExit(f"missing eosd artifact for --arch {args.arch}: {args.artifact}")
    for label, path in [
        ("Phase 1 baseline", args.phase1_baseline),
        ("Phase 0 baseline", args.phase0_baseline),
    ]:
        if not path.exists():
            raise SystemExit(f"missing {label} for --arch {args.arch}: {path}")
    metadata_path = args.artifact.with_suffix(".json")
    if not metadata_path.exists():
        return None
    metadata = json.loads(metadata_path.read_text())
    metadata_arch = metadata.get("arch")
    if metadata_arch != args.arch:
        raise SystemExit(
            f"artifact metadata arch {metadata_arch!r} does not match --arch "
            f"{args.arch!r}: {metadata_path}"
        )
    return metadata


def validate_baseline_architecture(path: Path, report: dict[str, Any], arch: str) -> None:
    environment_arch = normalize_container_arch(
        report.get("environment", {}).get("architecture", {}).get("stdout")
    )
    if environment_arch is None:
        raise SystemExit(f"baseline lacks environment architecture: {path}")
    if environment_arch != arch:
        raise SystemExit(
            f"baseline architecture {environment_arch!r} does not match --arch "
            f"{arch!r}: {path}"
        )


def normalize_container_arch(raw: object) -> str | None:
    value = str(raw or "").strip().lower()
    if value in {"x86_64", "amd64"}:
        return "amd64"
    if value in {"aarch64", "arm64"}:
        return "arm64"
    return None


async def run_phase3(args: argparse.Namespace) -> dict[str, Any]:
    target = TARGETS[args.arch]
    artifact_metadata = validate_target_inputs(args)
    phase1 = json.loads(args.phase1_baseline.read_text())
    phase0 = json.loads(args.phase0_baseline.read_text())
    validate_baseline_architecture(args.phase1_baseline, phase1, args.arch)
    validate_baseline_architecture(args.phase0_baseline, phase0, args.arch)
    bench = await DockerBench.create(
        image=args.docker_image,
        container_id=args.container_id,
        name_prefix=args.name_prefix,
        platform=args.docker_platform,
    )
    try:
        environment = await collect_environment(bench)
        environment_arch = normalize_container_arch(
            environment.get("architecture", {}).get("stdout")
        )
        if environment_arch != args.arch:
            raise RuntimeError(
                f"container architecture {environment_arch!r} from uname -m does "
                f"not match --arch {args.arch!r} / platform {args.docker_platform!r}"
            )
        report: dict[str, Any] = {
            "mode": "docker-phase3-rust-daemon-cp4s",
            "run_id": os.environ.get("EOS_TIER_RUN_ID")
            or f"local-{uuid.uuid4().hex[:12]}",
            "target": {
                "arch": args.arch,
                "docker_platform": args.docker_platform,
                "docker_image": args.docker_image,
                "expected_container_arch": target["container_arch"],
                "artifact": str(args.artifact),
                "artifact_metadata": artifact_metadata,
            },
            "sandbox_id": bench.sandbox_id,
            "created_container": bench.created,
            "host": {
                "platform": platform.platform(),
                "python": sys.version.split()[0],
            },
            "environment": environment,
            "baseline_paths": {
                "phase1": str(args.phase1_baseline),
                "phase0": str(args.phase0_baseline),
            },
            "samples_per_operation": args.samples,
            "load": {
                "concurrency_levels": parse_concurrency_levels(args.load_concurrency),
                "rounds_per_concurrency": max(0, args.load_rounds),
                "skipped": bool(args.skip_load),
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

            memory_samples = [await sample_daemon_memory(bench, "idle_before_load")]
            operations: dict[str, Any] = {}
            operations["noop_shell"] = await measure_operation(
                daemon_client,
                endpoint,
                "noop_shell",
                args.samples,
                lambda _index, invocation_id: (
                    "api.v1.exec_command",
                    {
                        "cmd": "true",
                        "cwd": ".",
                        "timeout_seconds": 10,
                        "invocation_id": invocation_id,
                    },
                ),
                expect_noop_shell,
            )
            memory_samples.append(await sample_daemon_memory(bench, "after_noop_shell"))

            operations["small_write"] = await measure_operation(
                daemon_client,
                endpoint,
                "small_write",
                args.samples,
                small_write_request,
                expect_small_write,
            )
            memory_samples.append(await sample_daemon_memory(bench, "after_small_write"))

            operations["glob"] = await measure_operation(
                daemon_client,
                endpoint,
                "glob",
                args.samples,
                lambda _index, invocation_id: (
                    "api.v1.glob",
                    {
                        "pattern": "phase3-small-*.txt",
                        "path": ".",
                        "invocation_id": invocation_id,
                    },
                ),
                lambda response: expect_search_count(response, args.samples),
            )
            memory_samples.append(await sample_daemon_memory(bench, "after_glob"))

            operations["grep"] = await measure_operation(
                daemon_client,
                endpoint,
                "grep",
                args.samples,
                lambda _index, invocation_id: (
                    "api.v1.grep",
                    {
                        "pattern": "README",
                        "path": ".",
                        "output_mode": "content",
                        "offset": 0,
                        "case_insensitive": False,
                        "line_numbers": True,
                        "multiline": False,
                        "invocation_id": invocation_id,
                    },
                ),
                lambda response: expect_search_count(response, 1),
            )
            memory_samples.append(await sample_daemon_memory(bench, "after_grep"))
            if args.skip_load:
                report["load"]["gate_pass"] = True
                report["load"]["operations"] = {}
                report["load"]["evaluation"] = {"skipped": True, "gate_pass": True}
            else:
                report["load"] = await measure_load_matrix(
                    daemon_client,
                    endpoint,
                    concurrencies=parse_concurrency_levels(args.load_concurrency),
                    rounds=max(0, args.load_rounds),
                )
                report["load"]["evaluation"] = evaluate_load_matrix(report["load"], phase1)
                report["load"]["gate_pass"] = bool(
                    report["load"]["evaluation"]["gate_pass"]
                )
            memory_samples.append(await sample_daemon_memory(bench, "after_load_matrix"))
            await asyncio.sleep(max(0.0, float(args.drain_seconds)))
            memory_samples.append(await sample_daemon_memory(bench, "idle_after_drain"))

            report["operations"] = operations
            report["memory"] = summarize_memory(memory_samples, phase0)
            report["final_state"] = await collect_final_state(
                daemon_client,
                endpoint,
                args.samples,
            )

        report["cp4s"] = evaluate_cp4s(report, phase1)
        report["gate_pass"] = bool(
            report["artifact"]["gate_pass"]
            and report["ready"].get("ready") is True
            and report["final_state"]["gate_pass"]
            and report["cp4s"]["gate_pass"]
            and report["load"]["gate_pass"]
        )
        return report
    finally:
        await bench.close(keep=args.keep_container)


async def seed_layer_stack_from_workspace(bench: DockerBench) -> dict[str, Any]:
    workspace_archive = await read_workspace_archive(bench)
    tar_stream, seed = layer_stack_archive_from_workspace(workspace_archive)
    await bench.adapter.put_archive(
        bench.sandbox_id,
        tar_stream=tar_stream,
        dest_dir="/",
    )
    return seed


async def read_workspace_archive(bench: DockerBench) -> bytes:
    def _run() -> bytes:
        client = bench.adapter._get_client()  # Docker-only benchmark helper.
        container = client.containers.get(bench.sandbox_id)
        chunks, _stat = container.get_archive(WORKSPACE_ROOT)
        return b"".join(chunks)

    return await asyncio.to_thread(_run)


def layer_stack_archive_from_workspace(workspace_archive: bytes) -> tuple[bytes, dict[str, Any]]:
    manifest = {
        "schema_version": 1,
        "version": 1,
        "layers": [
            {
                "layer_id": BASE_LAYER_ID,
                "path": f"layers/{BASE_LAYER_ID}",
            }
        ],
    }
    binding = {
        "workspace_root": WORKSPACE_ROOT,
        "layer_stack_root": LAYER_STACK_ROOT,
        "active_manifest_version": 1,
        "active_root_hash": "phase3-active-root",
        "base_manifest_version": 1,
        "base_root_hash": "phase3-base-root",
    }
    base_prefix = f"{LAYER_STACK_ROOT.strip('/')}/layers/{BASE_LAYER_ID}"
    raw = io.BytesIO()
    stats: dict[str, Any] = {
        "source_workspace": WORKSPACE_ROOT,
        "base_layer_id": BASE_LAYER_ID,
        "file_count": 0,
        "dir_count": 0,
        "symlink_count": 0,
        "other_count": 0,
        "file_bytes": 0,
        "source_archive_bytes": len(workspace_archive),
    }
    added_dirs: set[str] = set()

    with tarfile.open(fileobj=raw, mode="w") as out:
        for directory in (
            RUNTIME_ROOT,
            LAYER_STACK_ROOT,
            f"{LAYER_STACK_ROOT}/layers",
            f"{LAYER_STACK_ROOT}/layers/{BASE_LAYER_ID}",
            f"{LAYER_STACK_ROOT}/staging",
            WORKSPACE_ROOT,
        ):
            add_tar_dir(out, directory, added_dirs)
        add_tar_file(
            out,
            f"{LAYER_STACK_ROOT}/manifest.json",
            json.dumps(manifest, indent=2, sort_keys=True).encode(),
            added_dirs,
        )
        add_tar_file(
            out,
            f"{LAYER_STACK_ROOT}/workspace.json",
            json.dumps(binding, indent=2, sort_keys=True).encode(),
            added_dirs,
        )

        with tarfile.open(fileobj=io.BytesIO(workspace_archive), mode="r:*") as source:
            for member in source:
                rel = workspace_member_relpath(member.name)
                if rel is None or rel == "":
                    continue
                target = f"{base_prefix}/{rel}"
                add_parent_tar_dirs(out, target, added_dirs)
                copied = copy_tar_info(member, target)
                if copied.isdir():
                    add_tar_dir(out, f"/{target}", added_dirs)
                    stats["dir_count"] += 1
                elif copied.isfile():
                    extracted = source.extractfile(member)
                    if extracted is None:
                        stats["other_count"] += 1
                        continue
                    out.addfile(copied, extracted)
                    stats["file_count"] += 1
                    stats["file_bytes"] += int(copied.size)
                elif copied.issym():
                    out.addfile(copied)
                    stats["symlink_count"] += 1
                elif copied.islnk():
                    copied.linkname = rewrite_workspace_linkname(copied.linkname, base_prefix)
                    out.addfile(copied)
                    stats["other_count"] += 1
                else:
                    out.addfile(copied)
                    stats["other_count"] += 1

    payload = raw.getvalue()
    stats["layer_stack_archive_bytes"] = len(payload)
    return payload, stats


def workspace_member_relpath(name: str) -> str | None:
    normalized = PurePosixPath(name).as_posix().lstrip("./").lstrip("/")
    root = WORKSPACE_ROOT.strip("/")
    if normalized in {"", "."}:
        return ""
    if normalized == root:
        return ""
    prefix = f"{root}/"
    if normalized.startswith(prefix):
        return normalized[len(prefix) :]
    return None


def rewrite_workspace_linkname(linkname: str, base_prefix: str) -> str:
    rel = workspace_member_relpath(linkname)
    if rel is None:
        return linkname
    return base_prefix if rel == "" else f"{base_prefix}/{rel}"


def copy_tar_info(member: tarfile.TarInfo, name: str) -> tarfile.TarInfo:
    copied = copy.copy(member)
    copied.name = name
    copied.uid = 0
    copied.gid = 0
    copied.uname = ""
    copied.gname = ""
    copied.mtime = 0
    return copied


def add_parent_tar_dirs(
    tar: tarfile.TarFile,
    target_name: str,
    added: set[str],
) -> None:
    parent = PurePosixPath(target_name).parent
    current = PurePosixPath("")
    for part in parent.parts:
        current = current / part
        add_tar_dir(tar, f"/{current.as_posix()}", added)


def add_tar_dir(tar: tarfile.TarFile, path: str, added: set[str]) -> None:
    name = path.strip("/")
    if not name or name in added:
        return
    add_parent_tar_dirs(tar, name, added)
    info = tarfile.TarInfo(name)
    info.type = tarfile.DIRTYPE
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mode = 0o755
    tar.addfile(info)
    added.add(name)


def add_tar_file(
    tar: tarfile.TarFile,
    path: str,
    payload: bytes,
    added: set[str],
) -> None:
    name = path.strip("/")
    add_parent_tar_dirs(tar, name, added)
    info = tarfile.TarInfo(name)
    info.size = len(payload)
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mode = 0o644
    tar.addfile(info, io.BytesIO(payload))


def small_write_request(index: int, invocation_id: str) -> tuple[str, dict[str, Any]]:
    filename = f"phase3-small-{index:03d}.txt"
    return (
        "api.v1.exec_command",
        {
            "cmd": f"touch {filename}",
            "cwd": ".",
            "timeout_seconds": 10,
            "invocation_id": invocation_id,
            "expected_path": filename,
        },
    )


def load_small_write_request(
    index: int,
    concurrency: int,
    round_index: int,
    slot: int,
    invocation_id: str,
) -> tuple[str, dict[str, Any]]:
    filename = (
        f"phase3-load-c{concurrency:02d}-r{round_index:03d}-"
        f"s{slot:02d}-{index:04d}.txt"
    )
    return (
        "api.v1.exec_command",
        {
            "cmd": f"touch {filename}",
            "cwd": ".",
            "timeout_seconds": 10,
            "invocation_id": invocation_id,
            "expected_path": filename,
        },
    )


async def measure_operation(
    daemon_client: Any,
    endpoint: Any,
    name: str,
    count: int,
    build_request: Callable[[int, str], tuple[str, dict[str, Any]]],
    expect: Callable[[dict[str, Any]], bool],
) -> dict[str, Any]:
    samples: list[dict[str, Any]] = []
    for index in range(max(0, count)):
        invocation_id = f"phase3-{name}-{index:03d}-{uuid.uuid4().hex[:8]}"
        op, args = build_request(index, invocation_id)
        expected_path = args.pop("expected_path", None)
        started = time.perf_counter()
        response = await call_tcp(
            daemon_client,
            endpoint,
            daemon_request(op, args, invocation_id),
        )
        host_wall_ms = elapsed_ms(started)
        ok = expect(response)
        if expected_path is not None:
            ok = ok and expected_path in response.get("changed_paths", [])
        timings_ms = timing_ms(response)
        samples.append(
            {
                "index": index,
                "invocation_id": invocation_id,
                "op": op,
                "host_wall_ms": host_wall_ms,
                "ok": ok,
                "timings_ms": timings_ms,
                "derived_ms": derived_timings_ms(name, host_wall_ms, timings_ms),
                "response": trim_response(response),
            }
        )
    return summarize_operation(samples)


async def measure_load_matrix(
    daemon_client: Any,
    endpoint: Any,
    *,
    concurrencies: list[int],
    rounds: int,
) -> dict[str, Any]:
    operations: dict[str, dict[str, Any]] = {"noop_shell": {}, "small_write": {}}
    for concurrency in concurrencies:
        operations["noop_shell"][str(concurrency)] = await measure_concurrent_operation(
            daemon_client,
            endpoint,
            name="load_noop_shell",
            operation_key="noop_shell",
            concurrency=concurrency,
            rounds=rounds,
            build_request=lambda _index, _concurrency, _round, _slot, invocation_id: (
                "api.v1.exec_command",
                {
                    "cmd": "true",
                    "cwd": ".",
                    "timeout_seconds": 10,
                    "invocation_id": invocation_id,
                },
            ),
            expect=expect_noop_shell,
        )
        operations["small_write"][str(concurrency)] = await measure_concurrent_operation(
            daemon_client,
            endpoint,
            name="load_small_write",
            operation_key="small_write",
            concurrency=concurrency,
            rounds=rounds,
            build_request=load_small_write_request,
            expect=expect_small_write,
        )
    return {
        "skipped": False,
        "concurrency_levels": concurrencies,
        "rounds_per_concurrency": rounds,
        "operations": operations,
    }


async def measure_concurrent_operation(
    daemon_client: Any,
    endpoint: Any,
    *,
    name: str,
    operation_key: str,
    concurrency: int,
    rounds: int,
    build_request: Callable[[int, int, int, int, str], tuple[str, dict[str, Any]]],
    expect: Callable[[dict[str, Any]], bool],
) -> dict[str, Any]:
    samples: list[dict[str, Any]] = []
    waves: list[dict[str, Any]] = []
    if concurrency <= 0 or rounds <= 0:
        return {
            **summarize_operation(samples),
            "concurrency": concurrency,
            "rounds": rounds,
            "wave_wall_ms": summarize_samples([]),
            "throughput_cmd_s": 0.0,
            "waves": waves,
        }

    async def run_one(round_index: int, slot: int) -> dict[str, Any]:
        index = round_index * concurrency + slot
        invocation_id = (
            f"phase3-{name}-c{concurrency:02d}-r{round_index:03d}-"
            f"s{slot:02d}-{uuid.uuid4().hex[:8]}"
        )
        op, args = build_request(index, concurrency, round_index, slot, invocation_id)
        expected_path = args.pop("expected_path", None)
        started = time.perf_counter()
        response = await call_tcp(
            daemon_client,
            endpoint,
            daemon_request(op, args, invocation_id),
        )
        host_wall_ms = elapsed_ms(started)
        ok = expect(response)
        if expected_path is not None:
            ok = ok and expected_path in response.get("changed_paths", [])
        timings_ms = timing_ms(response)
        return {
            "index": index,
            "round": round_index,
            "slot": slot,
            "concurrency": concurrency,
            "invocation_id": invocation_id,
            "op": op,
            "host_wall_ms": host_wall_ms,
            "ok": ok,
            "timings_ms": timings_ms,
            "derived_ms": derived_timings_ms(operation_key, host_wall_ms, timings_ms),
            "response": trim_response(response),
        }

    for round_index in range(rounds):
        wave_started = time.perf_counter()
        wave_samples = await asyncio.gather(
            *(run_one(round_index, slot) for slot in range(concurrency))
        )
        wave_wall_ms = elapsed_ms(wave_started)
        samples.extend(wave_samples)
        waves.append(
            {
                "round": round_index,
                "concurrency": concurrency,
                "wave_wall_ms": wave_wall_ms,
                "success_count": sum(1 for sample in wave_samples if sample["ok"]),
                "sample_count": len(wave_samples),
            }
        )

    summary = summarize_operation(samples)
    total_wave_wall_ms = sum(float(wave["wave_wall_ms"]) for wave in waves)
    summary.update(
        {
            "concurrency": concurrency,
            "rounds": rounds,
            "wave_wall_ms": summarize_samples(
                [float(wave["wave_wall_ms"]) for wave in waves]
            ),
            "throughput_cmd_s": (
                len(samples) / (total_wave_wall_ms / 1000.0)
                if total_wave_wall_ms > 0.0
                else 0.0
            ),
            "waves": waves,
        }
    )
    return summary


def daemon_request(op: str, args: dict[str, Any], invocation_id: str) -> str:
    wire_args = {
        "layer_stack_root": LAYER_STACK_ROOT,
        "agent_id": AGENT_ID,
        **args,
    }
    wire_args.setdefault("invocation_id", invocation_id)
    return json.dumps(
        {
            "op": op,
            "invocation_id": invocation_id,
            "args": wire_args,
        },
        separators=(",", ":"),
    )


def expect_noop_shell(response: dict[str, Any]) -> bool:
    return (
        response.get("success") is True
        and response.get("exit_code") == 0
        and response.get("status") == "ok"
    )


def expect_small_write(response: dict[str, Any]) -> bool:
    return (
        response.get("success") is True
        and response.get("exit_code") == 0
        and response.get("status") in {"ok", "committed"}
    )


def expect_search_count(response: dict[str, Any], minimum: int) -> bool:
    return response.get("success") is True and int(response.get("num_files") or 0) >= minimum


def timing_ms(response: dict[str, Any]) -> dict[str, float]:
    out: dict[str, float] = {}
    timings = response.get("timings")
    if not isinstance(timings, dict):
        return out
    for key, value in timings.items():
        if key.endswith("_s") and isinstance(value, int | float):
            out[key] = float(value) * 1000.0
    return out


def derived_timings_ms(
    operation: str,
    host_wall_ms: float,
    timings: dict[str, float],
) -> dict[str, float]:
    api_keys = {
        "noop_shell": "api.shell.total_s",
        "small_write": "api.shell.total_s",
        "glob": "api.glob.total_s",
        "grep": "api.grep.total_s",
    }
    derived: dict[str, float] = {}
    api_total_ms = timings.get(api_keys[operation])
    if api_total_ms is not None:
        derived["host_minus_api_total_ms"] = max(0.0, host_wall_ms - api_total_ms)
    return derived


def summarize_operation(samples: list[dict[str, Any]]) -> dict[str, Any]:
    timing_keys = sorted(
        {
            key
            for sample in samples
            for key in sample["timings_ms"]
            if isinstance(sample["timings_ms"].get(key), int | float)
        }
    )
    derived_keys = sorted(
        {
            key
            for sample in samples
            for key in sample["derived_ms"]
            if isinstance(sample["derived_ms"].get(key), int | float)
        }
    )
    return {
        "success_count": sum(1 for sample in samples if sample["ok"]),
        "sample_count": len(samples),
        "all_samples_ok": bool(samples) and all(sample["ok"] for sample in samples),
        "host_wall_ms": summarize_samples([sample["host_wall_ms"] for sample in samples]),
        "phase_timing_ms": {
            key: summarize_samples(
                [
                    sample["timings_ms"][key]
                    for sample in samples
                    if key in sample["timings_ms"]
                ]
            )
            for key in timing_keys
        },
        "derived_ms": {
            key: summarize_samples(
                [
                    sample["derived_ms"][key]
                    for sample in samples
                    if key in sample["derived_ms"]
                ]
            )
            for key in derived_keys
        },
        "samples": samples,
    }


async def sample_daemon_memory(bench: DockerBench, label: str) -> dict[str, Any]:
    pid = await read_pid(bench)
    if pid is None:
        return {"label": label, "pid": None, "available": False}
    command = f"""
set -eu
pid={pid}
echo "pid=$pid"
if [ -r "/proc/$pid/smaps_rollup" ]; then
  awk '/^(Rss|Pss|Private_Clean|Private_Dirty):/ {{gsub(":", "", $1); print "smaps_" $1 "_kb=" $2}}' "/proc/$pid/smaps_rollup"
fi
if [ -r "/proc/$pid/status" ]; then
  awk '/^(VmRSS|VmSize):/ {{gsub(":", "", $1); print "status_" $1 "_kb=" $2}}' "/proc/$pid/status"
  awk '/^Threads:/ {{print "status_Threads=" $2}}' "/proc/$pid/status"
fi
if [ -r "/proc/$pid/cmdline" ]; then
  printf 'cmdline='
  tr '\\0' ' ' < "/proc/$pid/cmdline"
  printf '\\n'
fi
if [ -e "/proc/$pid/exe" ]; then
  printf 'exe='
  readlink "/proc/$pid/exe" || true
fi
if [ -r /sys/fs/cgroup/memory.current ]; then
  printf 'cgroup_memory_current_bytes='
  cat /sys/fs/cgroup/memory.current
elif [ -r /sys/fs/cgroup/memory/memory.usage_in_bytes ]; then
  printf 'cgroup_memory_current_bytes='
  cat /sys/fs/cgroup/memory/memory.usage_in_bytes
fi
"""
    result = await bench.exec(command, timeout=15)
    values: dict[str, Any] = {"label": label, "pid": pid, "available": True}
    for line in getattr(result, "stdout", "").splitlines():
        if "=" not in line:
            continue
        key, raw = line.split("=", 1)
        raw = raw.strip()
        try:
            values[key] = int(raw)
        except ValueError:
            values[key] = raw
    private_clean = values.get("smaps_Private_Clean_kb")
    private_dirty = values.get("smaps_Private_Dirty_kb")
    if isinstance(private_clean, int) and isinstance(private_dirty, int):
        values["smaps_Private_Total_kb"] = private_clean + private_dirty
    values["smaps_rollup_available"] = "smaps_Pss_kb" in values
    return values


def summarize_memory(
    samples: list[dict[str, Any]],
    phase0: dict[str, Any],
) -> dict[str, Any]:
    baseline_rss_kb = int(phase0.get("cp0", {}).get("daemon_idle_rss_kb") or 0)
    before = next(
        (sample for sample in samples if sample.get("label") == "idle_before_load"),
        {},
    )
    after = next(
        (sample for sample in samples if sample.get("label") == "idle_after_drain"),
        {},
    )
    pss_values = int_values(samples, "smaps_Pss_kb")
    rss_values = int_values(samples, "smaps_Rss_kb") or int_values(samples, "status_VmRSS_kb")
    peak_pss_kb = max(pss_values) if pss_values else None
    peak_rss_kb = max(rss_values) if rss_values else None
    before_pss = before.get("smaps_Pss_kb")
    after_pss = after.get("smaps_Pss_kb")
    before_rss = before.get("smaps_Rss_kb") or before.get("status_VmRSS_kb")
    after_rss = after.get("smaps_Rss_kb") or after.get("status_VmRSS_kb")
    idle_before = before_pss if isinstance(before_pss, int) else before_rss
    idle_after = after_pss if isinstance(after_pss, int) else after_rss
    idle_return = within_idle_return(idle_before, idle_after)
    active_basis = "pss" if peak_pss_kb is not None else "rss"
    active_peak = peak_pss_kb if peak_pss_kb is not None else peak_rss_kb
    rosetta_translated = any(
        "/run/rosetta/rosetta" in str(sample.get("cmdline", ""))
        or "/run/rosetta/rosetta" in str(sample.get("exe", ""))
        for sample in samples
    )
    active_memory_headroom_kb = (
        ROSETTA_ACTIVE_MEMORY_HEADROOM_KB if rosetta_translated else 0
    )
    active_memory_limit_kb = baseline_rss_kb + active_memory_headroom_kb
    active_memory_gate = (
        isinstance(active_peak, int)
        and baseline_rss_kb > 0
        and active_peak <= active_memory_limit_kb
    )
    rosetta_idle_ceiling = (
        rosetta_translated
        and active_memory_gate
        and isinstance(active_peak, int)
        and isinstance(idle_after, int)
        and idle_after <= active_peak + max(int(active_peak * 0.10), 2048)
    )
    idle_return_basis = (
        "cold_idle"
        if idle_return
        else "rosetta_active_peak_ceiling"
        if rosetta_idle_ceiling
        else "cold_idle_failed"
    )
    idle_return_gate = idle_return or rosetta_idle_ceiling
    return {
        "samples": samples,
        "baseline": {
            "cp0_daemon_idle_rss_kb": baseline_rss_kb,
            "active_memory_headroom_kb": active_memory_headroom_kb,
            "active_memory_limit_kb": active_memory_limit_kb,
            "active_memory_baseline_note": (
                "Phase 0 report lacks active daemon PSS; using CP-0 idle RSS as "
                "the conservative fallback until an active Python PSS baseline is captured."
            ),
        },
        "peak_pss_kb": peak_pss_kb,
        "peak_rss_kb": peak_rss_kb,
        "active_memory_basis": active_basis,
        "active_memory_gate_pass": active_memory_gate,
        "rosetta_translated": rosetta_translated,
        "idle_return_basis": idle_return_basis,
        "idle_return_gate_pass": idle_return_gate,
        "gate_pass": bool(active_memory_gate and idle_return_gate),
    }


def int_values(samples: list[dict[str, Any]], key: str) -> list[int]:
    return [sample[key] for sample in samples if isinstance(sample.get(key), int)]


def within_idle_return(before: object, after: object) -> bool:
    if not isinstance(before, int) or not isinstance(after, int):
        return False
    return after <= before + max(int(before * 0.10), 2048)


async def collect_final_state(
    daemon_client: Any,
    endpoint: Any,
    samples: int,
) -> dict[str, Any]:
    filenames = ["README.md"] + [f"phase3-small-{index:03d}.txt" for index in range(samples)]
    contents: dict[str, str] = {}
    for filename in filenames:
        response = await call_tcp(
            daemon_client,
            endpoint,
            daemon_request(
                "api.v1.read_file",
                {"path": f"{WORKSPACE_ROOT}/{filename}"},
                f"phase3-final-read-{uuid.uuid4().hex[:8]}",
            ),
        )
        contents[filename] = str(response.get("content", ""))
    expected = {
        f"phase3-small-{index:03d}.txt": "" for index in range(samples)
    }
    digest = hashlib.sha256(
        json.dumps(contents, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    return {
        "path_count": len(contents),
        "sha256": digest,
        "small_write_readback_match": all(
            contents.get(path) == content for path, content in expected.items()
        ),
        "gate_pass": all(contents.get(path) == content for path, content in expected.items()),
    }


def evaluate_cp4s(report: dict[str, Any], phase1: dict[str, Any]) -> dict[str, Any]:
    operations = report["operations"]
    phase1_perf = phase1["performance"]
    phase1_host = phase1_perf["host_wall_ms"]
    phase1_tool = phase1_perf["runner_tool_ms"]
    noop = operations["noop_shell"]
    noop_host = noop["host_wall_ms"]
    noop_run_command = stat_block(noop, "phase_timing_ms", "command_exec.run_command_s")
    noop_mount = stat_block(noop, "phase_timing_ms", "command_exec.mount_workspace_s")
    noop_dispatch = stat_block(noop, "derived_ms", "host_minus_api_total_ms")

    host_latency_gate = (
        stat_value(noop_host, "p50") <= float(phase1_host["p50"])
        and stat_value(noop_host, "p95") <= float(phase1_host["p95"])
    )
    shell_gate = stat_value(noop_run_command, "p50") <= float(phase1_tool["p50"]) * 0.30
    mount_gate = stat_value(noop_mount, "p95") <= 5.0
    dispatch_gate = stat_value(noop_dispatch, "p95") <= 5.0
    operations_gate = all(
        bool(operation["all_samples_ok"]) for operation in operations.values()
    )
    memory_gate = bool(report["memory"]["gate_pass"])
    gate_pass = all(
        [
            operations_gate,
            host_latency_gate,
            shell_gate,
            mount_gate,
            dispatch_gate,
            memory_gate,
        ]
    )
    return {
        "thresholds": {
            "phase1_noop_shell_host_wall_p50_ms": phase1_host["p50"],
            "phase1_noop_shell_host_wall_p95_ms": phase1_host["p95"],
            "phase1_runner_tool_p50_ms": phase1_tool["p50"],
            "required_shell_runner_tool_p50_ms": float(phase1_tool["p50"]) * 0.30,
            "overlay_mount_p95_max_ms": 5.0,
            "host_minus_api_total_p95_max_ms": 5.0,
        },
        "observed": {
            "noop_shell_host_wall_ms": noop_host,
            "noop_shell_run_command_ms": noop_run_command,
            "noop_shell_mount_ms": noop_mount,
            "noop_shell_host_minus_api_total_ms": noop_dispatch,
        },
        "gates": {
            "operations_all_samples_ok": operations_gate,
            "noop_host_no_worse_than_phase1": host_latency_gate,
            "shell_noop_70pct_faster_than_phase1": shell_gate,
            "overlay_mount_p95_lte_5ms": mount_gate,
            "host_minus_api_total_p95_lte_5ms": dispatch_gate,
            "daemon_memory": memory_gate,
        },
        "gate_pass": gate_pass,
    }


def evaluate_load_matrix(load: dict[str, Any], phase1: dict[str, Any]) -> dict[str, Any]:
    if load.get("skipped"):
        return {"skipped": True, "gate_pass": True}
    phase1_perf = phase1["performance"]
    phase1_host_p95 = float(phase1_perf["host_wall_ms"]["p95"])
    required_run_p50 = float(phase1_perf["runner_tool_ms"]["p50"]) * 0.30
    operation_gates: dict[str, dict[str, Any]] = {}
    all_gates: list[bool] = []
    for operation_name, by_concurrency in load.get("operations", {}).items():
        operation_gates[operation_name] = {}
        for concurrency, summary in by_concurrency.items():
            host = summary.get("host_wall_ms", {})
            run_command = stat_block(
                summary,
                "phase_timing_ms",
                "command_exec.run_command_s",
            )
            gates = {
                "all_samples_ok": bool(summary.get("all_samples_ok")),
                "host_p95_no_worse_than_phase1": stat_value(host, "p95")
                <= phase1_host_p95,
            }
            if operation_name == "noop_shell":
                gates["run_command_p50_70pct_faster_than_phase1"] = (
                    stat_value(run_command, "p50") <= required_run_p50
                )
            gates["gate_pass"] = all(gates.values())
            operation_gates[operation_name][concurrency] = gates
            all_gates.append(bool(gates["gate_pass"]))
    return {
        "skipped": False,
        "thresholds": {
            "phase1_host_wall_p95_ms": phase1_host_p95,
            "required_noop_run_command_p50_ms": required_run_p50,
        },
        "operation_gates": operation_gates,
        "gate_pass": bool(all_gates) and all(all_gates),
    }


def stat_block(operation: dict[str, Any], section: str, key: str) -> dict[str, Any]:
    return operation.get(section, {}).get(key, {"count": 0, "samples_ms": []})


def stat_value(stats: dict[str, Any], key: str) -> float:
    value = stats.get(key)
    if isinstance(value, int | float):
        return float(value)
    return float("inf")


def trim_response(response: dict[str, Any]) -> dict[str, Any]:
    trimmed = {
        "success": response.get("success"),
        "status": response.get("status"),
        "exit_code": response.get("exit_code"),
        "changed_paths": response.get("changed_paths"),
        "num_files": response.get("num_files"),
        "num_matches": response.get("num_matches"),
        "filenames": response.get("filenames"),
        "conflict_reason": response.get("conflict_reason"),
        "error": response.get("error"),
    }
    for key in ("stdout", "stderr", "content"):
        if key in response:
            trimmed[key] = truncate_text(str(response.get(key) or ""))
    return {key: value for key, value in trimmed.items() if value is not None}


def truncate_text(value: str, limit: int = 400) -> str:
    if len(value) <= limit:
        return value
    return f"{value[:limit]}...<truncated {len(value) - limit} chars>"


def parse_concurrency_levels(raw: str) -> list[int]:
    levels: list[int] = []
    for part in raw.split(","):
        part = part.strip()
        if not part:
            continue
        value = int(part)
        if value <= 0:
            raise argparse.ArgumentTypeError("concurrency levels must be positive integers")
        if value not in levels:
            levels.append(value)
    if not levels:
        raise argparse.ArgumentTypeError("at least one concurrency level is required")
    return levels


if __name__ == "__main__":
    raise SystemExit(main())
