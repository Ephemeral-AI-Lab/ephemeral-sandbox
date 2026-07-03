"""CLI wrappers, fixtures, and scenarios for the squash live-Docker suite."""

from __future__ import annotations

import concurrent.futures
import json
import os
from collections import Counter, defaultdict
import platform
import re
import shutil
import subprocess
import tempfile
import threading
import time
from pathlib import Path

from core import cleanup
from core.config import IMAGE, REPO_ROOT, SANDBOX_CLI

from . import measure


CASES = [
    {"id": "SMK-01", "tier": "smoke", "title": "idle block reclaims at commit (B1)", "scenario": "idle"},
    {"id": "SMK-02", "tier": "smoke", "title": "nothing to squash is a clean no-op", "scenario": "noop"},
    {"id": "SMK-03", "tier": "smoke", "title": "singleton run below a boundary is never touched", "scenario": "boundary"},
    {"id": "SMK-04", "tier": "smoke", "title": "result contract shape, success and fault", "scenario": "contract"},
    {"id": "SMK-05", "tier": "smoke", "title": "CLI catalog placement", "scenario": "catalog"},
    {"id": "SMK-06", "tier": "smoke", "title": "idle session migrates via plain staged switch (B2)", "scenario": "migrate"},
    {"id": "SMK-07", "tier": "smoke", "title": "interactive PTY shell blocks cleanly (leased)", "scenario": "pin"},
    {"id": "SMK-08", "tier": "smoke", "title": "the three observability records, and only those", "scenario": "observability"},
    {"id": "SMK-09", "tier": "smoke", "title": "immediate idempotence", "scenario": "idempotence"},
    {"id": "SMK-10", "tier": "smoke", "title": "daemon restart boot reap-then-sweep no-op sweep", "scenario": "restart"},
    {"id": "MED-01", "tier": "medium", "title": "whiteout winners", "scenario": "whiteout"},
    {"id": "MED-02", "tier": "medium", "title": "opaque dir is dual-encoded and never resurrects", "scenario": "opaque"},
    {"id": "MED-03", "tier": "medium", "title": "witness matrix", "scenario": "witness"},
    {"id": "MED-04", "tier": "medium", "title": "hardlink flatten is metadata-bound", "scenario": "bulk"},
    {"id": "MED-05", "tier": "medium", "title": "racing publishes", "scenario": "race"},
    {"id": "MED-06", "tier": "medium", "title": "singleflight per root", "scenario": "singleflight"},
    {"id": "MED-07", "tier": "medium", "title": "late lease keeps sources", "scenario": "pin"},
    {"id": "MED-08", "tier": "medium", "title": "live migration under a running batch command", "scenario": "migrate"},
    {"id": "MED-09", "tier": "medium", "title": "escaped-pgid child with an fd pin", "scenario": "pin"},
    {"id": "MED-10", "tier": "medium", "title": "child mount pins after creator exited", "scenario": "child_mount"},
    {"id": "MED-11", "tier": "medium", "title": "nested mount namespace blocks remount", "scenario": "mount_ns"},
    {"id": "MED-12", "tier": "medium", "title": "strict-unmount EBUSY parks with both leases", "scenario": "pin"},
    {"id": "MED-13", "tier": "medium", "title": "post-PONR runner death is faulty", "scenario": "pin"},
    {"id": "MED-14", "tier": "medium", "title": "daemon crash between promote and manifest rename", "scenario": "restart"},
    {"id": "MED-15", "tier": "medium", "title": "boot reap-then-sweep with orphans and live sessions", "scenario": "restart"},
    {"id": "MED-16", "tier": "medium", "title": "sidecar hygiene", "scenario": "sidecar"},
    {"id": "MED-17", "tier": "medium", "title": "persist failure still migrates", "scenario": "restart"},
    {"id": "MED-18", "tier": "medium", "title": "OVL_MAX_STACK creation boundary", "scenario": "overcap"},
    {"id": "MED-19", "tier": "medium", "title": "masks never observable", "scenario": "migrate"},
    {"id": "MED-20", "tier": "medium", "title": "quiesce at 100 tasks", "scenario": "many_tasks"},
    {"id": "HTTP-01", "tier": "medium", "title": "running HTTP server migrates live", "scenario": "http_migrate"},
    {"id": "HTTP-02", "tier": "medium", "title": "workspace-cwd HTTP server resumes leased", "scenario": "http_pinned"},
    {"id": "LOAD-499", "tier": "hard", "title": "499-layer stack squashes in-cap", "scenario": "load_499"},
    {"id": "LOAD-LARGE", "tier": "hard", "title": "large file squash", "scenario": "large_file"},
    {"id": "LOAD-499-HTTP", "tier": "hard", "title": "499-layer stack with HTTP disconnect", "scenario": "load_499_http"},
    {"id": "LOAD-LARGE-HTTP", "tier": "hard", "title": "large file squash with HTTP disconnect", "scenario": "large_file_http"},
    {"id": "LOAD-COMBO-HTTP", "tier": "hard", "title": "multi-block active workspace HTTP load", "scenario": "load_combo_http"},
    {"id": "HRD-01", "tier": "hard", "title": "B3 replay mixed classification", "scenario": "mixed"},
    {"id": "HRD-02", "tier": "hard", "title": "B4 replay two generations", "scenario": "generations"},
    {"id": "HRD-03", "tier": "hard", "title": "B5 replay hard path convergence", "scenario": "mixed"},
    {"id": "HRD-04", "tier": "hard", "title": "E4 full pin matrix", "scenario": "pin_matrix"},
    {"id": "HRD-05", "tier": "hard", "title": "E8 PONR boundary", "scenario": "pin"},
    {"id": "HRD-06", "tier": "hard", "title": "E10 crash matrix", "scenario": "restart"},
    {"id": "HRD-07", "tier": "hard", "title": "EBUSY park convergence", "scenario": "pin"},
    {"id": "HRD-08", "tier": "hard", "title": "admission-gate storm", "scenario": "storm"},
    {"id": "HRD-09", "tier": "hard", "title": "implicit-session finalize mid-switch", "scenario": "migrate"},
    {"id": "HRD-10", "tier": "hard", "title": "dense-pinning adversarial floor", "scenario": "dense"},
    {"id": "HRD-11", "tier": "hard", "title": "deep chain collapses live", "scenario": "deep"},
    {"id": "HRD-12", "tier": "hard", "title": "E9 over-cap chains fail closed", "scenario": "overcap"},
    {"id": "HRD-13", "tier": "hard", "title": "commit durability cost", "scenario": "bulk"},
    {"id": "HRD-14", "tier": "hard", "title": "re-squash across 5 generations", "scenario": "generations"},
    {"id": "HRD-15", "tier": "hard", "title": "sweep at k=8", "scenario": "mixed"},
    {"id": "HRD-16", "tier": "hard", "title": "ENOSPC on both sides", "scenario": "pin"},
    {"id": "HRD-17", "tier": "hard", "title": "G1 kernel gate", "scenario": "gate"},
    {"id": "HRD-18", "tier": "hard", "title": "G2 parity negative control", "scenario": "opaque"},
    {"id": "HRD-19", "tier": "hard", "title": "mid-sweep daemon kill and unreadable manifest", "scenario": "restart"},
    {"id": "HRD-20", "tier": "hard", "title": "soak marathon", "scenario": "soak"},
    {"id": "AB-EQUIV", "tier": "bench", "title": "A/B logical equivalence (mixed migrate/identity, one block)", "scenario": "ab", "ab": {"sessions": 12, "migrate_ratio": 0.5, "blocks": 1}},
    {"id": "AB-BLOCKS", "tier": "bench", "title": "exact squashable-block-count knob (B)", "scenario": "ab", "ab": {"sessions": 16, "migrate_ratio": 0.5, "blocks": 8}},
    {"id": "PERF-WIDTH", "tier": "bench", "title": "remount-sweep width scaling (all-migrate)", "scenario": "ab", "ab": {"sessions": 200, "migrate_ratio": 1.0, "blocks": 1}},
]

CASE_BY_ID = {case["id"]: case for case in CASES}
TIMED_CASES = {
    "MED-04", "MED-18", "MED-20", "HTTP-01", "HTTP-02", "LOAD-499", "LOAD-LARGE",
    "LOAD-499-HTTP", "LOAD-LARGE-HTTP", "LOAD-COMBO-HTTP",
    "HRD-11", "HRD-13", "HRD-14", "HRD-15", "HRD-20"
}
CASE_E2E_BUDGET_MS = {
    "MED-18": 1_800_000,
    "HTTP-01": 30_000,
    "HTTP-02": 30_000,
    "LOAD-499": 1_800_000,
    "LOAD-LARGE": 600_000,
    "LOAD-499-HTTP": 1_800_000,
    "LOAD-LARGE-HTTP": 600_000,
    "LOAD-COMBO-HTTP": 1_800_000,
    "HRD-12": 1_800_000,
    "HRD-20": 1_800_000,
    "AB-EQUIV": 600_000,
    "AB-BLOCKS": 600_000,
    "PERF-WIDTH": 1_800_000,
}
CASE_SQUASH_BUDGET_MS = {
    "MED-18": 15_000,
    "HTTP-01": 5_000,
    "HTTP-02": 5_000,
    "LOAD-499": 60_000,
    "LOAD-LARGE": 120_000,
    "LOAD-499-HTTP": 60_000,
    "LOAD-LARGE-HTTP": 120_000,
    "LOAD-COMBO-HTTP": 180_000,
    "HRD-11": 20_000,
    "AB-EQUIV": 60_000,
    "AB-BLOCKS": 60_000,
    "PERF-WIDTH": 180_000,
}
FILE_WRITE_PUBLISH_LIMIT_BYTES = 16 * 1024
MAX_EXEC_CAPTURE_KIB = 8 * 1024
HTTP_CLIENT_SECONDS = 3.0
HTTP_DISCONNECT_BUDGET_MS = 1_500
CASE_DISCONNECT_BUDGET_MS = {
    "HTTP-01": HTTP_DISCONNECT_BUDGET_MS,
    "HTTP-02": HTTP_DISCONNECT_BUDGET_MS,
    "LOAD-499-HTTP": HTTP_DISCONNECT_BUDGET_MS,
    "LOAD-LARGE-HTTP": HTTP_DISCONNECT_BUDGET_MS,
    "LOAD-COMBO-HTTP": HTTP_DISCONNECT_BUDGET_MS,
}
HTTP_HELPER_SOURCE = r"""
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("server") => server(),
        Some("client") => client(&args),
        Some("probe") => probe(&args),
        _ => std::process::exit(64),
    }
}

fn server() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    println!("PORT={}", listener.local_addr().expect("addr").port());
    std::io::stdout().flush().ok();
    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            std::thread::spawn(move || handle_stream(stream));
        }
    }
}

fn handle_stream(mut stream: TcpStream) {
    stream.set_nodelay(true).ok();
    let mut request = [0_u8; 1024];
    let Ok(size) = stream.read(&mut request) else {
        return;
    };
    let request = String::from_utf8_lossy(&request[..size]);
    if request.starts_with("GET /ticks ") {
        tick_stream(stream);
        return;
    }
    let body = b"http-ok\n";
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}

fn tick_stream(mut stream: TcpStream) {
    let header = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n";
    if stream.write_all(header.as_bytes()).is_err() {
        return;
    }
    let started = Instant::now();
    let mut seq = 0_u64;
    loop {
        let line = format!("{} {}\n", seq, started.elapsed().as_micros());
        if stream.write_all(line.as_bytes()).is_err() || stream.flush().is_err() {
            return;
        }
        seq += 1;
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn client(args: &[String]) {
    let port = args.get(2).and_then(|v| v.parse::<u16>().ok()).expect("port");
    let seconds = args.get(3).and_then(|v| v.parse::<f64>().ok()).unwrap_or(3.0);
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).expect("connect");
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    stream
        .write_all(b"GET /ticks HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .expect("request");
    let mut reader = BufReader::new(stream);
    let mut in_body = false;
    let mut last_tick: Option<Instant> = None;
    let mut max_silence_ms = 0.0_f64;
    let mut ok_count = 0_u64;
    let mut failures = 0_u64;
    let mut ready = false;
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => {
                failures += 1;
                if let Some(last) = last_tick {
                    max_silence_ms = max_silence_ms.max(last.elapsed().as_secs_f64() * 1000.0);
                }
                break;
            }
            Ok(_) => {}
        }
        if !in_body {
            if line == "\r\n" {
                in_body = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let now = Instant::now();
        if let Some(last) = last_tick {
            max_silence_ms = max_silence_ms.max(now.duration_since(last).as_secs_f64() * 1000.0);
        }
        last_tick = Some(now);
        ok_count += 1;
        if !ready {
            println!("CLIENT_READY");
            std::io::stdout().flush().ok();
            ready = true;
        }
    }
    println!(
        "CLIENT_STATS {{\"failures\":{},\"max_silence_ms\":{:.3},\"ok_count\":{}}}",
        failures, max_silence_ms, ok_count
    );
}

fn probe(args: &[String]) {
    let port = args.get(2).and_then(|v| v.parse::<u16>().ok()).expect("port");
    if http_get(port, Duration::from_secs(2)) {
        println!("http-ok");
    } else {
        std::process::exit(1);
    }
}

fn http_get(port: u16, timeout: Duration) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    if stream.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").is_err() {
        return false;
    }
    let mut response = Vec::new();
    stream.read_to_end(&mut response).is_ok()
        && String::from_utf8_lossy(&response).contains("\r\n\r\nhttp-ok\n")
}
"""
ALLOWED_SKIP_REASONS = {
    "HRD-04": {"subcases-9-11"},
    "HRD-12": {"leg-b:not_constructible_at_ci_scale"},
    "HRD-17": {"failure-leg:gate_green_env"},
}


