# Handoff — Live E2E for `tests/manager.rs` + `tests/runtime.rs` (time + cgroup + performance)

**Audience:** the engineer/agent running the live Docker E2E. The code is merged
and green on a macOS dev box, but every leaf in `tests/manager.rs` and
`tests/runtime.rs` *skips* there (no Docker, `EOS_E2E_RUN_ROOT` unset). Your job
is to turn those skips into real pass/fail with timing, cgroup, and performance
evidence.

## 0. Objective

Drive both suites against a real Docker-backed gateway and produce, per test:

- correctness verdict (`result.json` pass/fail),
- **time**: CLI `latency_ms` (`exchange.jsonl`), response `wall_time_seconds`,
  `command_total_time_seconds`,
- **cgroup + performance**: `observability.json.p1.{available, cpu_usage_usec,
  memory_current_bytes, memory_max_bytes, memory_max_unlimited}` or `p1.reason`
  when unavailable.

## 1. Where this must run (read first)

- **Linux host with Docker.** `eos-e2e`'s preflight refuses non-Linux
  (`crates/sandbox-e2e-live-test/src/bin/eos-e2e.rs:283`, `OS != "linux"`). Use a
  Linux box, a Linux VM, or WSL2. macOS/Windows Docker Desktop validation (spec
  §6) requires either running the orchestrator from inside a Linux context, or a
  deliberate relaxation of that Linux gate — do **not** silently remove it; raise
  it as a decision if macOS-host validation is in scope.
- The host-orchestration crates (`sandbox-gateway`, `sandbox-cli`,
  `sandbox-provider-docker`) are already cross-platform; only the *orchestrator
  binary's preflight* is Linux-gated.
- cgroup v2 must be active (`/sys/fs/cgroup/cgroup.controllers` present). Without
  it, `p1` records `available:false` with a `reason` and the perf comparison is
  report-only.

## 2. One required code change — run BOTH suites

The orchestrator currently targets only the manager suite:

```rust
// crates/sandbox-e2e-live-test/src/bin/eos-e2e.rs:20
const STAGE1_DEFAULT_TARGET: &[&str] = &["--test", "manager"];
```

Change it to run the runtime oneshot matrix as well (this is the "Stage 2 flip"
the comment describes — it changes nothing else):

```rust
const STAGE1_DEFAULT_TARGET: &[&str] = &["--test", "manager", "--test", "runtime"];
```

Why this is the right seam: `run_cargo_test` (`eos-e2e.rs:~/run_cargo_test`) sets
`EOS_E2E_RUN_ROOT` and runs the selected `--test` binaries while the
observability poller thread samples `get_observability_tree` every 1s
(`OBS_POLL_INTERVAL_MS`) and writes `observability.json` per sandbox. Running
both targets under one orchestrator process is what yields cgroup/perf evidence
for the runtime sandboxes. Driving `cargo test --test runtime` by hand would skip
the poller and produce no `observability.json`.

## 3. Prerequisites (on the Docker host, from repo root)

```sh
export PATH="$PWD/bin:$PATH"            # repo wrappers: sandbox-cli reads the gateway token file

# (1) Package the Linux daemon artifact locally on this host. Do not build a
#     custom sandbox image/container: create_sandbox uses the requested base image
#     and the Docker provider uploads this musl binary into each stopped sandbox
#     container before starting it.
#
#     For a linux/arm64 Docker engine, build the matching artifact:
cargo run -p xtask -- package --target aarch64-unknown-linux-musl --builder cargo --profile package-local
#     -> dist/sandbox-daemon-linux-arm64
#
#     For a linux/amd64 Docker engine:
#     cargo run -p xtask -- package --target x86_64-unknown-linux-musl --builder cargo --profile package-local
#     -> dist/sandbox-daemon-linux-amd64
#
#     If the host C compiler/linker is not already discoverable, put the local
#     musl cross-toolchain on PATH and/or set CC_<target> plus
#     CARGO_TARGET_<TARGET>_LINKER before running xtask. This is host setup only;
#     do not apt-get inside the sandbox and do not build a Docker builder image.
#     If you only have a linux/amd64 daemon on an arm64 host, set
#     manager.docker.platform: linux/amd64 in config/prd.yml.

# (2) Pre-pull the base image (v1 does not auto-pull).
docker pull python:3.11-bookworm

# (3) Build the gateway + cli + orchestrator.
cargo build -p sandbox-gateway -p sandbox-e2e-live-test
```