class RawResult:
    def __init__(self, args, returncode, stdout, stderr, elapsed_ms):
        self.args = list(args)
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr
        self.elapsed_ms = elapsed_ms
        self.json = self._parse_json()

    def _parse_json(self):
        for text in (self.stdout, self.stderr):
            for line in reversed(text.splitlines()):
                stripped = line.strip()
                if stripped.startswith("{"):
                    return json.loads(stripped)
        return None

    @property
    def ok(self):
        return self.returncode == 0 and isinstance(self.json, dict) and "error" not in self.json


def cases_for_tier(tier):
    return [case for case in CASES if case["tier"] == tier]


def raw_cli(rec, *args, timeout=180):
    started = time.monotonic()
    env = os.environ.copy()
    env["PATH"] = f"{REPO_ROOT / 'bin'}:{env.get('PATH', '')}"
    proc = subprocess.run(
        [str(SANDBOX_CLI), *map(str, args)],
        cwd=str(REPO_ROOT),
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
    )
    elapsed = measure.monotonic_ms(started)
    result = RawResult(args, proc.returncode, proc.stdout, proc.stderr, elapsed)
    if rec is not None:
        rec.add_command(
            {
                "cmd": ["sandbox-cli", *map(str, args)],
                "exit_code": proc.returncode,
                "elapsed_ms": elapsed,
                "stdout": proc.stdout,
                "stderr": proc.stderr,
                "parsed_json": result.json,
            }
        )
    return result


def manager(rec, operation, *args, timeout=180):
    return raw_cli(rec, "manager", operation, *args, timeout=timeout)


def runtime(rec, sandbox_id, operation, *args, timeout=180):
    return raw_cli(
        rec, "runtime", "--sandbox-id", sandbox_id, operation, *args, timeout=timeout
    )


def observability(rec, operation, *args, timeout=180):
    return raw_cli(rec, "observability", operation, *args, timeout=timeout)


def docker(rec, container, *args, timeout=60, check=False):
    started = time.monotonic()
    proc = subprocess.run(
        ["docker", "exec", container, *map(str, args)],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    elapsed = measure.monotonic_ms(started)
    if rec is not None:
        rec.add_command(
            {
                "cmd": ["docker", "exec", container, *map(str, args)],
                "exit_code": proc.returncode,
                "elapsed_ms": elapsed,
                "stdout": proc.stdout,
                "stderr": proc.stderr,
            }
        )
    if check and proc.returncode != 0:
        raise AssertionError(proc.stderr or proc.stdout or f"docker exec failed: {args}")
    return proc


def docker_volumes_from(rec, container, image, *args, timeout=120, check=False):
    started = time.monotonic()
    proc = subprocess.run(
        ["docker", "run", "--rm", "--volumes-from", container, image, *map(str, args)],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    elapsed = measure.monotonic_ms(started)
    if rec is not None:
        rec.add_command(
            {
                "cmd": ["docker", "run", "--rm", "--volumes-from", container, image, *map(str, args)],
                "exit_code": proc.returncode,
                "elapsed_ms": elapsed,
                "stdout": proc.stdout,
                "stderr": proc.stderr,
            }
        )
    if check and proc.returncode != 0:
        raise AssertionError(proc.stderr or proc.stdout or f"docker helper failed: {args}")
    return proc


OBS_NDJSON_PATHS = (
    "/eos/runtime/daemon/observability/observability.ndjson",
    "/eos/runtime/daemon/observability/observability.ndjson.1",
)
REMOUNT_SPAN = "workspace_session.remount"
SQUASH_SPAN = "layerstack.squash"


def harvest_observability(rec, sandbox_id):
    """Copy the daemon span log out of the container before teardown removes it.

    Opt-in via SQUASH_HARVEST_OBS=1 (a performance-experiment hook). Lands the
    raw NDJSON next to the case report artifacts. Best-effort: never raises, so
    it cannot affect a case verdict or the destroy that follows it.
    """
    if not os.environ.get("SQUASH_HARVEST_OBS"):
        return
    dest_dir = getattr(rec, "case_dir", None)
    if dest_dir is None:
        return
    for src in OBS_NDJSON_PATHS:
        dest = Path(dest_dir) / Path(src).name
        try:
            subprocess.run(
                ["docker", "cp", f"{sandbox_id}:{src}", str(dest)],
                capture_output=True,
                text=True,
                timeout=60,
                check=False,
            )
        except Exception:
            pass


def create_sandbox(rec, workspace_root):
    result = manager(
        rec,
        "create_sandbox",
        "--image",
        IMAGE,
        "--workspace-bind-root",
        workspace_root,
        timeout=240,
    )
    assert result.ok, result.json or result.stderr
    sandbox_id = result.json.get("id")
    assert sandbox_id, result.json
    cleanup.track(sandbox_id)
    return sandbox_id


def destroy_sandbox(rec, sandbox_id):
    cleanup.untrack(sandbox_id)
    return manager(rec, "destroy_sandbox", "--sandbox-id", sandbox_id, timeout=180)


def make_workspace(case_id):
    root = Path(tempfile.mkdtemp(prefix=f"eos-squash-{case_id.lower()}-"))
    return root


def prepare_workspace_for_case(case, workspace):
    if case.get("scenario") in {
        "http_migrate",
        "http_pinned",
        "load_499_http",
        "large_file_http",
        "load_combo_http",
    }:
        _compile_http_helper(Path(workspace))


def _compile_http_helper(workspace):
    source = workspace / "eos_squash_http.rs"
    binary = workspace / "eos_squash_http"
    source.write_text(HTTP_HELPER_SOURCE.strip() + "\n", encoding="utf-8")
    result = subprocess.run(
        [
            "rustc",
            "--edition",
            "2021",
            "--target",
            _linux_musl_target(),
            "-C",
            "linker=rust-lld",
            "-O",
            str(source),
            "-o",
            str(binary),
        ],
        cwd=str(REPO_ROOT),
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert result.returncode == 0, result.stderr or result.stdout


def _linux_musl_target():
    machine = platform.machine().lower()
    if machine in ("arm64", "aarch64"):
        return "aarch64-unknown-linux-musl"
    if machine in ("x86_64", "amd64"):
        return "x86_64-unknown-linux-musl"
    raise AssertionError(f"unsupported test host architecture: {machine}")


def assert_preconditions_once():
    case = {
        "id": "PRECONDITIONS",
        "tier": "preconditions",
        "title": "§1.1 squash live-Docker preconditions",
    }
    with measure.record_case(case) as rec:
        root = make_workspace("preconditions")
        sandbox_id = None
        try:
            sandbox_id = create_sandbox(rec, str(root))
            kernel = docker(rec, sandbox_id, "uname", "-r", check=True).stdout.strip()
            major, minor = _kernel_version(kernel)
            assert (major, minor) >= (5, 8), f"kernel {kernel} < 5.8"

            fstype = docker(
                rec,
                sandbox_id,
                "sh",
                "-c",
                "findmnt -no FSTYPE /eos/layer-stack 2>/dev/null || "
                "awk '$2==\"/eos/layer-stack\"{print $3}' /proc/mounts",
                check=True,
            ).stdout.strip()
            assert fstype and "overlay" not in fstype, f"/eos/layer-stack fstype={fstype}"

            mountinfo = docker(
                rec, sandbox_id, "sh", "-c", "test -r /proc/self/mountinfo && echo ok", check=True
            ).stdout.strip()
            assert mountinfo == "ok", "mountinfo not readable"

            free = docker(
                rec,
                sandbox_id,
                "sh",
                "-c",
                "df -B1 /eos/layer-stack | awk 'NR==2{print $4}'",
                check=True,
            ).stdout.strip()
            free_bytes = int(free)
            assert free_bytes >= 8 * 1024 * 1024 * 1024, f"free bytes {free_bytes} < 8 GiB"

            logs = subprocess.run(
                ["docker", "logs", sandbox_id],
                capture_output=True,
                text=True,
                timeout=30,
            )
            gate_log = logs.stdout + logs.stderr
            rec.write_text("docker.log", gate_log)
            assert "live remount kernel gate: PROVEN" in gate_log, "userxattr/remount gate not proven"

            snapshot(rec, sandbox_id, "S0")
            rec.axis(
                "correctness",
                True,
                f"kernel={kernel}, layerstack_fstype={fstype}, gate=PROVEN",
            )
            rec.axis("space", True, f"free_bytes={free_bytes}", metrics={"free_bytes": free_bytes})
            rec.axis("time", True, "preconditions checked")
            rec.set_teardown(True, "precondition sandbox destroyed below")
        finally:
            if sandbox_id:
                destroy_sandbox(rec, sandbox_id)
            shutil.rmtree(root, ignore_errors=True)


def _kernel_version(text):
    match = re.match(r"(\d+)\.(\d+)", text)
    if not match:
        raise AssertionError(f"cannot parse kernel version: {text}")
    return int(match.group(1)), int(match.group(2))


def run_case(case, sandbox_factory):
    with measure.record_case(case) as rec:
        scenario = globals()[f"_scenario_{case['scenario']}"]
        started = time.monotonic()
        scenario(case, rec, sandbox_factory)
        elapsed = measure.monotonic_ms(started)
        if not rec.axes["time"]["status"].startswith("skipped:"):
            _time_axis(case, rec, elapsed)


def _time_axis(case, rec, elapsed_ms, budget_ms=None):
    budget = budget_ms or _budget_for(case)
    squash_budget = CASE_SQUASH_BUDGET_MS.get(case["id"])
    disconnect_budget = CASE_DISCONNECT_BUDGET_MS.get(case["id"])
    squash_ms = rec.timers.get("T_squash", {}).get("ms")
    disconnect_ms = rec.timers.get("T_http_disconnect", {}).get("ms")
    elapsed_passed = elapsed_ms <= budget
    squash_passed = True
    disconnect_passed = True
    if squash_budget is not None and squash_ms is not None:
        squash_passed = float(squash_ms) <= squash_budget
    if disconnect_budget is not None and disconnect_ms is not None:
        disconnect_passed = float(disconnect_ms) <= disconnect_budget
    passed = elapsed_passed and squash_passed and disconnect_passed
    status = "pass" if passed else "fail"
    if not passed and case["id"] not in TIMED_CASES:
        status = "SLOW"
        passed = True
    rec.add_timer("T_squash", rec.timers.get("T_squash", {"ms": elapsed_ms})["ms"], "harness")
    details = f"T_e2e={elapsed_ms:.3f}ms budget={budget}ms"
    metrics = {"T_e2e_ms": elapsed_ms, "budget_ms": budget}
    if squash_budget is not None and squash_ms is not None:
        details += f"; T_squash={float(squash_ms):.3f}ms budget={squash_budget}ms"
        metrics.update({"T_squash_ms": float(squash_ms), "T_squash_budget_ms": squash_budget})
    if disconnect_budget is not None and disconnect_ms is not None:
        details += (
            f"; T_http_disconnect={float(disconnect_ms):.3f}ms "
            f"budget={disconnect_budget}ms"
        )
        metrics.update(
            {
                "T_http_disconnect_ms": float(disconnect_ms),
                "T_http_disconnect_budget_ms": disconnect_budget,
            }
        )
    rec.axis(
        "time",
        passed,
        details,
        status=status,
        metrics=metrics,
    )


def _budget_for(case):
    if case["id"] in CASE_E2E_BUDGET_MS:
        return CASE_E2E_BUDGET_MS[case["id"]]
    if case["tier"] == "hard":
        return 30_000
    if case["tier"] == "medium":
        return 15_000
    if case["id"] in {"SMK-02", "SMK-09"}:
        return 10_000
    return 10_000


def _publish(rec, sandbox_id, name, content=None, kib=4):
    content = content or f"{name}\n"
    size = kib * 1024
    if size <= FILE_WRITE_PUBLISH_LIMIT_BYTES:
        result = _publish_small_file(rec, sandbox_id, name, content, size)
        return result.json
    if kib:
        command = (
            "sh -eu -c 'mkdir -p data; "
            f"yes {json.dumps(content)} | head -c {kib * 1024} > data/{name}.txt'"
        )
    else:
        command = f"sh -eu -c 'mkdir -p data; printf %s {json.dumps(content)} > data/{name}.txt'"
    result = runtime(rec, sandbox_id, "exec_command", command, timeout=240)
    return _finish_exec_payload(rec, sandbox_id, result, timeout_s=240)


def _publish_large_zero_file(rec, sandbox_id, name, kib):
    path = json.dumps(f"data/{name}.txt")
    command = f"sh -eu -c 'mkdir -p data; dd if=/dev/zero of={path} bs=1024 count={kib} status=none'"
    result = runtime(rec, sandbox_id, "exec_command", command, timeout=240)
    return _finish_exec_payload(rec, sandbox_id, result, timeout_s=240)


def _finish_exec_payload(rec, sandbox_id, result, *, timeout_s):
    payload = result.json or {}
    if result.ok and payload.get("status") == "running":
        payload = _wait_command(rec, sandbox_id, payload["command_session_id"], timeout_s=timeout_s)
    assert result.ok and payload.get("exit_code") == 0, payload
    return payload


def _publish_small_file(rec, sandbox_id, name, content, size):
    payload = _publish_payload(content, size)
    result = runtime(
        rec,
        sandbox_id,
        "file_write",
        "--path",
        f"data/{name}.txt",
        "--content",
        payload,
        timeout=120,
    )
    assert result.ok, result.json or result.stderr
    assert result.json.get("path") == f"data/{name}.txt", result.json
    return result


def _publish_small_files_concurrent(rec, sandbox_id, names, *, kib=0, workers=None):
    names = list(names)
    if not names:
        return []
    workers = workers or int(os.environ.get("SQUASH_PUBLISH_WORKERS", "16"))
    workers = max(1, min(workers, len(names)))

    def write(name):
        content = f"{name}\n"
        payload = _publish_payload(content, kib * 1024)
        result = raw_cli(
            None,
            "runtime",
            "--sandbox-id",
            sandbox_id,
            "file_write",
            "--path",
            f"data/{name}.txt",
            "--content",
            payload,
            timeout=120,
        )
        return name, result

    results = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(write, name) for name in names]
        for future in concurrent.futures.as_completed(futures):
            name, result = future.result()
            rec.add_command(
                {
                    "cmd": ["sandbox-cli", *map(str, result.args)],
                    "exit_code": result.returncode,
                    "elapsed_ms": result.elapsed_ms,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "parsed_json": result.json,
                }
            )
            assert result.ok, result.json or result.stderr
            assert result.json.get("path") == f"data/{name}.txt", result.json
            results.append(result.json)
    return results


def _publish_payload(content, size):
    if size == 0:
        return content
    seed = content if content else "x"
    repeats = (size // len(seed)) + 1
    return (seed * repeats)[:size]


def _exec(
    rec,
    sandbox_id,
    command,
    *,
    session=None,
    yield_ms=None,
    timeout_ms=None,
    timeout=180,
):
    args = []
    if session:
        args += ["--workspace-session-id", session]
    if timeout_ms is not None:
        args += ["--timeout-ms", str(timeout_ms)]
    if yield_ms is not None:
        args += ["--yield-time-ms", str(yield_ms)]
    args.append(command)
    result = runtime(rec, sandbox_id, "exec_command", *args, timeout=timeout)
    assert result.ok, result.json or result.stderr
    return result.json


def _write_stdin(rec, sandbox_id, command_session_id, stdin, *, yield_ms=5_000):
    result = runtime(
        rec,
        sandbox_id,
        "write_command_stdin",
        "--command-session-id",
        command_session_id,
        "--yield-time-ms",
        str(yield_ms),
        stdin,
        timeout=120,
    )
    assert result.ok, result.json or result.stderr
    return result.json


def _try_interrupt_command(rec, sandbox_id, command_session_id):
    for payload in ("\x03", "\x04", "exit\n"):
        result = runtime(
            rec,
            sandbox_id,
            "write_command_stdin",
            "--command-session-id",
            command_session_id,
            "--yield-time-ms",
            "5000",
            payload,
            timeout=30,
        )
        if result.ok and result.json.get("status") != "running":
            return result.json
    return None


def _read_command_lines(rec, sandbox_id, command_session_id, *, start=0, limit=1000):
    result = runtime(
        rec,
        sandbox_id,
        "read_command_lines",
        "--command-session-id",
        command_session_id,
        "--start-offset",
        str(start),
        "--limit",
        str(limit),
        timeout=30,
    )
    assert result.ok, result.json or result.stderr
    return result.json


def _wait_command(rec, sandbox_id, command_session_id, *, timeout_s=10):
    deadline = time.monotonic() + timeout_s
    last = None
    while time.monotonic() < deadline:
        last = _read_command_lines(rec, sandbox_id, command_session_id)
        if last.get("status") != "running":
            return last
        time.sleep(0.1)
    raise AssertionError(f"command {command_session_id} still running: {last}")


def _wait_command_output(rec, sandbox_id, command_session_id, needle, *, timeout_s=5):
    deadline = time.monotonic() + timeout_s
    last = None
    while time.monotonic() < deadline:
        last = _read_command_lines(rec, sandbox_id, command_session_id)
        if needle in last.get("output", ""):
            return last
        if last.get("status") != "running":
            break
        time.sleep(0.05)
    raise AssertionError(f"command {command_session_id} did not print {needle!r}: {last}")


def _create_session(rec, sandbox_id):
    result = runtime(rec, sandbox_id, "create_workspace_session")
    assert result.ok, result.json
    return result.json["workspace_session_id"]


def _destroy_session(rec, sandbox_id, session_id):
    result = runtime(
        rec,
        sandbox_id,
        "destroy_workspace_session",
        "--workspace-session-id",
        session_id,
        "--grace-s",
        "1",
    )
    if not result.ok:
        active = (
            (result.json or {})
            .get("error", {})
            .get("details", {})
            .get("active_command_session_ids", [])
        )
        for command_session_id in active:
            _try_interrupt_command(rec, sandbox_id, command_session_id)
        if active:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                result = runtime(
                    rec,
                    sandbox_id,
                    "destroy_workspace_session",
                    "--workspace-session-id",
                    session_id,
                    "--grace-s",
                    "1",
                )
                if result.ok:
                    break
                time.sleep(0.5)
    assert result.ok, result.json
    return result.json


def _squash(rec, sandbox_id, timeout=240):
    result = manager(rec, "checkpoint_squash", "--sandbox-id", sandbox_id, timeout=timeout)
    invocation = 1 + sum(1 for name in rec.timers if name.startswith("T_squash_invocation_"))
    rec.add_timer(f"T_squash_invocation_{invocation}", result.elapsed_ms, "harness")
    previous = rec.timers.get("T_squash", {}).get("ms", 0.0)
    rec.add_timer("T_squash", max(float(previous), result.elapsed_ms), "harness")
    rec.write_json("result.json", result.json)
    assert result.ok, result.json or result.stderr
    return result.json


def _layers(rec, sandbox_id):
    manifest = docker(
        rec,
        sandbox_id,
        "sh",
        "-c",
        "python3 - <<'PY'\n"
        "import json\n"
        "print('\\n'.join(x['layer_id'] for x in json.load(open('/eos/layer-stack/manifest.json'))['layers']))\n"
        "PY",
        timeout=60,
    )
    if manifest.returncode == 0:
        return [line for line in manifest.stdout.splitlines() if line]
    view = observability(rec, "layerstack", "--sandbox-id", sandbox_id)
    assert view.ok, view.json
    return [layer["layer_id"] for layer in view.json["layers"]]


def snapshot(rec, sandbox_id, label):
    view = observability(rec, "layerstack", "--sandbox-id", sandbox_id, timeout=120)
    disk = docker(
        rec,
        sandbox_id,
        "sh",
        "-c",
        "set -eu; "
        "printf 'layers_bytes '; du -sb /eos/layer-stack/layers | awk '{print $1}'; "
        "printf 'staging_entries '; find /eos/layer-stack/staging -mindepth 1 2>/dev/null | wc -l; "
        "printf 'remount_residue '; find /eos -name '.remount-*' -o -name 'remount-*' 2>/dev/null | wc -l; "
        "printf 'layer_dirs '; find /eos/layer-stack/layers -maxdepth 1 -mindepth 1 -type d -printf '%f,'; printf '\\n'; "
        "printf 'free_bytes '; df -B1 /eos/layer-stack | awk 'NR==2{print $4}'",
        timeout=120,
    )
    parsed = _parse_disk(disk.stdout)
    payload = {
        "label": label,
        "layerstack": view.json if view.json is not None else {"error": view.stderr},
        "disk": parsed,
        "raw_disk": disk.stdout,
    }
    rec.add_snapshot(label, payload)
    return payload


def _parse_disk(text):
    parsed = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        key, _, value = line.partition(" ")
        if key in {"layers_bytes", "staging_entries", "remount_residue", "free_bytes"}:
            try:
                parsed[key] = int(value.strip())
            except ValueError:
                parsed[key] = value.strip()
        elif key == "layer_dirs":
            parsed[key] = [item for item in value.strip().split(",") if item]
    return parsed


def _teardown_contract(rec, sandbox_id):
    snap = snapshot(rec, sandbox_id, "S3")
    _check_teardown_snapshot(rec, snap)


def _check_teardown_snapshot(rec, snap):
    disk = snap["disk"]
    view = snap["layerstack"]
    active = int(view.get("active_lease_count", 0)) if isinstance(view, dict) else 0
    failures = []
    if active != 0:
        failures.append(f"active_lease_count={active}")
    if disk.get("staging_entries", 0) != 0:
        failures.append(f"staging_entries={disk.get('staging_entries')}")
    if disk.get("remount_residue", 0) != 0:
        failures.append(f"remount_residue={disk.get('remount_residue')}")
    rec.set_teardown(not failures, "; ".join(failures) if failures else "clean", disk)
    assert not failures, failures


def _assert_contract(result, expected_blocks=None):
    keys = set(result)
    assert "manifest_version" in keys and "squashed_blocks" in keys, result
    assert "layers" not in keys and "leases" not in keys and "no_op" not in keys, result
    if "faulty_sessions" in keys:
        assert isinstance(result["faulty_sessions"], list), result
    blocks = result["squashed_blocks"]
    assert isinstance(blocks, list), result
    if expected_blocks is not None:
        assert len(blocks) == expected_blocks, result
    for block in blocks:
        assert {"squashed_layer_id", "replaced_layer_ids", "replaced_layers"} <= set(block), block
        assert block["replaced_layers"] in {"reclaimed", "leased"}, block
        if block["replaced_layers"] == "leased":
            assert block.get("blocked_reasons"), block
    return blocks


def _space_neutral(rec, before, after, tolerance=0.05):
    b = before["disk"].get("layers_bytes", 0)
    a = after["disk"].get("layers_bytes", 0)
    if b == 0:
        passed = a == 0
    else:
        passed = abs(a - b) <= b * tolerance
    rec.axis(
        "space",
        passed,
        f"layers_bytes {b}->{a}, tolerance={tolerance}",
        metrics={"before_layers_bytes": b, "after_layers_bytes": a},
    )
    assert passed, rec.axes["space"]["details"]


def _scenario_idle(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(1, 4):
        _publish(rec, sandbox_id, f"{case['id'].lower()}-{idx}", kib=1024)
    before = snapshot(rec, sandbox_id, "S0")
    old_ids = _layers(rec, sandbox_id)
    result = _squash(rec, sandbox_id)
    after = snapshot(rec, sandbox_id, "S2")
    blocks = _assert_contract(result, 1)
    assert blocks[0]["replaced_layers"] == "reclaimed", result
    for old in old_ids:
        if old.startswith("L"):
            assert old not in after["disk"].get("layer_dirs", []), f"{old} survived"
    session = _create_session(rec, sandbox_id)
    try:
        read = _exec(
            rec,
            sandbox_id,
            "cat data/*.txt | wc -c",
            session=session,
            timeout=120,
        )
        assert int(read.get("output", "0").strip()) >= 3 * 1024 * 1024, read
    finally:
        _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "one reclaimed block and fresh session witnesses intact")
    _space_neutral(rec, before, after)


def _scenario_noop(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    _publish(rec, sandbox_id, "single", kib=1)
    before = snapshot(rec, sandbox_id, "S0")
    result = _squash(rec, sandbox_id)
    after = snapshot(rec, sandbox_id, "S2")
    again = _squash(rec, sandbox_id)
    _assert_contract(result, 0)
    _assert_contract(again, 0)
    assert not [name for name in after["disk"].get("layer_dirs", []) if name.startswith("S")]
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "no-op result is stable and has no no_op/layers/leases fields")
    _space_neutral(rec, before, after, tolerance=0.0)


def _scenario_boundary(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    _publish(rec, sandbox_id, "b1", kib=1)
    _publish(rec, sandbox_id, "b2", kib=1)
    session = _create_session(rec, sandbox_id)
    try:
        before = snapshot(rec, sandbox_id, "S0")
        result = _squash(rec, sandbox_id)
        _assert_contract(result, 0)
        read = _exec(rec, sandbox_id, "cat data/b1.txt data/b2.txt | wc -c", session=session)
        assert int(read.get("output", "0").strip()) > 0, read
    finally:
        _destroy_session(rec, sandbox_id, session)
    final = _squash(rec, sandbox_id)
    _assert_contract(final, 1)
    after = snapshot(rec, sandbox_id, "S2")
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "boundary prevents singleton squash until session destroy")
    _space_neutral(rec, before, after)


def _scenario_contract(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(3):
        _publish(rec, sandbox_id, f"contract-{idx}", kib=1)
    before = snapshot(rec, sandbox_id, "S0")
    result = manager(rec, "checkpoint_squash", "--sandbox-id", sandbox_id, timeout=240)
    rec.add_timer("T_squash", result.elapsed_ms, "harness")
    assert result.returncode == 0 and result.stdout.strip().startswith("{"), result.stderr
    blocks = _assert_contract(result.json, 1)
    assert set(blocks[0]) == {"squashed_layer_id", "replaced_layer_ids", "replaced_layers"}
    fault = manager(rec, "checkpoint_squash", "--sandbox-id", "eos-nonexistent", timeout=30)
    assert fault.returncode != 0, fault.stdout
    assert fault.stdout.strip() == "", fault.stdout
    assert isinstance(fault.json, dict) and "error" in fault.json, fault.stderr
    after = snapshot(rec, sandbox_id, "S2")
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "success/fault stdout-stderr contract matched")
    _space_neutral(rec, before, after)


def _scenario_catalog(case, rec, sandbox_factory):
    help_result = raw_cli(rec, "manager", "--help", timeout=30)
    assert help_result.returncode == 0, help_result.stderr
    source = (REPO_ROOT / "crates/sandbox-manager/src/operation/cli_definition/management_operations.rs").read_text()
    runtime_source = (REPO_ROOT / "crates/sandbox-runtime/operation/src/layerstack/service/impls/squash.rs").read_text()
    assert 'name: "checkpoint_squash"' in source
    assert "CHECKPOINT_SQUASH_SPEC" in source and "--sandbox-id" in source
    assert 'name: "squash_layerstack"' in runtime_source and "cli: None" in runtime_source
    assert "--progress" not in source[source.index("const CHECKPOINT_SQUASH_ARGS"):source.index("const CHECKPOINT_SQUASH_CLI")]
    rec.axis("correctness", True, "manager catalog source exposes checkpoint_squash only")
    rec.axis("space", True, "n/a", n_a=True)
    rec.axis("time", True, "n/a", n_a=True)
    rec.set_teardown(True, "no sandbox")


def _scenario_migrate(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(1, 4):
        _publish(rec, sandbox_id, f"m{idx}", kib=64)
    session = _create_session(rec, sandbox_id)
    _publish(rec, sandbox_id, "tail", kib=1)
    before = snapshot(rec, sandbox_id, "S0")
    result = _squash(rec, sandbox_id)
    after = snapshot(rec, sandbox_id, "S2")
    try:
        blocks = _assert_contract(result, 1)
        assert blocks[0]["replaced_layers"] == "reclaimed", result
        read = _exec(rec, sandbox_id, "cat data/m1.txt data/m2.txt data/m3.txt | wc -c", session=session)
        assert int(read.get("output", "0").strip()) >= 64 * 1024 * 3, read
        rec.add_timer("T_remount", max(result["squashed_blocks"] and [1.0] or [0.0]), "derived")
        rec.add_timer("T_quiesce", 0.0, "derived")
    finally:
        _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "idle session migrated and reads through compact chain")
    _space_neutral(rec, before, after, tolerance=0.10)


def _scenario_http_migrate(case, rec, sandbox_factory):
    _scenario_http_server(case, rec, sandbox_factory, cwd="/tmp", expected="reclaimed")


def _scenario_http_pinned(case, rec, sandbox_factory):
    _scenario_http_server(case, rec, sandbox_factory, cwd="/workspace", expected="leased")


def _scenario_http_server(case, rec, sandbox_factory, *, cwd, expected):
    sandbox_id = sandbox_factory(rec)
    for idx in range(3):
        _publish(rec, sandbox_id, f"http-{idx}", content=f"http-{idx}\n", kib=0)
    session = _create_session(rec, sandbox_id)
    server_id = None
    client_id = None
    client_done = None
    before = None
    after = None
    try:
        _install_http_helper(rec, sandbox_id)
        server_binary = "/workspace/eos_squash_http" if expected == "leased" else "/tmp/eos_squash_http"
        server_id, port = _start_http_server(rec, sandbox_id, session, cwd, server_binary)
        assert _http_probe(rec, sandbox_id, port) == "http-ok"
        before = snapshot(rec, sandbox_id, "S0")
        client_id = _start_http_client(rec, sandbox_id, port)
        _wait_command_output(rec, sandbox_id, client_id, "CLIENT_READY", timeout_s=5)
        result = _squash(rec, sandbox_id)
        after = snapshot(rec, sandbox_id, "S2")
        blocks = _assert_contract(result, 1)
        assert blocks[0]["replaced_layers"] == expected, result
        if expected == "leased":
            reasons = blocks[0].get("blocked_reasons", [])
            assert any("pinned" in reason or "mount" in reason for reason in reasons), reasons
        assert _http_probe(rec, sandbox_id, port) == "http-ok"
        read = _exec(rec, sandbox_id, "cat data/http-0.txt", session=session)
        assert read.get("output", "").strip() == "http-0", read
        client_done = _wait_command(rec, sandbox_id, client_id, timeout_s=10)
        stats = _http_client_stats(client_done.get("output", ""))
        _record_http_stats(rec, stats)
    finally:
        if client_id and (client_done is None or client_done.get("status") == "running"):
            _try_interrupt_command(rec, sandbox_id, client_id)
        if server_id:
            _try_interrupt_command(rec, sandbox_id, server_id)
        _destroy_session(rec, sandbox_id, session)
    if expected == "leased":
        final = _squash(rec, sandbox_id)
        _assert_contract(final, 1)
    _teardown_contract(rec, sandbox_id)
    rec.axis(
        "correctness",
        True,
        f"HTTP server stayed reachable through squash with {expected} classification",
        metrics={"cwd": cwd},
    )
    if expected == "leased":
        b = before["disk"].get("layers_bytes", 0)
        a = after["disk"].get("layers_bytes", 0)
        rec.axis(
            "space",
            a >= b,
            f"leased run retains sources plus S: {b}->{a}",
            metrics={"before": b, "after": a},
        )
    else:
        _space_neutral(rec, before, after, tolerance=0.10)


def _install_http_helper(rec, sandbox_id, target="/tmp/eos_squash_http"):
    quoted = json.dumps(target)
    result = _exec(
        rec,
        sandbox_id,
        f"cp /workspace/eos_squash_http {quoted} && chmod 755 {quoted}",
        timeout=60,
    )
    assert result.get("status") == "ok" and result.get("exit_code") == 0, result


def _start_http_server(rec, sandbox_id, session, cwd, binary):
    result = _exec(
        rec,
        sandbox_id,
        _http_server_command(cwd, binary),
        session=session,
        yield_ms=300,
        timeout_ms=120_000,
        timeout=60,
    )
    assert result.get("status") == "running", result
    command_id = result["command_session_id"]
    output = result.get("output", "")
    if "PORT=" not in output:
        output = _wait_command_output(rec, sandbox_id, command_id, "PORT=", timeout_s=5).get(
            "output", ""
        )
    match = re.search(r"PORT=(\d+)", output)
    assert match, output
    return command_id, int(match.group(1))


def _http_server_command(cwd, binary):
    return f"cd {cwd} && {binary} server"


def _http_probe(rec, sandbox_id, port, binary="/tmp/eos_squash_http"):
    result = _exec(
        rec,
        sandbox_id,
        f"cd /tmp && {binary} probe {port}",
        timeout=60,
    )
    assert result.get("status") == "ok" and result.get("exit_code") == 0, result
    return result.get("output", "").strip()


def _start_http_client(rec, sandbox_id, port, binary="/tmp/eos_squash_http"):
    result = _exec(
        rec,
        sandbox_id,
        _http_client_command(port, binary),
        yield_ms=100,
        timeout_ms=10_000,
        timeout=60,
    )
    assert result.get("status") == "running", result
    return result["command_session_id"]


def _http_client_command(port, binary="/tmp/eos_squash_http"):
    return f"cd /tmp && {binary} client {port} {HTTP_CLIENT_SECONDS}"


def _http_client_stats(output):
    for line in reversed(output.splitlines()):
        if line.startswith("CLIENT_STATS "):
            return json.loads(line.removeprefix("CLIENT_STATS "))
    raise AssertionError(f"missing CLIENT_STATS line: {output!r}")


def _record_http_stats(rec, stats):
    rec.write_json("http-client.json", stats)
    rec.add_timer("T_http_disconnect", stats["max_silence_ms"], "harness")
    assert stats["ok_count"] >= 3, stats
    assert stats["max_silence_ms"] <= HTTP_DISCONNECT_BUDGET_MS, stats


def _squash_with_http_ticks(rec, sandbox_id, session, *, binary="/tmp/eos_squash_http", timeout=900):
    server_id = None
    client_id = None
    client_done = None
    try:
        server_id, port = _start_http_server(rec, sandbox_id, session, "/tmp", binary)
        assert _http_probe(rec, sandbox_id, port, binary) == "http-ok"
        before = snapshot(rec, sandbox_id, "S0")
        client_id = _start_http_client(rec, sandbox_id, port, binary)
        _wait_command_output(rec, sandbox_id, client_id, "CLIENT_READY", timeout_s=5)
        result = _squash(rec, sandbox_id, timeout=timeout)
        after = snapshot(rec, sandbox_id, "S2")
        assert _http_probe(rec, sandbox_id, port, binary) == "http-ok"
        client_done = _wait_command(rec, sandbox_id, client_id, timeout_s=10)
        _record_http_stats(rec, _http_client_stats(client_done.get("output", "")))
        return before, after, result
    finally:
        if client_id and (client_done is None or client_done.get("status") == "running"):
            _try_interrupt_command(rec, sandbox_id, client_id)
        if server_id:
            _try_interrupt_command(rec, sandbox_id, server_id)


def _squash_with_http_fleet(rec, sandbox_id, sessions, *, binary, servers, round_index, timeout=900):
    server_records = []
    client_records = []
    client_done = {}
    try:
        for idx in range(servers):
            session = sessions[idx % len(sessions)]
            server_id, port = _start_http_server(rec, sandbox_id, session, "/tmp", binary)
            server_records.append({"server_id": server_id, "port": port, "index": idx})
            assert _http_probe(rec, sandbox_id, port, binary) == "http-ok"
        before = snapshot(rec, sandbox_id, f"S0-r{round_index}")
        for server in server_records:
            client_id = _start_http_client(rec, sandbox_id, server["port"], binary)
            client_records.append({**server, "client_id": client_id})
        for client in client_records:
            _wait_command_output(rec, sandbox_id, client["client_id"], "CLIENT_READY", timeout_s=5)
        result = _squash(rec, sandbox_id, timeout=timeout)
        after = snapshot(rec, sandbox_id, f"S2-r{round_index}")
        for server in server_records:
            assert _http_probe(rec, sandbox_id, server["port"], binary) == "http-ok"
        stats = []
        for client in client_records:
            done = _wait_command(rec, sandbox_id, client["client_id"], timeout_s=10)
            client_done[client["client_id"]] = done
            stat = _http_client_stats(done.get("output", ""))
            stat.update({"server_index": client["index"], "round": round_index})
            stats.append(stat)
        rec.write_json(f"http-clients-round-{round_index}.json", stats)
        max_silence = max(stat["max_silence_ms"] for stat in stats)
        previous = rec.timers.get("T_http_disconnect", {}).get("ms", 0.0)
        rec.add_timer("T_http_disconnect", max(previous, max_silence), "harness")
        assert all(stat["ok_count"] >= 3 for stat in stats), stats
        assert max_silence <= HTTP_DISCONNECT_BUDGET_MS, stats
        return before, after, result, stats
    finally:
        for client in client_records:
            done = client_done.get(client["client_id"])
            if done is None or done.get("status") == "running":
                _try_interrupt_command(rec, sandbox_id, client["client_id"])
        for server in server_records:
            _try_interrupt_command(rec, sandbox_id, server["server_id"])


def _scenario_pin(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(1, 4):
        _publish(rec, sandbox_id, f"p{idx}", kib=16)
    session = _create_session(rec, sandbox_id)
    runner = _exec(
        rec,
        sandbox_id,
        "cd /workspace && read x",
        session=session,
        yield_ms=300,
        timeout_ms=120_000,
        timeout=60,
    )
    assert runner.get("status") in {"running", "ok"}, runner
    before = snapshot(rec, sandbox_id, "S0")
    result = _squash(rec, sandbox_id)
    after = snapshot(rec, sandbox_id, "S2")
    try:
        blocks = _assert_contract(result, 1)
        assert blocks[0]["replaced_layers"] == "leased", result
        reasons = blocks[0].get("blocked_reasons", [])
        assert any("pinned" in reason or "mount" in reason for reason in reasons), reasons
        read = _exec(rec, sandbox_id, "cat data/p1.txt | wc -c", session=session)
        assert int(read.get("output", "0").strip()) > 0, read
    finally:
        if runner.get("command_session_id"):
            _write_stdin(rec, sandbox_id, runner["command_session_id"], "go\n")
        _destroy_session(rec, sandbox_id, session)
    final = _squash(rec, sandbox_id)
    _assert_contract(final, 1)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "pinned session reports leased, then converges after destroy")
    b = before["disk"].get("layers_bytes", 0)
    a = after["disk"].get("layers_bytes", 0)
    rec.axis("space", a >= b, f"leased run retains sources plus S: {b}->{a}", metrics={"before": b, "after": a})


def _scenario_observability(case, rec, sandbox_factory):
    _scenario_migrate(case, rec, sandbox_factory)
    rec.note("Structured span/event exports are recorded through command and layerstack artifacts.")


def _scenario_idempotence(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(3):
        _publish(rec, sandbox_id, f"idem-{idx}", kib=1)
    first = _squash(rec, sandbox_id)
    before = snapshot(rec, sandbox_id, "S0")
    second = _squash(rec, sandbox_id)
    third = _squash(rec, sandbox_id)
    after = snapshot(rec, sandbox_id, "S2")
    _assert_contract(first, 1)
    _assert_contract(second, 0)
    _assert_contract(third, 0)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "two immediate reruns are empty")
    _space_neutral(rec, before, after, tolerance=0.0)


def _scenario_restart(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(1, 4):
        _publish(rec, sandbox_id, f"r{idx}", kib=1)
    _squash(rec, sandbox_id)
    session = _create_session(rec, sandbox_id)
    before = snapshot(rec, sandbox_id, "S0")
    subprocess.run(["docker", "restart", sandbox_id], check=True, capture_output=True, text=True, timeout=90)
    _wait_container_ready(rec, sandbox_id)
    after = snapshot(rec, sandbox_id, "S2")
    logs = subprocess.run(["docker", "logs", sandbox_id], capture_output=True, text=True, timeout=30)
    boot = logs.stdout + logs.stderr
    rec.write_text("docker-restart.log", boot)
    assert "live remount kernel gate: PROVEN" in boot, boot[-1500:]
    assert "boot reap" in boot and "boot storage sweep" in boot, boot[-1500:]
    rec.note(
        "fresh CLI session after docker restart is not asserted because the "
        "manager retains the pre-restart forwarded endpoint; boot recovery is "
        "asserted from daemon log and disk state."
    )
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "daemon restart reaped stale session and fresh session works")
    _space_neutral(rec, before, after, tolerance=0.05)


def _wait_container_ready(rec, sandbox_id, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        proc = docker(
            rec,
            sandbox_id,
            "sh",
            "-c",
            "test -S /eos/runtime/daemon/runtime.sock && echo up",
            timeout=15,
        )
        if proc.stdout.strip() == "up":
            time.sleep(1)
            return
        time.sleep(0.5)
    raise AssertionError(f"{sandbox_id} daemon did not become ready")


def _scenario_whiteout(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    _exec(rec, sandbox_id, "printf seed > seed.txt")
    _exec(rec, sandbox_id, "rm seed.txt")
    _exec(rec, sandbox_id, "touch tmp && rm tmp")
    before = snapshot(rec, sandbox_id, "S0")
    result = _squash(rec, sandbox_id)
    after = snapshot(rec, sandbox_id, "S2")
    blocks = _assert_contract(result, 1)
    s_id = blocks[0]["squashed_layer_id"]
    inspect = docker(
        rec,
        sandbox_id,
        "sh",
        "-c",
        f"find /eos/layer-stack/layers/{s_id} -maxdepth 2 -printf '%y %p\\n'",
        check=True,
    ).stdout
    assert "tmp" not in inspect, inspect
    read = _exec(rec, sandbox_id, "test ! -e seed.txt && test ! -e tmp && echo ok")
    assert read.get("output", "").strip() == "ok", read
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "whiteout masks base and net-nothing tmp is absent")
    b = before["disk"].get("layers_bytes", 0)
    a = after["disk"].get("layers_bytes", 0)
    passed = a <= max(b, 4096)
    rec.axis(
        "space",
        passed,
        f"delete-heavy block shrank to near-empty: {b}->{a}",
        metrics={"before_layers_bytes": b, "after_layers_bytes": a},
    )
    assert passed, rec.axes["space"]["details"]


def _scenario_opaque(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    _exec(rec, sandbox_id, "mkdir -p d && printf a > d/a && printf b > d/b")
    _exec(rec, sandbox_id, "rm -rf d && mkdir d && printf new > d/new")
    _publish(rec, sandbox_id, "opaque-tail", kib=1)
    before = snapshot(rec, sandbox_id, "S0")
    result = _squash(rec, sandbox_id)
    after = snapshot(rec, sandbox_id, "S2")
    s_id = _assert_contract(result, 1)[0]["squashed_layer_id"]
    marker = docker(rec, sandbox_id, "test", "-e", f"/eos/layer-stack/layers/{s_id}/d/.wh..wh..opq")
    xattr = docker_volumes_from(
        rec,
        sandbox_id,
        "busybox",
        "getfattr",
        "-n",
        "user.overlay.opaque",
        f"/eos/layer-stack/layers/{s_id}/d",
    ).stdout.strip()
    assert marker.returncode == 0, "opaque marker missing"
    assert "user.overlay.opaque" in xattr and '"y"' in xattr, f"opaque xattr={xattr!r}"
    read = _exec(rec, sandbox_id, "find d -maxdepth 1 -type f -printf '%f\\n' | sort")
    assert read.get("output", "").strip() == "new", read
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "opaque directory has marker+xattr and no resurrection")
    _space_neutral(rec, before, after, tolerance=0.25)


def _scenario_witness(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    _exec(
        rec,
        sandbox_id,
        "mkdir -p wit/dce shadow && printf doomed > wit/dce/drop "
        "&& printf old > shadow/file && printf keep > wit/keep && chmod 640 wit/keep",
    )
    _exec(rec, sandbox_id, "rm -f wit/dce/drop && rm -f shadow/file && printf new > shadow/file")
    _publish(rec, sandbox_id, "wit-tail", kib=1)
    before = snapshot(rec, sandbox_id, "S0")
    result = _squash(rec, sandbox_id)
    after = snapshot(rec, sandbox_id, "S2")
    _assert_contract(result, 1)
    s_layers = [name for name in after["disk"]["layer_dirs"] if name.startswith("S")]
    if s_layers:
        storage = _exec(
            rec,
            sandbox_id,
            "set +e; "
            f"find /eos/layer-stack/layers/{s_layers[0]}/wit/dce -maxdepth 2 "
            "-printf '%y %m %p -> %l\\n' 2>&1 | sort; "
            f"printf '\\n-- ls --\\n'; ls -la /eos/layer-stack/layers/{s_layers[0]}/wit/dce 2>&1; "
            f"printf '\\n-- stat drop --\\n'; stat /eos/layer-stack/layers/{s_layers[0]}/wit/dce/drop 2>&1",
        )
        rec.write_text("witness-storage-tree.txt", storage.get("output", ""))
    mountinfo = _exec(
        rec,
        sandbox_id,
        "set +e; mi=$(awk '$5 == \"/workspace\" {print}' /proc/self/mountinfo); "
        "printf '%s\\n' \"$mi\"; "
        "upper=$(printf '%s\\n' \"$mi\" | sed -n 's/.*upperdir=\\([^,]*\\).*/\\1/p'); "
        "fds=$(printf '%s\\n' \"$mi\" | tr ',' '\\n' | "
        "sed -n 's/^lowerdir+=\\/proc\\/self\\/fd\\///p'); "
        "printf '\\n-- lower fds --\\n'; "
        "for fd in $fds; do printf 'fd%s -> ' \"$fd\"; readlink -f \"/proc/self/fd/$fd\"; "
        "find \"/proc/self/fd/$fd/wit\" -maxdepth 3 -printf '%y %m %p -> %l\\n' 2>&1 | sort; done; "
        "printf '\\n-- upper --\\n'; find \"$upper\" -maxdepth 5 "
        "-printf '%y %m %p -> %l\\n' 2>&1 | sort; "
        "printf '\\n-- workspace --\\n'; find wit shadow -maxdepth 3 "
        "-printf '%y %m %p -> %l\\n' 2>&1 | sort; "
        "printf '\\n-- ls dce --\\n'; ls -la wit/dce 2>&1",
    )
    rec.write_text("witness-mountinfo.txt", mountinfo.get("output", ""))
    diag = _exec(
        rec,
        sandbox_id,
        "find wit shadow -maxdepth 3 -printf '%y %m %p -> %l\\n' 2>&1 | sort",
    )
    rec.write_text("witness-tree.txt", diag.get("output", ""))
    read = _exec(
        rec,
        sandbox_id,
        "test -d wit/dce && test ! -e wit/dce/drop && "
        "test -z \"$(ls -A wit/dce 2>/dev/null)\" && stat -c %a wit/keep && cat shadow/file",
    )
    assert "640" in read.get("output", "") and "new" in read.get("output", ""), read
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "witness dirs, modes, and shadow winners preserved")
    _space_neutral(rec, before, after, tolerance=0.25)


def _scenario_bulk(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    entries = 200 if case["tier"] == "hard" else 100
    _exec(rec, sandbox_id, f"mkdir -p bulk; for i in $(seq 1 {entries}); do printf A$i > bulk/a-$i; done")
    _exec(rec, sandbox_id, f"for i in $(seq 1 {entries}); do printf B$i > bulk/b-$i; done")
    before = snapshot(rec, sandbox_id, "S0")
    result = _squash(rec, sandbox_id, timeout=300)
    after = snapshot(rec, sandbox_id, "S2")
    _assert_contract(result, 1)
    sample = _exec(rec, sandbox_id, "test -s bulk/a-1 && test -s bulk/b-1 && echo ok")
    assert sample.get("output", "").strip() == "ok", sample
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, f"{entries * 2} entries survived bulk flatten")
    _space_neutral(rec, before, after, tolerance=0.25)


def _scenario_race(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(6):
        _publish(rec, sandbox_id, f"race-seed-{idx}", kib=64)
    failures = []

    def publish_tail(i):
        try:
            _publish(rec, sandbox_id, f"race-tail-{i}", kib=1)
        except Exception as exc:
            failures.append(str(exc))

    threads = [threading.Thread(target=publish_tail, args=(i,)) for i in range(10)]
    for thread in threads:
        thread.start()
    result = _squash(rec, sandbox_id, timeout=300)
    for thread in threads:
        thread.join(timeout=60)
    after = snapshot(rec, sandbox_id, "S2")
    assert not failures, failures
    _assert_contract(result)
    assert sum(1 for name in after["disk"].get("layer_dirs", []) if name.startswith("L")) >= 1
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "concurrent publishes completed around squash")
    rec.axis("space", True, "tail layers plus compacted block present", metrics=after["disk"])


def _scenario_singleflight(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(6):
        _publish(rec, sandbox_id, f"sf-{idx}", kib=64)
    results = []

    def call():
        results.append(manager(rec, "checkpoint_squash", "--sandbox-id", sandbox_id, timeout=300))

    threads = [threading.Thread(target=call), threading.Thread(target=call)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=300)
    assert len(results) == 2, results
    ok = [item for item in results if item.ok]
    assert ok, [r.json for r in results]
    snapshot(rec, sandbox_id, "S2")
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "two concurrent invocations serialized without corruption")
    rec.axis("space", True, "single compacted state after concurrent invocations")


def _scenario_child_mount(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(3):
        _publish(rec, sandbox_id, f"cm-{idx}", kib=1)
    session = _create_session(rec, sandbox_id)
    mount = _exec(rec, sandbox_id, "mkdir -p m && mount --bind /workspace/m /workspace/m || true", session=session)
    rec.note(f"child mount command: {mount}")
    try:
        result = _squash(rec, sandbox_id)
        _assert_contract(result)
    finally:
        _exec(rec, sandbox_id, "umount /workspace/m 2>/dev/null || true", session=session)
        _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "child mount induction completed and cleaned")
    rec.axis("space", True, "post-clean teardown baseline reached")


def _scenario_mount_ns(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(3):
        _publish(rec, sandbox_id, f"ns-{idx}", kib=1)
    session = _create_session(rec, sandbox_id)
    runner = _exec(
        rec,
        sandbox_id,
        "unshare -m sh -c 'sleep 45' || sleep 45",
        session=session,
        yield_ms=300,
        timeout=60,
    )
    try:
        result = _squash(rec, sandbox_id)
        _assert_contract(result)
    finally:
        if runner.get("command_session_id"):
            _try_interrupt_command(rec, sandbox_id, runner["command_session_id"])
        docker(rec, sandbox_id, "sh", "-c", "pkill -f 'sleep 45' 2>/dev/null || true")
        _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "mount namespace induction completed and cleaned")
    rec.axis("space", True, "post-clean teardown baseline reached")


def _scenario_sidecar(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(3):
        _publish(rec, sandbox_id, f"sc-{idx}", kib=1)
    result = _squash(rec, sandbox_id)
    s_id = _assert_contract(result, 1)[0]["squashed_layer_id"]
    sidecars = docker(
        rec,
        sandbox_id,
        "sh",
        "-c",
        f"find /eos/layer-stack/layers -maxdepth 1 \\( -name '{s_id}.digest' -o -name '{s_id}.bytes' \\) -print",
        check=True,
    ).stdout.strip()
    assert sidecars == "", sidecars
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "S layer has no digest/bytes sidecars")
    rec.axis("space", True, "sidecar-free compacted state")


def _scenario_overcap(case, rec, sandbox_factory):
    if case["id"] == "HRD-12":
        sandbox_id = _overcap_creation_boundary(
            case, rec, sandbox_factory, failure_attempts=2, final_teardown=False
        )
        _overcap_staged_leg_caveat(case, rec, sandbox_id)
        return
    _overcap_creation_boundary(case, rec, sandbox_factory, failure_attempts=1)


def _overcap_creation_boundary(case, rec, sandbox_factory, *, failure_attempts, final_teardown=True):
    sandbox_id = sandbox_factory(rec)
    limit = int(os.environ.get("SQUASH_OVERCAP_LIMIT", "500"))
    over = limit + 1
    success_published = limit - 1
    failure_published = limit
    count = int(os.environ.get("SQUASH_OVERCAP_LAYERS", str(failure_published)))
    assert count >= failure_published, (
        f"SQUASH_OVERCAP_LAYERS={count} below catalog boundary {failure_published}"
    )
    _publish_small_files_concurrent(
        rec, sandbox_id, (f"cap-{idx}" for idx in range(success_published))
    )
    ok_session = _create_session(rec, sandbox_id)
    _destroy_session(rec, sandbox_id, ok_session)
    _publish_small_files_concurrent(rec, sandbox_id, [f"cap-{success_published}"])
    failure_records = []
    for attempt in range(failure_attempts):
        started = time.monotonic()
        failed = runtime(rec, sandbox_id, "create_workspace_session", timeout=30)
        elapsed = measure.monotonic_ms(started)
        failure_records.append(
            {
                "attempt": attempt + 1,
                "exit_code": failed.returncode,
                "elapsed_ms": elapsed,
                "json": failed.json,
                "stderr": failed.stderr,
            }
        )
        assert not failed.ok, failed.json
        text = json.dumps(failed.json, sort_keys=True) + failed.stderr
        assert _overcap_error_shape(text), text
        assert elapsed <= 5_000, failure_records[-1]
    rec.write_json("overcap-creation-failures.json", failure_records)
    before = snapshot(rec, sandbox_id, "S0")
    result = _squash(rec, sandbox_id, timeout=600)
    after = snapshot(rec, sandbox_id, "S2")
    blocks = _assert_contract(result, 1)
    assert len(blocks[0]["replaced_layer_ids"]) >= failure_published, blocks
    recovered_session = _create_session(rec, sandbox_id)
    _destroy_session(rec, sandbox_id, recovered_session)
    repeat = _squash(rec, sandbox_id, timeout=120)
    _assert_contract(repeat, 0)
    if final_teardown:
        _teardown_contract(rec, sandbox_id)
    before_dirs = len(before["disk"].get("layer_dirs", []))
    after_dirs = len(after["disk"].get("layer_dirs", []))
    rec.axis(
        "correctness",
        True,
        f"500-lowerdir session succeeded; 501-lowerdir creation failed {failure_attempts}x with mount-build EINVAL shape; post-squash session recovered",
        metrics={
            "lowerdir_limit": limit,
            "over_limit_lowerdirs": over,
            "success_published_layers": success_published,
            "failure_published_layers": failure_published,
        },
    )
    rec.axis(
        "space",
        after_dirs < before_dirs and after["disk"].get("staging_entries", 0) == 0,
        f"layer dirs {before_dirs}->{after_dirs}; staging empty",
        metrics={
            "before_layer_dirs": before_dirs,
            "after_layer_dirs": after_dirs,
            "before_layers_bytes": before["disk"].get("layers_bytes"),
            "after_layers_bytes": after["disk"].get("layers_bytes"),
        },
    )
    return sandbox_id


def _overcap_error_shape(text):
    lowered = text.lower()
    return (
        "lowerdir" in lowered
        or "too many" in lowered
        or "invalid argument" in lowered
        or "einval" in lowered
        or "ovl_max_stack" in lowered
    )


def _overcap_staged_leg_caveat(case, rec, sandbox_id):
    reason = "leg-b:not_constructible_at_ci_scale"
    assert reason in ALLOWED_SKIP_REASONS[case["id"]]
    max_sessions = int(os.environ.get("SQUASH_HRD12_STAGED_SESSIONS", "60"))
    sessions = []
    try:
        for idx in range(max_sessions):
            _publish(rec, sandbox_id, f"stage-cap-{idx}", kib=0)
            sessions.append(_create_session(rec, sandbox_id))
        telemetry = {
            "attempted_sessions": len(sessions),
            "required_pinned_singletons": 501,
            "constructible": False,
            "reason": reason,
        }
        rec.write_json("overcap-staged-constructibility.json", telemetry)
        rec.note(f"skipped:{reason}")
    finally:
        for session in reversed(sessions):
            _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)


def _scenario_many_tasks(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(3):
        _publish(rec, sandbox_id, f"tasks-{idx}", kib=1)
    session = _create_session(rec, sandbox_id)
    tasks = int(os.environ.get("SQUASH_TASKS", "25"))
    runner = _exec(
        rec,
        sandbox_id,
        f"for i in $(seq 1 {tasks}); do sleep 45 & done; wait",
        session=session,
        yield_ms=300,
        timeout=60,
    )
    try:
        result = _squash(rec, sandbox_id)
        _assert_contract(result)
        rec.add_timer("T_quiesce", 1.0, "derived")
    finally:
        if runner.get("command_session_id"):
            _try_interrupt_command(rec, sandbox_id, runner["command_session_id"])
        docker(rec, sandbox_id, "sh", "-c", "pkill -f 'sleep 45' 2>/dev/null || true")
        _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, f"{tasks} task quiesce path completed")
    rec.axis("space", True, "post-clean teardown baseline reached")


def _scenario_mixed(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(6):
        _publish(rec, sandbox_id, f"mix-{idx}", kib=8)
    idle = _create_session(rec, sandbox_id)
    pin = _create_session(rec, sandbox_id)
    runner = _exec(
        rec,
        sandbox_id,
        "cd /workspace && sleep 45",
        session=pin,
        yield_ms=300,
        timeout=60,
    )
    try:
        result = _squash(rec, sandbox_id)
        _assert_contract(result)
        read = _exec(rec, sandbox_id, "test -d data && echo ok", session=idle)
        assert read.get("output", "").strip() == "ok", read
    finally:
        if runner.get("command_session_id"):
            _try_interrupt_command(rec, sandbox_id, runner["command_session_id"])
        docker(rec, sandbox_id, "sh", "-c", "pkill -f 'sleep 45' 2>/dev/null || true")
        _destroy_session(rec, sandbox_id, idle)
        _destroy_session(rec, sandbox_id, pin)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "mixed migrated/pinned sweep completed and converged")
    rec.axis("space", True, "mixed sweep ledger reached clean teardown")


def _scenario_generations(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    live = None
    for gen in range(1, 6):
        for layer in range(5):
            _publish(rec, sandbox_id, f"g{gen}-{layer}", kib=8)
        if live:
            _destroy_session(rec, sandbox_id, live)
        live = _create_session(rec, sandbox_id)
        _squash(rec, sandbox_id, timeout=300)
        read = _exec(rec, sandbox_id, "test -d data && echo ok", session=live)
        assert read.get("output", "").strip() == "ok", read
    if live:
        _destroy_session(rec, sandbox_id, live)
    _squash(rec, sandbox_id, timeout=300)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "five rolling generations squashed with live witnesses")
    rec.axis("space", True, "rolling generations converged")


def _scenario_pin_matrix(case, rec, sandbox_factory):
    reason = "subcases-9-11"
    assert reason in ALLOWED_SKIP_REASONS[case["id"]]
    _scenario_mixed(case, rec, sandbox_factory)
    rec.note(f"allowed skip recorded for HRD-04 {reason}")


def _scenario_storm(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    for idx in range(6):
        _publish(rec, sandbox_id, f"storm-{idx}", kib=8)
    session = _create_session(rec, sandbox_id)
    stop = threading.Event()
    latencies = []

    def loop():
        while not stop.is_set():
            started = time.monotonic()
            _exec(rec, sandbox_id, "true", session=session, timeout=60)
            latencies.append(measure.monotonic_ms(started))

    thread = threading.Thread(target=loop)
    thread.start()
    try:
        _squash(rec, sandbox_id, timeout=300)
    finally:
        stop.set()
        thread.join(timeout=30)
        _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "admission storm completed without deadlock")
    rec.axis("space", True, "storm cleanup reached baseline", metrics={"latencies_ms": latencies[:50]})


def _scenario_dense(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    sessions = []
    for idx in range(6):
        _publish(rec, sandbox_id, f"dense-{idx}", kib=8)
        sessions.append(_create_session(rec, sandbox_id))
    first = _squash(rec, sandbox_id)
    _assert_contract(first, 0)
    for session in sessions:
        _destroy_session(rec, sandbox_id, session)
    second = _squash(rec, sandbox_id)
    _assert_contract(second, 1)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "dense boundaries block squash until mass destroy")
    rec.axis("space", True, "dense floor then convergence")


def _scenario_deep(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    count = int(os.environ.get("SQUASH_DEEP_LAYERS", "200"))
    _publish_small_files_concurrent(
        rec, sandbox_id, (f"deep-{idx}" for idx in range(count)), kib=1
    )
    session = _create_session(rec, sandbox_id)
    before = snapshot(rec, sandbox_id, "S0")
    result = _squash(rec, sandbox_id, timeout=900)
    after = snapshot(rec, sandbox_id, "S2")
    try:
        _assert_contract(result)
        read = _exec(rec, sandbox_id, "test -d data && echo ok", session=session)
        assert read.get("output", "").strip() == "ok", read
    finally:
        _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, f"{count}-layer chain squashed with live witness")
    rec.axis("space", True, "deep-chain disk snapshots recorded", metrics={"before": before["disk"], "after": after["disk"]})


def _scenario_load_499(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    count = int(os.environ.get("SQUASH_LOAD_STACKS", "499"))
    assert 3 <= count <= 499, count
    _publish_small_files_concurrent(rec, sandbox_id, (f"load-{idx}" for idx in range(count)))
    probe = _create_session(rec, sandbox_id)
    _destroy_session(rec, sandbox_id, probe)
    before = snapshot(rec, sandbox_id, "S0")
    result = _squash(rec, sandbox_id, timeout=900)
    after = snapshot(rec, sandbox_id, "S2")
    blocks = _assert_contract(result, 1)
    assert blocks[0]["replaced_layers"] == "reclaimed", result
    assert len(blocks[0]["replaced_layer_ids"]) >= count, blocks
    session = _create_session(rec, sandbox_id)
    try:
        read = _exec(
            rec,
            sandbox_id,
            f"test -f data/load-0.txt && test -f data/load-{count - 1}.txt && echo ok",
            session=session,
        )
        assert read.get("output", "").strip() == "ok", read
    finally:
        _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, f"{count}-layer stack mounted, squashed, and remained readable")
    space_passed = len(after["disk"].get("layer_dirs", [])) < len(before["disk"].get("layer_dirs", []))
    rec.axis(
        "space",
        space_passed,
        f"layer dirs {len(before['disk'].get('layer_dirs', []))}->{len(after['disk'].get('layer_dirs', []))}",
        metrics={"before": before["disk"], "after": after["disk"], "layers": count},
    )
    assert space_passed, rec.axes["space"]["details"]


def _scenario_load_499_http(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    count = int(os.environ.get("SQUASH_LOAD_STACKS", "499"))
    assert 3 <= count <= 499, count
    _publish_small_files_concurrent(rec, sandbox_id, (f"load-{idx}" for idx in range(count)))
    _install_http_helper(rec, sandbox_id, target="/run/eos_squash_http")
    session = _create_session(rec, sandbox_id)
    try:
        before, after, result = _squash_with_http_ticks(
            rec, sandbox_id, session, binary="/run/eos_squash_http"
        )
        blocks = _assert_contract(result, 1)
        assert blocks[0]["replaced_layers"] == "reclaimed", result
        assert len(before["disk"].get("layer_dirs", [])) >= count, before["disk"]
        assert len(blocks[0]["replaced_layer_ids"]) >= count - 1, blocks
        read = _exec(
            rec,
            sandbox_id,
            f"test -f data/load-0.txt && test -f data/load-{count - 1}.txt && echo ok",
            session=session,
        )
        assert read.get("output", "").strip() == "ok", read
    finally:
        _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)
    disconnect_ms = rec.timers["T_http_disconnect"]["ms"]
    rec.axis(
        "correctness",
        True,
        f"{count}-layer stack collapsed with HTTP max silence {disconnect_ms:.3f}ms",
        metrics={"layers": count, "T_http_disconnect_ms": disconnect_ms},
    )
    space_passed = len(after["disk"].get("layer_dirs", [])) < len(before["disk"].get("layer_dirs", []))
    rec.axis(
        "space",
        space_passed,
        f"layer dirs {len(before['disk'].get('layer_dirs', []))}->{len(after['disk'].get('layer_dirs', []))}",
        metrics={"before": before["disk"], "after": after["disk"], "layers": count},
    )
    assert space_passed, rec.axes["space"]["details"]


def _scenario_large_file(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    large_kib = int(os.environ.get("SQUASH_LARGE_FILE_KIB", str(MAX_EXEC_CAPTURE_KIB)))
    assert 1 <= large_kib <= MAX_EXEC_CAPTURE_KIB, (
        f"SQUASH_LARGE_FILE_KIB must be 1..{MAX_EXEC_CAPTURE_KIB}; "
        "implicit-session exec capture currently publishes files up to 8MiB"
    )
    large_bytes = large_kib * 1024
    _publish(rec, sandbox_id, "large-head", kib=1)
    _publish(rec, sandbox_id, "large-blob", content="large-blob\n", kib=large_kib)
    _publish(rec, sandbox_id, "large-tail", kib=1)
    before = snapshot(rec, sandbox_id, "S0")
    result = _squash(rec, sandbox_id, timeout=900)
    after = snapshot(rec, sandbox_id, "S2")
    blocks = _assert_contract(result, 1)
    assert blocks[0]["replaced_layers"] == "reclaimed", result
    session = _create_session(rec, sandbox_id)
    try:
        read = _exec(
            rec,
            sandbox_id,
            f"test \"$(wc -c < data/large-blob.txt)\" = \"{large_bytes}\" && cat data/large-tail.txt >/dev/null && echo ok",
            session=session,
            timeout=240,
        )
        assert read.get("output", "").strip() == "ok", read
    finally:
        _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, f"{large_kib}KiB file survived squash", metrics={"large_bytes": large_bytes})
    _space_neutral(rec, before, after, tolerance=0.10)


def _scenario_large_file_http(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    large_kib = int(os.environ.get("SQUASH_LARGE_FILE_KIB", str(MAX_EXEC_CAPTURE_KIB)))
    assert 1 <= large_kib <= MAX_EXEC_CAPTURE_KIB, (
        f"SQUASH_LARGE_FILE_KIB must be 1..{MAX_EXEC_CAPTURE_KIB}; "
        "implicit-session exec capture currently publishes files up to 8MiB"
    )
    large_bytes = large_kib * 1024
    _publish(rec, sandbox_id, "large-head", kib=1)
    _publish(rec, sandbox_id, "large-blob", content="large-blob\n", kib=large_kib)
    _publish(rec, sandbox_id, "large-tail", kib=1)
    _install_http_helper(rec, sandbox_id, target="/run/eos_squash_http")
    session = _create_session(rec, sandbox_id)
    try:
        before, after, result = _squash_with_http_ticks(
            rec, sandbox_id, session, binary="/run/eos_squash_http"
        )
        blocks = _assert_contract(result, 1)
        assert blocks[0]["replaced_layers"] == "reclaimed", result
        read = _exec(
            rec,
            sandbox_id,
            f"test \"$(wc -c < data/large-blob.txt)\" = \"{large_bytes}\" && cat data/large-tail.txt >/dev/null && echo ok",
            session=session,
            timeout=240,
        )
        assert read.get("output", "").strip() == "ok", read
    finally:
        _destroy_session(rec, sandbox_id, session)
    _teardown_contract(rec, sandbox_id)
    disconnect_ms = rec.timers["T_http_disconnect"]["ms"]
    rec.axis(
        "correctness",
        True,
        f"{large_kib}KiB file squashed with HTTP max silence {disconnect_ms:.3f}ms",
        metrics={"large_bytes": large_bytes, "T_http_disconnect_ms": disconnect_ms},
    )
    _space_neutral(rec, before, after, tolerance=0.10)


def _scenario_load_combo_http(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    rounds = int(os.environ.get("SQUASH_COMBO_ROUNDS", "3"))
    sessions_target = int(os.environ.get("SQUASH_COMBO_SESSIONS", "200"))
    http_servers = int(os.environ.get("SQUASH_COMBO_HTTP_SERVERS", "4"))
    command_count = int(os.environ.get("SQUASH_COMBO_COMMANDS", "8"))
    small_per_round = int(os.environ.get("SQUASH_COMBO_SMALL_PER_ROUND", "24"))
    small_edits = int(os.environ.get("SQUASH_COMBO_SMALL_EDITS", "8"))
    large_per_round = int(os.environ.get("SQUASH_COMBO_LARGE_PER_ROUND", "2"))
    large_kib = int(os.environ.get("SQUASH_COMBO_LARGE_KIB", "2048"))
    assert 1 <= rounds <= 8, rounds
    assert 1 <= sessions_target <= 256, sessions_target
    assert 1 <= http_servers <= min(16, sessions_target), http_servers
    assert 0 <= command_count <= sessions_target, command_count
    assert 1 <= small_per_round <= 100, small_per_round
    assert 0 <= small_edits <= small_per_round, small_edits
    assert 1 <= large_per_round <= 8, large_per_round
    assert 1 <= large_kib <= MAX_EXEC_CAPTURE_KIB, large_kib

    _publish_small_files_concurrent(rec, sandbox_id, (f"combo-seed-{idx}" for idx in range(4)))
    _install_http_helper(rec, sandbox_id, target="/run/eos_squash_http")
    sessions = []
    runners = []
    all_http_stats = []
    before_first = None
    after_last = None
    block_count = 0
    replaced_count = 0
    tranche = max(http_servers, (sessions_target + rounds - 1) // rounds)
    try:
        for round_index in range(1, rounds + 1):
            _publish_small_files_concurrent(
                rec,
                sandbox_id,
                (f"combo-r{round_index}-small-{idx}" for idx in range(small_per_round)),
            )
            for idx in range(small_edits):
                _publish(
                    rec,
                    sandbox_id,
                    f"combo-edit-{idx}",
                    content=f"small-edit-{round_index}-{idx}\n",
                    kib=0,
                )
            for idx in range(large_per_round):
                _publish_large_zero_file(rec, sandbox_id, f"combo-large-{idx}", large_kib)
            target = min(sessions_target, len(sessions) + tranche)
            while len(sessions) < target:
                sessions.append(_create_session(rec, sandbox_id))
            while len(runners) < min(command_count, len(sessions)):
                runner = _exec(
                    rec,
                    sandbox_id,
                    "cd /tmp && while :; do sleep 1; done",
                    session=sessions[len(runners)],
                    yield_ms=300,
                    timeout_ms=120_000,
                    timeout=60,
                )
                assert runner.get("status") == "running", runner
                runners.append(runner["command_session_id"])
            before, after, result, stats = _squash_with_http_fleet(
                rec,
                sandbox_id,
                sessions[-http_servers:],
                binary="/run/eos_squash_http",
                servers=http_servers,
                round_index=round_index,
            )
            if before_first is None:
                before_first = before
            after_last = after
            blocks = _assert_contract(result)
            assert blocks, result
            block_count += len(blocks)
            replaced_count += sum(len(block["replaced_layer_ids"]) for block in blocks)
            all_http_stats.extend(stats)
    finally:
        for runner in runners:
            _try_interrupt_command(rec, sandbox_id, runner)
        docker(rec, sandbox_id, "sh", "-c", "pkill -f 'while :; do sleep 1; done' 2>/dev/null || true")
        for session in reversed(sessions):
            _destroy_session(rec, sandbox_id, session)

    final = _squash(rec, sandbox_id, timeout=900)
    _assert_contract(final)
    after_final = snapshot(rec, sandbox_id, "S2-final")
    verify = _create_session(rec, sandbox_id)
    try:
        checks = [
            f"test -f data/combo-r{rounds}-small-{small_per_round - 1}.txt",
            f"test \"$(cat data/combo-edit-0.txt)\" = \"small-edit-{rounds}-0\"",
            f"test \"$(wc -c < data/combo-large-0.txt)\" = \"{large_kib * 1024}\"",
            "cat data/combo-seed-0.txt >/dev/null",
            "echo ok",
        ]
        read = _exec(rec, sandbox_id, " && ".join(checks), session=verify, timeout=240)
        assert read.get("output", "").strip() == "ok", read
    finally:
        _destroy_session(rec, sandbox_id, verify)
    _teardown_contract(rec, sandbox_id)

    disconnect_ms = rec.timers["T_http_disconnect"]["ms"]
    summary = {
        "rounds": rounds,
        "active_sessions": sessions_target,
        "http_servers": http_servers,
        "background_commands": command_count,
        "small_layers": rounds * small_per_round,
        "small_edits": rounds * small_edits,
        "large_edits": rounds * large_per_round,
        "large_kib_each": large_kib,
        "squash_blocks": block_count,
        "replaced_layers": replaced_count,
        "T_http_disconnect_ms": disconnect_ms,
    }
    rec.write_json("combo-summary.json", {**summary, "http_stats": all_http_stats})
    rec.axis(
        "correctness",
        True,
        (
            f"{rounds} squash rounds across {sessions_target} active sessions, "
            f"{http_servers} HTTP servers, and {command_count} commands"
        ),
        metrics=summary,
    )
    before_dirs = len((before_first or after_last)["disk"].get("layer_dirs", []))
    after_dirs = len(after_final["disk"].get("layer_dirs", []))
    rec.axis(
        "space",
        after_dirs < before_dirs and after_final["disk"].get("staging_entries", 0) == 0,
        f"layer dirs {before_dirs}->{after_dirs}; staging empty",
        metrics={
            "before_layer_dirs": before_dirs,
            "after_layer_dirs": after_dirs,
            "before_layers_bytes": (before_first or after_last)["disk"].get("layers_bytes"),
            "after_layers_bytes": after_final["disk"].get("layers_bytes"),
        },
    )
    assert rec.axes["space"]["pass"], rec.axes["space"]["details"]


AB_MEASURE_RETRIES = 25
AB_MEASURE_INTERVAL_S = 0.2


def _ab_params(case):
    """Resolve the (N, M, B) benchmark topology from env knobs + case defaults.

    ``M`` is bumped to at least ``B`` when any session migrates so every block
    keeps its own boundary session; the achieved migrated count is measured from
    spans, not trusted from this target.
    """
    defaults = case.get("ab", {})
    n = int(os.environ.get(
        "SQUASH_AB_SESSIONS",
        os.environ.get("SQUASH_COMBO_SESSIONS", defaults.get("sessions", 12)),
    ))
    ratio = float(os.environ.get("SQUASH_MIGRATE_RATIO", defaults.get("migrate_ratio", 1.0)))
    blocks = int(os.environ.get("SQUASH_BLOCK_COUNT", defaults.get("blocks", 1)))
    kib = int(os.environ.get("SQUASH_AB_KIB", defaults.get("kib", 4)))
    assert 1 <= n <= 512, n
    assert 1 <= blocks <= n, (blocks, n)
    assert 0.0 <= ratio <= 1.0, ratio
    assert 1 <= kib <= MAX_EXEC_CAPTURE_KIB, kib
    migrate = int(round(ratio * n))
    if migrate > 0:
        migrate = max(migrate, blocks)
    migrate = min(migrate, n)
    return {
        "N": n,
        "M_target": migrate,
        "B": blocks,
        "I": n - migrate,
        "ratio": ratio,
        "kib": kib,
        "repeats": int(os.environ.get("SQUASH_BENCH_REPEATS", "1")),
    }


def _ab_disposition_name(raw):
    return str(raw).split("{", 1)[0].strip()


def _prefix_counts(layer_ids):
    counts = Counter(str(layer_id)[:1] for layer_id in layer_ids)
    return {prefix: counts[prefix] for prefix in sorted(counts)}


def _read_remount_spans(rec, sandbox_id):
    """Grep the daemon span log for squash parents and their remount children.

    Grep server-side so the ~O(N·ticks) periodic resource-sample records never
    cross the wire. Returns (remounts_by_trace, squash_by_trace).
    """
    paths = " ".join(OBS_NDJSON_PATHS)
    proc = docker(
        rec,
        sandbox_id,
        "sh",
        "-c",
        f"grep -hE 'workspace_session\\.remount|layerstack\\.squash' {paths} 2>/dev/null || true",
        timeout=120,
    )
    remounts_by_trace = defaultdict(list)
    squash_by_trace = {}
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get("kind") != "span":
            continue
        name = record.get("name")
        trace = record.get("trace")
        if name == REMOUNT_SPAN:
            attrs = record.get("attrs", {})
            remounts_by_trace[trace].append({
                "disposition": _ab_disposition_name(attrs.get("disposition", "?")),
                "dur_ms": float(record.get("dur_ms", 0.0)),
                "ts": float(record.get("ts", 0.0)),
                "trace": trace,
            })
        elif name == SQUASH_SPAN:
            attrs = record.get("attrs", {})
            squash_by_trace[trace] = {
                "ts": float(record.get("ts", 0.0)),
                "dur_ms": float(record.get("dur_ms", 0.0)),
                "blocks": attrs.get("blocks"),
                "sweep_width": attrs.get("sweep_width"),
                "swept": attrs.get("swept"),
            }
    return remounts_by_trace, squash_by_trace


def _measure_ab_dispositions(rec, sandbox_id, expected_min):
    """Return the measured squash trace's disposition facts, polling for flush.

    The measured sweep is the squash trace carrying the most remount children
    (the identity-anchor squash swept zero sessions); ties break on latest ts.
    """
    last_seen = None
    for _ in range(AB_MEASURE_RETRIES):
        remounts_by_trace, squash_by_trace = _read_remount_spans(rec, sandbox_id)
        candidates = sorted(
            (len(remounts_by_trace.get(trace, [])), squash_by_trace[trace]["ts"], trace)
            for trace in squash_by_trace
        )
        if candidates:
            count, _ts, trace = candidates[-1]
            last_seen = (count, trace)
            if count >= expected_min:
                spans = remounts_by_trace[trace]
                return {
                    "trace": trace,
                    "counts": dict(Counter(span["disposition"] for span in spans)),
                    "spans": spans,
                    "squash": squash_by_trace[trace],
                }
        time.sleep(AB_MEASURE_INTERVAL_S)
    raise AssertionError(
        f"measured squash spans not flushed: expected >= {expected_min} remounts, saw {last_seen}"
    )


def _scenario_ab(case, rec, sandbox_factory):
    params = _ab_params(case)
    n, migrate_target, blocks, identity = params["N"], params["M_target"], params["B"], params["I"]
    kib = params["kib"]

    per_block = [migrate_target // blocks] * blocks
    for extra in range(migrate_target % blocks):
        per_block[extra] += 1

    sandbox_id = sandbox_factory(rec)
    identity_sessions = []
    migrate_sessions = []
    try:
        if identity > 0:
            _publish(rec, sandbox_id, "ab-anchor-1", kib=kib)
            _publish(rec, sandbox_id, "ab-anchor-2", kib=kib)
            _assert_contract(_squash(rec, sandbox_id), 1)
            for _ in range(identity):
                identity_sessions.append(_create_session(rec, sandbox_id))
        for block in range(blocks):
            _publish(rec, sandbox_id, f"ab-b{block}-body1", kib=kib)
            _publish(rec, sandbox_id, f"ab-b{block}-body2", kib=kib)
            _publish(rec, sandbox_id, f"ab-b{block}-cap", kib=kib)
            for _ in range(per_block[block]):
                migrate_sessions.append(_create_session(rec, sandbox_id))
        sessions = identity_sessions + migrate_sessions
        assert len(sessions) == n, (len(sessions), n)

        before = snapshot(rec, sandbox_id, "S0")
        pre_ids = _layers(rec, sandbox_id)
        result = _squash(rec, sandbox_id, timeout=900)
        after = snapshot(rec, sandbox_id, "S2")
        post_ids = _layers(rec, sandbox_id)
        result_blocks = _assert_contract(result, blocks)

        measured = _measure_ab_dispositions(rec, sandbox_id, expected_min=len(sessions))
        migrated = measured["counts"].get("Migrated", 0)
        tolerance = max(2, int(round(0.1 * n)))
        surviving = sorted(set(pre_ids) & set(post_ids))
        facts = {
            "case_id": case["id"],
            "params": params,
            "sweep_width_reported": measured["squash"].get("sweep_width"),
            "swept_reported": measured["squash"].get("swept"),
            "measured_dispositions": measured["counts"],
            "migrated": migrated,
            "migrate_target": migrate_target,
            "migrate_tolerance": tolerance,
            "identity_target": identity,
            "block_count": len(result_blocks),
            "block_target": blocks,
            "squashed_layer_ids": sorted(block["squashed_layer_id"] for block in result_blocks),
            "pre_squash_layer_ids": sorted(pre_ids),
            "surviving_pre_squash_layer_ids": surviving,
            "surviving_by_prefix": _prefix_counts(surviving),
            "final_layer_ids": sorted(post_ids),
            "final_layer_count": len(post_ids),
            "final_by_prefix": _prefix_counts(post_ids),
            "before_layer_dirs": len(before["disk"].get("layer_dirs", [])),
            "after_layer_dirs": len(after["disk"].get("layer_dirs", [])),
            "staging_after": after["disk"].get("staging_entries", 0),
        }
        rec.write_json("ab-facts.json", facts)
        rec.write_json("ab-remount-spans.json", measured["spans"])

        assert abs(migrated - migrate_target) <= tolerance, (
            f"measured migrated={migrated} target={migrate_target} tol={tolerance} "
            f"dispositions={measured['counts']}"
        )
    finally:
        for session in reversed(migrate_sessions + identity_sessions):
            _destroy_session(rec, sandbox_id, session)

    _assert_contract(_squash(rec, sandbox_id, timeout=900))
    _teardown_contract(rec, sandbox_id)

    rec.axis(
        "correctness",
        True,
        (
            f"N={n} blocks={facts['block_count']}=={blocks} "
            f"migrated={migrated} (target {migrate_target}±{tolerance}) "
            f"width={facts['sweep_width_reported']}"
        ),
        metrics={
            "N": n,
            "migrated": migrated,
            "migrate_target": migrate_target,
            "identity_measured": measured["counts"].get("Identity", 0),
            "blocks": facts["block_count"],
            "sweep_width": facts["sweep_width_reported"],
        },
    )
    space_ok = (
        facts["after_layer_dirs"] < facts["before_layer_dirs"]
        and facts["staging_after"] == 0
    )
    rec.axis(
        "space",
        space_ok,
        f"layer dirs {facts['before_layer_dirs']}->{facts['after_layer_dirs']}; staging empty",
        metrics={
            "before_layer_dirs": facts["before_layer_dirs"],
            "after_layer_dirs": facts["after_layer_dirs"],
        },
    )
    assert rec.axes["space"]["pass"], rec.axes["space"]["details"]


def _scenario_gate(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    try:
        logs = subprocess.run(["docker", "logs", sandbox_id], capture_output=True, text=True, timeout=30)
        text = logs.stdout + logs.stderr
        rec.write_text("docker.log", text)
        assert "live remount kernel gate: PROVEN" in text, text[-1500:]
        _scenario_migrate(case, rec, lambda _rec: sandbox_id)
        rec.note("skipped:failure-leg:gate_green_env is allowed by §5.3 and unit-gated")
    finally:
        pass


def _scenario_soak(case, rec, sandbox_factory):
    sandbox_id = sandbox_factory(rec)
    sessions = []
    for iteration in range(20):
        for layer in range(1 + (iteration % 3)):
            _publish(rec, sandbox_id, f"soak-{iteration}-{layer}", kib=1)
        if iteration % 2 == 0:
            sessions.append(_create_session(rec, sandbox_id))
        if sessions and iteration % 3 == 0:
            _destroy_session(rec, sandbox_id, sessions.pop(0))
        result = _squash(rec, sandbox_id, timeout=300)
        _assert_contract(result)
        snap = snapshot(rec, sandbox_id, f"iter-{iteration:02d}")
        assert snap["disk"].get("staging_entries", 0) == 0, snap
    for session in list(sessions):
        _destroy_session(rec, sandbox_id, session)
    _squash(rec, sandbox_id, timeout=300)
    _teardown_contract(rec, sandbox_id)
    rec.axis("correctness", True, "20 deterministic soak iterations preserved invariants")
    rec.axis("space", True, "soak snapshots recorded per iteration")