Confirm `config/prd.yml` `manager.docker` paths resolve from the gateway's
working directory and point at the artifact you just built, for example
`dist/sandbox-daemon-linux-amd64` or `dist/sandbox-daemon-linux-arm64`.
Container paths (`/eos/...`, `/workspace`) must stay absolute.

## 4. Start the docker-wired gateway

```sh
# Writes /tmp/eos-gateway.token and exports SANDBOX_GATEWAY_AUTH_TOKEN; binds 127.0.0.1:7878.
start-sandbox-gateway --backend docker --config-yaml config/prd.yml

# Sanity: the front door answers over TCP with auth (bin/sandbox-cli reads the token file).
sandbox-cli manager list_sandboxes
```

If `create_sandbox` later reports `sandbox runtime is not configured`, the gateway
was not started with `--backend docker` (the orchestrator preflight prints the
full `UNCONFIGURED_GATEWAY_MESSAGE`).

## 5. Run the live E2E

```sh
# Preflight (Linux + Docker reachable + image present + one real create/destroy probe).
eos-e2e preflight --gateway-socket 127.0.0.1:7878 --image python:3.11-bookworm

# Full run: manager lifecycle + runtime oneshot matrix. Keep artifacts for analysis.
eos-e2e --gateway-socket 127.0.0.1:7878 --image python:3.11-bookworm \
        --run-id docker-live-1 --keep-artifacts

# Narrow to just the oneshot exec matrix while iterating:
eos-e2e --gateway-socket 127.0.0.1:7878 --image python:3.11-bookworm --keep-artifacts \
        --test-names \
          command_exec_command_oneshot_success_and_output \
          command_exec_command_oneshot_failure_and_validation \
          command_exec_command_oneshot_running_and_timeout \
          command_exec_command_oneshot_isolation_and_cleanup \
          command_exec_command_oneshot_cgroup_performance
```

The orchestrator writes everything under `{run-root}/` (default
`EOS_E2E_RUN_ROOT_BASE`/`{run_id}`; printed in the summary line). `summary.json`
gives `status`, `counts`, per-test timing, and `observability`.

## 6. What is measured and where

| Signal | Source artifact | Field(s) |
|---|---|---|
| CLI latency | `reports/<sandbox_id>/exchange.jsonl` | `latency_ms` (per recorded call) |
| Command wall time | command response in `exchange.jsonl` | `wall_time_seconds` |
| Command total time | command response | `command_total_time_seconds` |
| Pass/fail + duration | `reports/<sandbox_id>/result.json` | `status`, `duration_ms`, `assertions` |
| cgroup + perf | `reports/<sandbox_id>/observability.json` | `p1.available`, `p1.cpu_usage_usec`, `p1.memory_current_bytes`, `p1.memory_max_bytes`, `p1.memory_max_unlimited`, `p1.reason` |

Every runtime call is appended with `sb.record(&rec)` in the test bodies, so it
lands in `exchange.jsonl`; the follow-up `read_command_lines` calls (OS-EXEC-006,
012) are recorded too.

## 7. Extraction commands

```sh
RUN_ROOT=<run-root>          # from the eos-e2e summary line
SANDBOX_ID=<sandbox-id>      # from reports/ (one dir per provisioned sandbox)

# Time per call:
jq -c 'select(has("argv")) | {argv, exit_code, latency_ms,
        status: .response.status, command_exit_code: .response.exit_code,
        wall_time_seconds: .response.wall_time_seconds,
        command_total_time_seconds: .response.command_total_time_seconds}' \
  "$RUN_ROOT/reports/$SANDBOX_ID/exchange.jsonl"

# cgroup + perf:
jq '{sandbox_id, poll_meta, p1,
     latest_cgroup: .node.resources.latest.cgroup,
     recent_traces: .node.recent_traces}' \
  "$RUN_ROOT/reports/$SANDBOX_ID/observability.json"
```

Performance comparisons the matrix asks for (compute from the above, do not hard-gate in Rust):

- OS-EXEC-008 vs OS-EXEC-001: latency of the 200-line command vs `pwd`.
- OS-EXEC-010 vs OS-EXEC-001: `p1.cpu_usage_usec` of the CPU loop vs `pwd` baseline.
- OS-EXEC-011 vs OS-EXEC-001: `p1.memory_current_bytes` of the 32 MiB pass vs `pwd` baseline.

## 8. Reporting template (fill one row per case)

```
Case        | correctness | cli_latency_ms | wall_s | cmd_total_s | p1.available | cpu_usage_usec | mem_current_bytes | notes/reason
OS-EXEC-001 |             |                |        |             |              |                |                   |
OS-EXEC-002 | ...
... through OS-EXEC-012
manager: create / list / inspect / destroy / observability_tree | pass? | ... | n/a (no command) | ... | p1 from create's sandbox
```

Also capture, once per run: `summary.json` `status` + `counts`, gateway attach
time (`timing.runner.gateway_attach_ms`), and `observability.poll_cycles` /
`poll_errors`.

## 9. Pass/fail expectations (what "real pass" means)

- `tests/manager.rs` (5 leaves): create → list → inspect → destroy round-trips;
  `create_sandbox` returns `/state == "ready"` with `/daemon/host == "127.0.0.1"`
  and a numeric `/daemon/port`; `observability_tree` lists the live sandbox.
- `tests/runtime.rs` (OS-EXEC-001…012): assertions in each oneshot file
  (status/exit_code/offsets/timeouts). These exercise the full path
  CLI → gateway(TCP+auth) → manager → `TcpSandboxDaemonClient`(TCP+auth) →
  in-container daemon.
- §8 caveat: no case reads pre-existing `--workspace-root` files (see
  `workspace-visibility-resolution.md`). If you intend to validate host-file
  visibility, that is a separate `workspace`-crate change, not these tests.

## 10. Likely failure modes to watch

- **`nftables` absent in `python:3.11-bookworm`** (spec §9.8): isolated-network workspaces
  fail at `nft`. One-shot `exec_command` defaults to a Shared-network workspace, so
  the matrix should be unaffected; if a case fails at network setup, confirm the
  workspace network mode or bake `nftables` into the image.
- **Python is available but not load-bearing**: the matrix deliberately keeps its
  core assertions on portable shell (`head -c ... /dev/zero | wc -c`, shell loops)
  so the daemon/runtime lane is not coupled to Python package behavior.
- **Readiness timeout**: if `check_daemon` times out, raise
  `manager.docker.readiness_timeout_ms`; the installer captures container
  `State`/logs into the `ManagerError` for diagnosis (`installer.rs` →
  `engine.capture_failure_context`).
- **Privilege/cgroup**: without `Privileged + CgroupnsMode:private + Init` the
  daemon's overlay/namespace/cgroup work degrades; these are set by the provider,
  but a restricted Docker host (rootless, userns-remap) may still block them.

## 11. Multi-sandbox + recovery (spec §11 acceptance items, same harness)

- N concurrent sandboxes: run the suites with `--max-parallel N` and confirm N
  distinct `reports/<sandbox_id>/` trees, each create/exec/destroy clean.
- Recovery: with sandboxes live, restart the gateway (`start-sandbox-gateway
  --backend docker --config-yaml config/prd.yml`) and run `sandbox-cli manager
  list_sandboxes` — records must be rebuilt from Docker labels + published ports
  (`DockerSandboxRuntime::recover_sandboxes`). Then destroy them.

## 12. Done criteria

- `eos-e2e` exits `0`; `summary.json` `status == "passed"`.
- Each OS-EXEC-001…012 row and each manager leaf has correctness + time + cgroup
  evidence filled in (§8 template).
- Any report-only cgroup gaps carry an explicit `p1.reason`.
- The one-line `STAGE1_DEFAULT_TARGET` change is the only orchestrator edit; if a
  Linux-gate relaxation was needed for macOS-host validation, it is called out
  separately for review.
```
