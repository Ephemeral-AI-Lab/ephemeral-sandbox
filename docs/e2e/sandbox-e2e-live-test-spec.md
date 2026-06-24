# `sandbox-e2e-live-test` — Live End-to-End Test Runner Spec

This is the implementation spec for the new crate `crates/sandbox-e2e-live-test`.
It is a **black-box live E2E runner**: it drives real Docker-container sandboxes
exclusively through the public `sandbox-cli` → `sandbox-gateway` boundary, runs
multiple sandboxes in parallel with configurable concurrency, monitors
performance through observability, and produces run-scoped, reproducible
artifacts with run-scoped cleanup.

## Ownership Boundary (read first)

This spec keeps a strict black-box boundary, fixed by three product decisions:

1. **Sandbox and image operations are performed by `sandbox-cli`.** The runner
   never injects a `SandboxRuntime` or calls manager/runtime internals. Every
   sandbox lifecycle and runtime command — including `--image` provisioning —
   goes out as a `sandbox-cli` invocation against a gateway socket. The runner's
   only job is to *drive the CLI, capture typed responses, assert, monitor, and
   clean up*.
2. **No manager-side observability sink is required.** Performance monitoring
   uses the existing public `manager get_observability_tree` plus daemon-local
   spans. The runner does **not** depend on any new manager trace store. Manager
   create/destroy phase timing, if needed, is measured by the runner as
   wall-clock around the `sandbox-cli` call, not from an internal span.
3. **Linux + Docker only.** The sandbox container is a Docker container. There is
   no non-Linux code path; off-Linux the runner exits with a clear precondition
   error. Run-scoped cleanup keys on the runtime-assigned sandbox ids captured
   for this run plus path namespacing (see *Cleanup*).

**Hard prerequisite (outside this crate, currently unshipped):** the runner
targets a `sandbox-gateway` that is wired with the **real Docker-backed runtime**.
The shipped `sandbox-gateway` binary wires `UnconfiguredRuntime` /
`UnconfiguredDaemonInstaller` stubs that always error
(`crates/sandbox-gateway/src/gateway/main.rs:94-146`); a gateway spawned from the
shipped binary therefore fails every `create_sandbox` with `"sandbox runtime is
not configured"`. **In v1 the runner only attaches** to an externally started,
real-runtime gateway via `--gateway-socket`. Spawning a gateway is deferred until
that runtime ships (see *Open Items*). Until then the live suite is
non-executable; the preflight (below) fails fast and names the missing gateway.

## Live Checkout Anchors

The current checkout has these relevant shapes:

- The crate is **not yet a workspace member** and the directory
  `crates/sandbox-e2e-live-test/` **does not exist**. The `members` array ends at
  `"xtask"` (`Cargo.toml:4-16`); line 17 is `]`. A workspace-wide `cargo build`
  **succeeds today**. Phase 0 must atomically add the member entry *and* create
  the manifest + `src/` + `tests/` tree (adding the member without the manifest is
  what would break the build).
- Workspace conventions: `resolver = "2"`, `edition 2021`, `rust-version 1.85`,
  centralized `[workspace.dependencies]` consumed via `dep.workspace = true`
  (`Cargo.toml:2,19-23,25-68`). Available deps: `tokio` "full" (`:43`),
  `tokio-util` (`:44`), `futures-util` (`:52`), `clap` v4 derive (`:49`),
  `anyhow` (`:48`), `thiserror` (`:40`), `serde` (`:27`), `serde_json` (`:28`),
  `uuid` (v4-only, `:42`), `time` (`:41`), `sha2` (`:34`).
- The public CLI client connects to a Unix socket, writes one JSON line,
  half-closes, reads exactly one newline-terminated JSON line back as a
  `serde_json::Value` (`crates/sandbox-gateway/src/cli/client.rs:30-95`).
- CLI surface: `manager <op> [args]` (System scope) and
  `runtime --sandbox-id <id> <op> [args]` (Sandbox scope); scope/id resolution at
  `crates/sandbox-gateway/src/cli/request_builder.rs:74-98`. The **global**
  scope/default flag is `--default-sandbox-id` (`output.rs:31`). The runtime
  subcommand's `--sandbox-id` (`output.rs:53-55`) selects Sandbox scope. For
  **manager** ops, `--sandbox-id` is an *operation argument* placed in
  `request.args` (`management/impls/inspect_sandbox.rs:14-22`, surfaced in
  `management/mod.rs:59`), not a scope selector. The execution space (subcommand)
  fixes the scope; a manager op cannot be forced into Sandbox scope through the
  CLI.
- CLI exit codes: `0` ok, `1` operation/connection failure (response carried a
  top-level `error` key), `2` usage/build error
  (`crates/sandbox-gateway/src/cli/output.rs:21-23`). The error-key discriminator
  routes a carried `error` to **stderr + exit 1**, a clean response to
  **stdout + exit 0** (`output.rs:266-272`); a request that cannot be *built*
  (e.g. missing `--sandbox-id`) is rendered to **stderr + exit 2**
  (`output.rs:90-96,140-150,288-292`). Response error shape is
  `{ error: { kind, message, details } }`
  (`crates/sandbox-protocol/src/response.rs:30-49`).
- Manager ops and response shapes:
  `crates/sandbox-manager/src/operation/impls/management/` — `create_sandbox`
  requires `--image` + absolute `--workspace-root`
  (`create_sandbox.rs:6-44`; absolute check `management/mod.rs:63-72`); the
  **runtime assigns the sandbox id** and returns it — records serialize as
  `{ id, workspace_root, state, daemon: { socket_path } | null }`
  (`management/mod.rs:88-95`); `get_observability_tree` is bounded fan-out
  (cap 8 concurrent, 1500 ms/daemon — `get_observability_tree.rs:12-13`; traces
  off by default; `trace_limit ≤ 100`, `resource_window_ms ≤ 600000` enforced
  **daemon-side** at `crates/sandbox-daemon/src/observability/service.rs:30-31`,
  applied `:515-521`). Caps are **clamped, not rejected**: an over-limit argument
  is silently reduced, so tests assert clamping, never an error.
- Runtime ops: `crates/sandbox-runtime/operation/src/cli_definition/*` —
  `exec_command`, `write_command_stdin`, `read_command_lines`,
  `create_workspace_session`, `destroy_workspace_session`, `squash`. Command
  yields carry `{ status, exit_code, start_offset, end_offset, total_lines,
  output, wall_time_seconds, command_total_time_seconds, original_token_count,
  command_session_id? }` (`command_operations.rs:324-340`). `command_session_id`
  is present iff the yielding command is still running (a runtime-service
  invariant: the JSON field is emitted iff the domain output carries a session id,
  `command_operations.rs:336`).
- **Runtime errors are uniformly `operation_failed`.** All command,
  workspace-session, and layerstack failures wrap `kind: "operation_failed"`
  (`crates/sandbox-protocol/src/response.rs:20-22`; `command_operations.rs:298`;
  `workspace_session_operations.rs:215,219`; `layerstack_operations.rs:51`).
  Discrimination is via `error.details.*`, not `error.kind`. Only **manager** ops
  emit semantic kinds (`invalid_request`, `unknown_op`, `internal_error`).
- Per-sandbox isolation is inherent: daemon state lives at
  `{runtime_root}/{sandbox_id}/runtime.sock|runtime.pid`
  (`crates/sandbox-manager/src/daemon_install.rs:52-57`), and the observability
  DB path is derived from the socket path:
  `{socket.parent}/observability/observability.sqlite`
  (`crates/sandbox-observability/src/paths.rs:19-35`).
- Sandbox ids are runtime-assigned strings validated `[A-Za-z0-9._-]`, non-empty
  (`crates/sandbox-manager/src/model.rs:10-22`). The caller cannot supply the id
  through `sandbox-cli create_sandbox` (its only args are `--image` /
  `--workspace-root`), so the runner **reads `/id` from the create response** and
  round-trips it.
- Async concurrency idiom in-tree is `Arc<Semaphore>` + `tokio::spawn` (no
  `JoinSet` anywhere): `crates/sandbox-gateway/src/gateway/lifecycle.rs:18,45-56`.
- Existing repeatable-runner precedent: `experiments/sandbox-cli-latency/run.py`
  builds binaries, writes a timestamped run dir with `samples.jsonl` /
  `summary.json`, and records per-invocation `duration_ms`, `returncode`, byte
  counts + sha256.
- Observability records exclude command/env/file contents: schema V5 dropped the
  `execution_snapshots` table and its indexes
  (`crates/sandbox-observability/src/store.rs:237-241`); bounds
  `MAX_ID_LENGTH = 256`, `MAX_ERROR_MESSAGE_LENGTH = 4096`,
  `MAX_PATH_LENGTH = 4096` (`crates/sandbox-observability/src/records.rs:1-11`).
  CPU/memory samples are always `NULL` today (cgroup only `unavailable()` —
  `crates/sandbox-daemon/src/observability/cgroup.rs:12-20`;
  `.../service.rs:283,436`); namespace executions record a single `started_at` at
  `Starting` with no enqueue/`Running` timestamp
  (`crates/sandbox-runtime/operation/src/namespace_execution.rs:177,204`).

## Crate Shape

The crate is **a harness library + integration tests + a thin orchestrator
binary**, not bin-only. This matches how the repo already organizes tests
(`crates/sandbox-daemon/tests/unit.rs` composes `tests/unit/*.rs` submodules via
`#[path]` / `include!`, with shared helpers in a `support` module).

- `src/` is the **harness library** (`config`, `cli_client`, `fixtures`,
  `gateway`, `report`, `cleanup`, `assertion`) plus a small orchestrator bin
  `eos-e2e`. (`report` owns artifact writing *and* the outcome DTOs *and*
  observability snapshotting; `cleanup` owns the run guard *and* Docker reaping —
  the thin single-job wrappers are merged into their sole consumer.)
- `tests/` holds the **per-operation tests**, one directory per operation with
  one leaf file per test case, organized
  `[manager|runtime]/<operation_family>/<operation>/<case>.rs`, plus a
  `routing/` slot for cross-cutting negative/contract cases.
- Operation families mirror the source grouping exactly: manager =
  `lifecycle` + `observability` (`operation/impls/management/*.rs`); runtime =
  `command` + `workspace_session` + `layerstack`
  (`cli_definition/{command,workspace_session,layerstack}_operations.rs`).

Process model (a property of `cargo test`, designed around deliberately): each
top-level `tests/*.rs` compiles to a **separate test binary**, and `#[test]` fns
within a binary run on parallel threads. Therefore the **shared gateway and run
root are owned by the orchestrator bin, not the tests**: `eos-e2e` builds the
binaries, attaches one run-scoped gateway, exports a single `EOS_E2E_RUN_ROOT`,
runs `cargo test`, then aggregates artifacts and cleans up. Concurrency is
`cargo test -- --test-threads=N` (this is `max_parallel`); each `#[test]`
provisions its own sandbox through the shared gateway, giving real parallel
containers. There is no structured cross-binary report on stable Rust
(`--format=json` needs nightly `-Z`), so aggregation reads each test's
`result.json` plus the `cargo test` process exit code — not libtest stdout.

## Resulting File And Folder Structure

```text
docs/e2e/
  sandbox-e2e-live-test-spec.md          # this file

crates/sandbox-e2e-live-test/
  Cargo.toml                             # lib + [[bin]] eos-e2e
  build.rs                               # generate the per-leaf #[path] include lists
  src/                                   # HARNESS LIBRARY (config + runner) + orchestrator bin
    lib.rs                               # re-exports harness surface used by tests/support
    config.rs                            # RunConfig + clap Args; flag > env > default
    cli_client.rs                        # invoke sandbox-cli; capture {response, exit, stdio, latency}
    fixtures.rs                          # provision_sandbox()/with_workspace_session() over sandbox-cli
    gateway.rs                           # attach to run-scoped gateway socket; readiness; shutdown
    report.rs                            # outcome DTOs + artifact writer + observability snapshots
    cleanup.rs                           # RAII run guard: destroy captured ids -> gateway stop -> dirs
    assertion.rs                         # Assertion helpers + evaluation over captured JSON/stdio/exit
    bin/
      eos-e2e.rs                         # orchestrator: preflight -> build -> attach gateway -> cargo test -> aggregate -> cleanup
  tests/
    support/
      mod.rs                             # shared fixture entry: reads env, re-exports src harness
    manager.rs                           # test binary: mod support; include!(generated manager mods)
    manager/
      lifecycle/
        create_sandbox/                  # one dir per operation; one file per case
          returns_ready.rs               # M1 — one #[test] fn per case file
        inspect_sandbox/
          returns_record.rs              # M3
        list_sandboxes/
          lists_ready.rs                 # M2
        destroy_sandbox/
          removes_sandbox.rs             # M5
      observability/
        get_observability_tree/
          returns_tree.rs                # M4
      routing/
        scope_and_dispatch/
          unknown_op.rs                  # negative/contract: N1
    runtime.rs                           # test binary: mod support; include!(generated runtime mods)
    runtime/
      command/
        exec_command/                    # several test files per operation
          one_shot.rs                    # R1
          in_session.rs                  # R3
          long_running.rs                # R4
        write_command_stdin/
          echoes_input.rs                # R5
        read_command_lines/
          monotonic_offsets.rs           # R6
      workspace_session/
        create_workspace_session/
          host_compatible.rs             # R2
        destroy_workspace_session/
          clean.rs                       # R7a
          busy.rs                        # R7b
      layerstack/
        squash/
          after_mutation.rs              # R8
      routing/
        scope_and_dispatch/
          missing_sandbox_id.rs          # negative/contract: N2
```

Module wiring follows the repo convention (`crates/sandbox-daemon/tests/unit.rs`
uses `#[path]` + `include!`), but the per-leaf include list is **generated**, not
hand-maintained: `build.rs` walks `tests/<scope>/**/*.rs` and emits
`$OUT_DIR/<scope>_mods.rs` containing one `#[path = "..."] mod <slug>;` line per
leaf. Each root test binary stays a stable two lines, so **adding a test case is
adding one file** (a new operation is a new directory; its first case is still one
file) — no registry edit:

```rust
// tests/manager.rs
#[path = "support/mod.rs"] mod support;
include!(concat!(env!("OUT_DIR"), "/manager_mods.rs"));
```

Module slugs are derived deterministically from the leaf path
(`<family>_<operation>_<case>`), so two authors cannot collide.

`Cargo.toml` (lib + orchestrator bin; tests drive the system over the socket, so
no manager/runtime internal crates are needed for the black-box path):

```toml
[package]
name = "sandbox-e2e-live-test"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lib]
path = "src/lib.rs"

[[bin]]
name = "eos-e2e"
path = "src/bin/eos-e2e.rs"

[dependencies]
clap.workspace = true
tokio.workspace = true
tokio-util.workspace = true     # CancellationToken for shutdown
anyhow.workspace = true
thiserror.workspace = true
serde = { workspace = true }
serde_json.workspace = true     # parse the NDJSON response line
uuid.workspace = true           # internal request correlation only (NOT run_id)
time.workspace = true           # UTC timestamps for run dirs
sha2.workspace = true           # deterministic run-id slug

[dev-dependencies]
# tests link the harness as a normal lib dependency; if assertions parse
# typed DTOs instead of serde_json::Value, add sandbox-protocol here too.

[lints]
workspace = true
```

`futures-util` is not required (cargo test owns thread-level parallelism; the
orchestrator does not run its own `join_all` fan-out). Optional: add
`sandbox-protocol.workspace = true` only if typed request/response DTOs are
preferred over `serde_json::Value`. Default is `serde_json::Value`, to stay
strictly behind the public socket boundary.

## Runner Architecture

Two cooperating layers, split along the `cargo test` process boundary. The split
is forced by that process model — separate test binaries share one externally
started gateway and run root only through the environment, and stable libtest
gives no machine report — so the orchestrator is the thin owner of preflight,
manifest, and aggregation across the independent `cargo test` binaries.

- **Orchestrator bin `eos-e2e`** (`src/bin/eos-e2e.rs`) owns the run: it runs the
  preflight, builds binaries, attaches one run-scoped gateway, exports the run
  environment, invokes `cargo test`, then aggregates artifacts and runs cleanup.
  It is the single command an operator or CI runs.
- **Integration tests** (`tests/`) own per-operation correctness. Each `#[test]`
  uses the `support` fixtures to provision a sandbox via `sandbox-cli`, drives the
  operation under test plus the minimal precondition calls it needs, asserts typed
  response fields, and writes its own per-sandbox artifacts. Tests discover the
  gateway socket and run root from `run-manifest.json` under `EOS_E2E_RUN_ROOT`.

Orchestrator data flow:

```text
eos-e2e
 └─ RunConfig (flag > env > default); allocate/validate run_id
 └─ Phase P  PREFLIGHT (before any build)
      Linux? -> docker reachable? -> `docker image inspect {image}`? ->
      runtime probe: a cheap manager call against --gateway-socket; if the
      response error says "runtime is not configured", exit 2 naming the missing
      real-runtime gateway. Any failure: exit 2 with the exact missing item.
 └─ RunReport::create(run_root) -> run-manifest.json (git HEAD, config, gateway socket, image, clock)
 └─ Phase A  BUILD (untimed by runner; recorded in summary.timing.build.*)
      cargo build sandbox-gateway/sandbox-cli --profile package-fast
      [skipped when BuildSource::Prebuilt or --gateway-socket is given]
 └─ Phase B  RUNNER CLOCK STARTS
      gateway.rs: attach to --gateway-socket; wait_for_path(socket)   # readiness poll
      export EOS_E2E_RUN_ROOT={run_root}                              # the only env contract
      run:  cargo test -p sandbox-e2e-live-test [--test manager|runtime] \
                       [-- <name filters>] -- --test-threads={max_parallel}
      RUNNER CLOCK STOPS
 └─ report: glob reports/*/result.json  ->  summary.json  (pass/fail gate = cargo test exit code)
 └─ cleanup (per policy): destroy captured ids -> gateway shutdown -> remove run_root
 └─ ExitCode::SUCCESS iff cargo test exited 0 and every result.json is status==passed
```

Per-test flow inside each `#[test]` (driven through `support` fixtures, all over
`sandbox-cli`):

```text
support::harness()                       # lazy: read EOS_E2E_RUN_ROOT, load run-manifest, build cli_client
let sb = harness.provision_sandbox(..);  # sandbox-cli manager create_sandbox --image .. --workspace-root ..
                                         #   sb.id := response "/id"  (runtime-assigned, round-tripped)
  (RAII guard) on drop -> sandbox-cli manager destroy_sandbox --sandbox-id sb.id
<operation under test + preconditions>   # sandbox-cli manager|runtime <op> ...
assert typed response fields
harness.snapshot_observability(sb.id);   # sandbox-cli manager get_observability_tree --sandbox-id sb.id ...
write reports/{sb.id}/{exchange.jsonl, observability.json, result.json}
```

Every `sandbox-cli` invocation is captured by `cli_client.rs` as a record:
`{ argv, request_json?, response_json, exit_code, stdout, stderr, latency_ms }`.
Response parsing is `serde_json::from_slice::<Value>` on the single response line;
the parsed `error` may arrive on stdout (exit 1) or stderr (exit 2), so the record
keeps both streams. Setup, the operation under test, and observability reads all
go through the same public `sandbox-cli` path — there is no separate provisioning
API.

## Config Schema

```rust
struct RunConfig {
    run_id: String,            // --run-id | derived "r{ts}-{sha256(HEAD‖tests‖salt)[..8]}";
                               //   must match SandboxId charset [A-Za-z0-9._-]
    max_parallel: usize,       // --max-parallel | EOS_E2E_MAX_PARALLEL |
                               //   available_parallelism().min(8); 1 = serial.
                               //   Passed to cargo test as --test-threads=N.
    tests: TestSelection,      // All | Names(Vec<String>) | RerunFailedFrom(PathBuf).
                               //   Mapped to `cargo test --test {manager|runtime}`
                               //   plus libtest name filters (family_operation_case).
    image: String,             // --image (e.g. "ubuntu:24.04"); default for provisioning
    run_root: PathBuf,         // ${EOS_E2E_RUN_ROOT:-$TMPDIR/eos-e2e}/{run_id}
    gateway_socket: PathBuf,   // --gateway-socket; v1 is attach-only (required)
    build: BuildSource,        // Cargo { profile } (default "package-fast") | Prebuilt(PathBuf)
    cli_timeout: Duration,     // per CLI call, default 30s
    gateway_ready_timeout: Duration, // socket-bind wait, default 5s
    cleanup: CleanupPolicy,    // Always | OnSuccess (default) | Never ; --keep-artifacts
}

enum TestSelection { All, Names(Vec<String>), RerunFailedFrom(PathBuf) }
enum CleanupPolicy { Always, OnSuccess, Never }
enum BuildSource   { Cargo { profile: String }, Prebuilt(PathBuf) }
```

`BuildSource` folds the former `build: bool` + `prebuilt_bin_dir` into one
unambiguous field (`Prebuilt` ⇒ skip Phase A; `build.*_ms = 0`). There is no
per-test timeout knob: a per-test cap cannot be enforced across the `cargo test`
process boundary on stable, so wall-clock limits are left to `cargo`/CI.

The single cross-process env contract is **`EOS_E2E_RUN_ROOT`**; the gateway
socket, `run_id`, and image are read from `{run_root}/run-manifest.json`. Honor
existing env names where the CLI reads them directly: `SANDBOX_GATEWAY_SOCKET`,
`SANDBOX_DEFAULT_ID` (`crates/sandbox-config/src/configs/cli.rs:6-7`),
`CARGO_TARGET_DIR`. Durations in serialized form are `f64` seconds. Reject a
`run_id` containing characters outside `[A-Za-z0-9._-]` at parse time, because it
namespaces the run root and artifact paths.

## Test Layout and Fixtures

Each operation gets its own directory under
`tests/<scope>/<operation_family>/<operation>/`, holding one leaf file per test
case (`<case>.rs`), each carrying the `#[test]` fn for that case. A test is:
provision via fixture → drive the operation under test
plus its minimal precondition calls → assert typed response fields → (RAII) tear
down. No central suite registry; the test tree *is* the registry, discovered by
`cargo test` and wired through the generated include list.

The shared harness lives in `src/` and is surfaced to tests through
`tests/support/mod.rs`:

```rust
// src/fixtures.rs (re-exported via tests/support/mod.rs)
pub struct Harness { cli: CliClient, run_root: PathBuf, run_id: String, image: String }

impl Harness {
    // Lazy singleton: reads EOS_E2E_RUN_ROOT, loads {run_root}/run-manifest.json
    // for the gateway socket / run_id / image. When EOS_E2E_RUN_ROOT is unset
    // (i.e. tests were run without the eos-e2e orchestrator), each #[test]
    // early-returns after writing a result.json with status "skipped" and a
    // reason ("run via eos-e2e"), instead of panicking.
    pub fn get() -> Option<&'static Harness>;

    // Setup via the existing manager CLI — same path as the system under test.
    // image defaults to the manifest image; profile selects the session profile.
    pub fn provision_sandbox(&self, slug: &str, image: Option<&str>) -> Sandbox; // id := response "/id"
    pub fn cli(&self) -> &CliClient;                                             // raw sandbox-cli driver
    pub fn snapshot_observability(&self, id: &str);                             // get_observability_tree -> artifact
}

pub struct Sandbox { pub id: String, pub workspace_root: PathBuf, /* ... */ }
impl Drop for Sandbox { /* sandbox-cli manager destroy_sandbox --sandbox-id id (idempotent) */ }
```

Assertion helpers (in `src/assertion.rs`) keep leaf tests terse and consistent
and absorb every response shape, so leaves never hand-walk JSON. New
response-shape checks are added here, not inlined in a leaf:

```rust
pub fn ok(resp: &Value);                                   // asserts no top-level "error" key
pub fn field<'a>(resp: &'a Value, ptr: &str) -> &'a Value; // json-pointer get-or-panic
pub fn err_kind_at(rec: &CallRecord, kind: &str, exit: i32); // locate `error` on stdout-or-stderr;
                                                             //   assert error.kind == kind AND exit
pub fn err_detail<'a>(resp: &'a Value, ptr: &str) -> &'a Value; // error.details pointer (runtime errors)
pub fn offsets_monotonic(resp: &Value);                    // within one response: start <= end <= total
pub fn non_decreasing(prev: &Value, next: &Value, ptr: &str); // across two responses: next[ptr] >= prev[ptr]
```

`err_kind_at` is the single negative-path helper: manager errors arrive on
stdout/exit-1 with a semantic `kind`; CLI build/usage errors arrive on
stderr/exit-2; runtime errors arrive with `kind == "operation_failed"` and detail
under `error.details`. `err_detail` reads that detail; `non_decreasing` expresses
the cross-read offset invariant a single-response check cannot.

A leaf test reads like:

```rust
// tests/runtime/command/exec_command/one_shot.rs
#[test]
fn one_shot_exec_returns_ok_and_zero_exit() {
    let Some(h) = support::harness() else { return };          // skip when not under eos-e2e
    let sb = h.provision_sandbox("command-exec_command-case1", None); // create_sandbox; id from response
    let resp = h.cli().runtime(&sb.id, "exec_command", &["pwd"]);
    assert::ok(&resp);
    assert_eq!(assert::field(&resp, "/status"), "ok");
    assert_eq!(assert::field(&resp, "/exit_code"), 0);
    assert!(resp.get("command_session_id").is_none());          // terminal => no session id
    // sb drops here -> sandbox-cli manager destroy_sandbox
}
```

Per-test sandbox ids are **runtime-assigned** and captured from the create
response; the runner round-trips them for per-id ops, teardown, and artifact
paths. Test slugs are structural — `{family}-{operation}-case{N}` — so workspace
roots and report dirs stay collision-free without hand-authored ids. The
`Sandbox` RAII guard makes teardown panic-safe even when an assertion fails.

## Manager and Runtime CLI Test Matrix

All ops are driven via `sandbox-cli` against the gateway socket (this exercises
both the System and Sandbox routing arms enforced by
`crates/sandbox-manager/src/router/dispatch.rs:8-31`). Assertions read typed JSON
fields, never string formatting.

| #  | Op (scope)                       | Precondition            | Invocation                                                                          | Assertions                                                                                          |
|----|----------------------------------|-------------------------|-------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------|
| M1 | create_sandbox (Sys)             | gateway up; abs ws root | `manager create_sandbox --image I --workspace-root {ws}`                            | no `error`; `/id` non-empty; `/state == "ready"`; `/daemon/socket_path` non-null                    |
| M2 | list_sandboxes (Sys)             | after M1                | `manager list_sandboxes`                                                            | `/sandboxes` array contains `{ id, state: "ready" }`                                                 |
| M3 | inspect_sandbox (Sys)            | after M1                | `manager inspect_sandbox --sandbox-id id`                                           | `/id == id`; `/workspace_root`, `/state`, `/daemon` present                                          |
| M4 | get_observability_tree (Sys)     | after M1                | `manager get_observability_tree --sandbox-id id --include-recent-traces 1 --trace-limit 100` | `/sandboxes/0/sandbox_id == id`; `/availability ∈ {available,partial,unavailable}`; keys `resources,workspaces,recent_traces,errors` present; over-limit `--trace-limit 9999` is silently clamped, not rejected |
| M5 | destroy_sandbox (Sys)            | M1, state Ready         | `manager destroy_sandbox --sandbox-id id`                                           | no `error`; returned `/id == id`; follow-up `inspect_sandbox` returns `error` (removed)              |
| R1 | exec_command one-shot (Sbx)      | Ready sandbox           | `runtime --sandbox-id id exec_command pwd`                                          | `/status == "ok"`; `/exit_code == 0`; no `/command_session_id`                                       |
| R2 | create_workspace_session (Sbx)   | Ready                   | `runtime --sandbox-id id create_workspace_session --profile host_compatible`        | `/workspace_session_id` non-empty; `/profile == "host_compatible"`                                   |
| R3 | exec in session (Sbx)            | after R2 (ws)           | `runtime --sandbox-id id exec_command --workspace-session-id ws "echo hi > f"` then a second exec reading `f` | both `/status == "ok"`; second exec observes the first's write (state persists)            |
| R4 | exec long-running (Sbx)          | Ready                   | `runtime --sandbox-id id exec_command --yield-time-ms 0 cat`                         | `/status == "running"`; capture `/command_session_id`                                                |
| R5 | write_command_stdin (Sbx)        | after R4 (cmd)          | `runtime --sandbox-id id write_command_stdin --command-session-id cmd hello`         | `/start_offset`,`/end_offset` are u64; `/output` reflects echoed input                               |
| R6 | read_command_lines offsets (Sbx) | after R4/R5             | `runtime --sandbox-id id read_command_lines --command-session-id cmd --start-offset 0 --limit 100` | `/command_session_id == cmd`; `offsets_monotonic`; re-read from prior `end_offset` ⇒ `non_decreasing("/start_offset")` |
| R7a| destroy_workspace_session clean (Sbx) | R2, no active cmds | `runtime --sandbox-id id destroy_workspace_session --workspace-session-id ws`        | no `error`; `/destroyed == true`                                                                    |
| R7b| destroy_workspace_session busy (Sbx)  | R2 + a running command in ws | same, while a command is live                                              | `error.kind == "operation_failed"`; `err_detail("/active_command_session_ids")` non-empty           |
| R8 | squash after mutation (Sbx)      | Ready; run a file-writing command first | `runtime --sandbox-id id squash`                                  | no `error`; `/squashed == true`; `/revision/root_hash` non-empty                                    |
| N1 | unknown system op                | gateway up              | `manager <unknown-op>`                                                              | `err_kind_at(rec, "unknown_op", 1)` (error on stderr, exit 1)                                        |
| N2 | runtime op, no sandbox id        | gateway up              | `runtime <op>` without `--sandbox-id`/`--default-sandbox-id`                        | exit `2`; error on stderr; message "runtime operations require --sandbox-id or SANDBOX_DEFAULT_ID"   |

The negatives live under `tests/<scope>/routing/scope_and_dispatch/<case>.rs`. The former
"manager op forced into Sandbox scope" case is **dropped**: the CLI fixes scope
from the subcommand (`request_builder.rs:74-79`), so that fault is only reachable
by hand-forging a protocol request, which violates the black-box boundary. Scope
routing is covered indirectly by N1/N2.

Ordering constraints the tree must encode: `create_sandbox` precedes all per-id
ops; `destroy_sandbox` is rejected while `Creating`/`Stopping`; the tree only
aggregates Ready sandboxes; session-scoped exec needs a prior
`create_workspace_session`; `write_command_stdin`/`read_command_lines` need a
still-running command's `command_session_id`; `destroy_workspace_session` succeeds
only with no active commands (else `operation_failed` +
`active_command_session_ids`); `squash` reports `true` only after committed layer
changes, so R8's fixture mutates first.

Assertion strategy: discriminate success via absence of the top-level `error` key;
for expected failures use `err_kind_at` (manager semantic kinds; runtime
`operation_failed`) and inspect `err_detail`. Assert field presence + type +
invariants (monotonic offsets, integer-or-null exit codes, `command_session_id`
present iff the command is still running). Round-trip ids (capture → feed →
destroy) rather than matching formats. Assert CLI exit codes (0/1/2) to cover the
stdout/stderr stream contract.

## Observability and Performance Monitoring

No manager observability sink is introduced. Monitoring is read-only over the
public tree plus optional daemon-side spans.

Primary signal — `report.rs` polls
`manager get_observability_tree --include-recent-traces 1 --trace-limit 100
--resource-window-ms 60000` periodically **during** the run (recent traces age out
of the bounded window) and writes per-sandbox `observability.json` snapshots
(latest tree node + bounded recent-trace summaries in one file). The tree exposes,
per sandbox: `lifecycle_state`, `availability`, `resources` (latest + history),
`workspaces` (+ active namespace executions), and bounded `recent_traces`
(`crates/sandbox-manager/src/operation/impls/management/get_observability_tree.rs:88-206`).

Runner-measured timing (no internal spans needed): the runner records wall-clock
around each `sandbox-cli` call, so it already captures `create_ms`,
`daemon_ready_ms` (the cost of `create_sandbox` +
`crates/sandbox-manager/src/operation/impls/management/create_sandbox.rs:62-90`),
per-op latency, and end-to-end test time. This satisfies "performance, not just
correctness" without a manager trace store.

Optional daemon-side enhancements (separate, additive; not required for the runner
to function — file under follow-up work):

- **P1 cgroup CPU/memory in resource samples.** Owner `sandbox-daemon`
  (`observability/cgroup.rs`, `service.rs`). Fills existing schema columns
  (`cpu_usage_usec`, `memory_current_bytes`, `memory_max_*` — V2 schema), no new
  table. Privacy: numeric counters + sandbox-internal cgroup path only, bounded by
  `MAX_PATH_LENGTH`/`MAX_ERROR_MESSAGE_LENGTH`. Especially relevant now that
  sandboxes are Docker containers (cgroup is the per-container pressure signal).
- **P2 namespace queue-wait timing.** Owner `sandbox-runtime/operation`
  (`namespace_execution.rs`) + daemon projection + observability schema V6 (two
  additive columns `enqueued_at_unix_ms`, `running_at_unix_ms` ⇒ derive
  `queue_wait_ms`). Privacy: timestamps only. This is the one gap that separates
  queue wait from exec time under parallel load.

The runner consumes P1/P2 automatically **iff they surface in
`get_observability_tree`** (`resources` / `recent_traces` nodes), since that is the
only black-box path; the tree builder is additive
(`get_observability_tree.rs:253-303`), so new daemon-emitted sub-keys appear
without a runner change. The `*_for_test` SQLite readers
(`crates/sandbox-observability/src/store.rs:1062,1108`) are **not** used — linking
the observability crate would violate the black-box boundary — so P1/P2
unit-level assertions belong to the daemon crates' own tests, not this runner.
Their absence only reduces diagnostic resolution; it does not block the runner.

Deliberately **out of scope**: gateway/manager/forwarding spans and a manager
trace store (decision 2). If forwarding latency must be attributed, the runner
infers it from the gap between its own measured CLI latency and the daemon-side
request trace `duration_ms` exposed in the tree.

## Parallel Execution Model

- Unit of parallelism: one `#[test]` owns exactly one sandbox — aligns the test
  boundary with the system's natural per-sandbox-id isolation
  (`crates/sandbox-manager/src/daemon_install.rs:55`).
- Mechanism: `cargo test`'s own thread pool. The orchestrator passes
  `-- --test-threads={max_parallel}`, so N tests (hence N sandboxes/containers)
  run concurrently against the one shared gateway. No bespoke `Semaphore`/`JoinSet`
  fan-out in the harness — the test runner is the scheduler. A shared gateway
  across separate test binaries is sound: it is a concurrent `UnixListener` with a
  `Semaphore`-bounded accept loop and the client connects per call
  (`lifecycle.rs:14,18,35-57`; `client.rs:31`).
- `max_parallel`: `--max-parallel` > `EOS_E2E_MAX_PARALLEL` >
  `available_parallelism().min(8)`; `N = 1` (`--test-threads=1`) is deterministic
  serial mode. Note the two test binaries (`manager`, `runtime`) run sequentially
  by default; `cargo test` parallelizes within each binary. The orchestrator can
  run them as one invocation or target a single `--test` for focused runs.
- Isolation boundary: one shared gateway per run (stateless routing front door)
  plus a distinct runtime-assigned sandbox id + Docker container per test.
  Distinct ids already give full socket/pid/observability-DB isolation
  (`crates/sandbox-observability/src/paths.rs:28`), so N gateways are unnecessary.
- Shared mutable state across tests is avoided by construction: each test
  provisions and destroys its own sandbox; the only shared resources are the
  read-only gateway socket and the append-only run-root (per-test subdirs keyed by
  the unique sandbox id), so parallel tests never contend.

## Reproducibility, Artifacts, and Cleanup

Reproducibility — one `run_root` whose leaf is `run_id`; all paths derive
deterministically except the sandbox id, which the runtime assigns and the runner
captures:

| Resource          | Value                                                                    |
|-------------------|--------------------------------------------------------------------------|
| sandbox id        | runtime-assigned (read from create response, validated `[A-Za-z0-9._-]`) |
| workspace root    | `{run_root}/work/{slug}`                                                 |
| daemon socket/pid | `{runtime_root}/{sandbox_id}/runtime.{sock,pid}` (inherent)             |
| observability db  | `{...}/{sandbox_id}/observability/observability.sqlite` (auto-derived)  |
| gateway socket/pid| external; recorded in `run-manifest.json`                               |
| report dir        | `{run_root}/reports/{sandbox_id}/`                                       |

`run_id`: `--run-id` verbatim, else
`r{ts}-{sha256(git_HEAD ‖ test_manifest_hash ‖ EOS_E2E_RUN_SALT)[..8]}` using
`sha2` (timestamp pinnable via `EOS_E2E_RUN_CLOCK` for byte-stable reruns; `sha2`
and `time` are workspace deps, so the scheme is dependency-feasible). `uuid` is
deliberately avoided for `run_id` (it is v4-random in-tree); it is used only for
internal request correlation where nondeterminism is harmless. Sandbox ids are
*not* deterministic — uniqueness and run-scoping come from the `run_root`
namespace, not from a predicted id.

Artifact tree (five kinds, each carrying `schema_version`):

```text
{run_root}/                                  # leaf = run_id
  run-manifest.json     # schema_version, git HEAD, config, gateway socket, image, clock
  summary.json          # schema_version, run rollup, timing sub-object, cleanup sub-object
  work/{slug}/
  reports/{sandbox_id}/
    exchange.jsonl      # schema_version header line, then one {argv,request,response,exit_code,stdout,stderr,latency_ms} per line
    observability.json  # schema_version; latest tree node + bounded recent-trace summaries for this sandbox
    result.json         # schema_version; TestOutcome (test_name, status, assertions, durations)
```

`timing.json`, `cleanup-report.json`, `traces.json`, and per-test `stdout.log` /
`stderr.log` are **not** separate files: timing and cleanup are sub-objects of
`summary.json`; trace summaries fold into `observability.json`; stdout/stderr are
recorded per call inside `exchange.jsonl`.

`summary.json`:
`{ schema_version, run_id, git_head, started_at, finished_at, max_parallel,
status (passed|failed|error), counts{total,passed,failed,skipped,errored},
tests[]{ name (scope::family::operation::case::fn), sandbox_id, status, duration_ms,
workspace_root, report_dir, assertions{total,failed}, failure }, failed_tests[],
artifacts_root, timing{...}, cleanup{...} }`. The orchestrator builds `tests[]`
solely by globbing each test's `reports/*/result.json`; a missing `result.json` is
an `errored` test. The pass/fail gate is the **`cargo test` process exit code** —
there is no libtest-stdout parsing (it is nightly-only for JSON and brittle to
renames/`#[ignore]`). `failed_tests[]` is the set of `result.json` with
`status == failed`, and drives focused rerun.

`summary.timing` separates build from runner wall time:
`{ build{ gateway_build_ms, cli_build_ms, cargo_profile, cache_hit }, runner{
wall_ms, gateway_attach_ms, test_setup_total_ms, test_exec_total_ms, teardown_ms,
max_parallel_observed, queue_wait_p50_ms, queue_wait_p95_ms }, per_test[]{ name,
sandbox_id, queue_wait_ms, create_ms, daemon_ready_ms, exec_ms, teardown_ms,
total_ms } }`. Build binaries in **Phase A** (own `Instant`s → `timing.build.*`);
start the **runner clock only after** binaries exist and the gateway socket is
reachable. `BuildSource::Prebuilt` / `--gateway-socket` set `build.*_ms = 0`,
keeping `runner.wall_ms` cache-independent.

Run-scoped cleanup — provably this-run-only, keyed on two tags so it can never
touch a sibling run or another agent:

1. **Captured sandbox ids** — the runner destroys exactly the ids it created this
   run (collected from each create response). Manager `destroy_sandbox` is issued
   only for those ids.
2. **Path namespacing** — every artifact/socket/pid/db/workspace this runner owns
   lives under `{run_root}`; `remove_dir_all(run_root)` cannot reach a sibling
   run's tree.

There is **no Docker-label tag**: `create_sandbox` accepts only
`--image`/`--workspace-root` and the runtime carries no label field
(`crates/sandbox-manager/src/runtime.rs:6-14`), so no `eos.e2e.run_id` label can
be stamped through `sandbox-cli` today. Consequently there is **no orphan-reaping
backstop** for a process killed mid-run (`--test-threads` abort / SIGINT /
SIGKILL): the RAII `Sandbox` drop reaps on assertion panic but not on a hard kill.
Restoring a label-based backstop requires the runtime to accept and apply a
run-id label (see *Open Items*).

Teardown order (each step idempotent):

1. For each captured sandbox id (primarily via each test's RAII `Sandbox` drop,
   with the orchestrator sweeping any survivors it recorded):
   `sandbox-cli manager destroy_sandbox` (graceful).
2. Gateway: the runner only **detaches**; it does not stop a gateway it did not
   start. (When spawn mode lands, this step shuts the spawned gateway down via its
   `CancellationToken` — the gateway self-removes its socket+pid,
   `crates/sandbox-gateway/src/gateway/lifecycle.rs:90-93`.)
3. `remove_dir_all(run_root)` gated by `CleanupPolicy` (default: keep on failure
   for inspection, remove on success; `--keep-artifacts` forces keep).

An RAII drop guard owns `run_root` so panic / Ctrl-C still tears down. A standalone
`--clean-run {run_id}` repeats the teardown for re-cleanup. The cleanup result
(which sandboxes, sockets, and directories were removed) is recorded in
`summary.cleanup`.

Linux/Docker precondition is enforced by the preflight (next section), before any
build.

## Preflight

`eos-e2e` runs one ordered preflight before Phase A, failing fast with the exact
missing item (exit `2`) so first-run problems surface before an expensive build:

1. Linux — else `exit 2`: "EphemeralOS E2E is Linux+Docker only; current OS=…".
2. Docker reachable (`docker version`) — else `exit 2`: "Docker daemon not
   reachable at $DOCKER_HOST".
3. Image present (`docker image inspect {image}`) — else `exit 2`: "image {image}
   not present; run `docker pull {image}`".
4. Real-runtime gateway probe — a cheap `manager` call against `--gateway-socket`;
   if the response error contains "runtime is not configured", `exit 2`: "the
   target gateway has no Docker runtime wired (shipped `sandbox-gateway` uses
   Unconfigured stubs, `gateway/main.rs:94-146`); point `--gateway-socket` at a
   real-runtime gateway. This crate does not provide it."

The same checks are available as `eos-e2e preflight` for a standalone dry run.

### Daemon binary & gateway config (prerequisite enumeration)

The real-runtime gateway the runner attaches to needs three inputs the runner does
not provide (they belong to the unshipped runtime wiring, *Open Items* #1): a
`sandbox-daemon` executable (built by `cargo run -p xtask -- package`,
`xtask/src/main.rs:764`), a daemon config YAML, and a runtime-root — the
`SandboxDaemonInstaller` consumes all three (`daemon_install.rs:33-64`,
per-sandbox paths `:52-57`). The spec enumerates them so an operator knows exactly
what a conforming attach gateway must be started with.

## Implementation Phases

- **Phase 0 — Scaffold the crate.** Add `"crates/sandbox-e2e-live-test"` to
  `Cargo.toml` `members` *and*, in the same change, create `Cargo.toml`,
  `build.rs`, `src/lib.rs`, `src/bin/eos-e2e.rs` stubs and the empty `tests/`
  tree, so the workspace still builds. Verify:
  `cargo build -p sandbox-e2e-live-test`.
- **Phase 1 — Harness core + one operation.** `config.rs`, `cli_client.rs`,
  `fixtures.rs` (`provision_sandbox` reading `/id`, RAII `Sandbox`),
  `tests/support/mod.rs`, `gateway.rs` (attach mode via `--gateway-socket`),
  `build.rs` include generation, and one leaf test
  `tests/runtime/command/exec_command/one_shot.rs` plus
  `tests/manager/lifecycle/create_sandbox/returns_ready.rs`. Verify against a gateway wired with
  the real Docker runtime by setting `EOS_E2E_RUN_ROOT` (with a `run-manifest.json`
  inside) and running `cargo test -p sandbox-e2e-live-test -- --test-threads=1`.
- **Phase 2 — Full per-operation tree + assertions.** All leaf files under
  `tests/manager/...` and `tests/runtime/...` covering M1-M5, R1-R8, plus
  `routing/scope_and_dispatch/` for N1-N2; `assertion.rs` helpers
  (`err_kind_at`, `err_detail`, `non_decreasing`); per-test `exchange.jsonl`
  capture.
- **Phase 3 — Orchestrator, reproducibility, artifacts, cleanup.**
  `src/bin/eos-e2e.rs` (preflight → build → attach gateway → `cargo test` →
  aggregate from `result.json`), deterministic run paths, captured-id cleanup,
  `report.rs` (`summary.json` with timing + cleanup sub-objects), RAII cleanup
  guard, `--rerun-failed-from`.
- **Phase 4 — Observability monitoring.** `report.rs` polling +
  `observability.json`; assertions over existing daemon spans; consume P1 (cgroup
  CPU/mem) and P2 (queue-wait) once they surface in the tree.

### Two-stage delivery during the runtime migration

While `sandbox-runtime` is mid-migration its CLI operations (the R-series —
`exec_command`, `write_command_stdin`, `read_command_lines`,
`create_workspace_session`, `destroy_workspace_session`, `squash`) cannot be
driven, so the phases above ship in two stages split on that one fault line.
**Stage 1** delivers everything that never drives a runtime op: Phase 0; the
Phase 1 harness with M1 green (the R1 leaf compiles and ships but is dormant); the
manager half of Phase 2 (M2–M5, N1, `err_kind_at`); all of Phase 3 with the
orchestrator's default test target pinned to `cargo test --test manager`; and the
snapshot/P1 half of Phase 4. **Stage 2** resumes once the migrated runtime serves
the R-series: green R1, author R2–R8 and N2 plus the runtime assertion helpers
(`err_detail`, `offsets_monotonic`, `non_decreasing`), consume P2 and runtime
command traces, and flip the orchestrator default to the full suite. The only
structural change at the boundary is that default test target — every runtime leaf
is an additive file the generated include list discovers (the
"add-a-test-case = add one file" invariant). The gate is binary-level, not
per-leaf: the sole skip path is `EOS_E2E_RUN_ROOT` unset, so a runtime leaf driven
against a not-yet-migrated runtime would *fail* (`operation_failed`), never skip —
hence Stage 1 keeps the runtime binary out of the green target instead of adding a
runtime-readiness skip guard. Manager provisioning is assumed live; if it is also
down, Stage 1 stays code-complete and skip-safe and only its green proof waits. See
`sandbox-e2e-live-test-phases-note.md` → *Two-stage delivery (runtime-migration
gate)* for the per-phase map.

## Verification Commands

```sh
cargo build  -p sandbox-e2e-live-test
cargo clippy -p sandbox-e2e-live-test --all-targets -- -D warnings

# Tests require the orchestrator to set up the run env. Either run via the
# orchestrator (PROOF below), or set EOS_E2E_RUN_ROOT (with a run-manifest.json
# inside, pointing at a real-runtime gateway socket) for a focused run:
EOS_E2E_RUN_ROOT=<dir> \
  cargo test -p sandbox-e2e-live-test --test runtime -- command_exec_command --test-threads=4

# PROOF (self-contained; no Makefile in repo). Requires a Linux host with Docker
# and an externally started gateway wired with the real Docker runtime, attached
# via --gateway-socket. The orchestrator preflights, builds, runs cargo test at
# the chosen concurrency, aggregates, cleans up. (Spawn mode — no --gateway-socket
# — is deferred until the real-runtime gateway ships; see Open Items #1.)
cargo run -p sandbox-e2e-live-test --bin eos-e2e --profile package-fast -- \
    --gateway-socket /path/to/real-runtime-gateway.sock \
    --run-id "$(git rev-parse --short HEAD)-proof" \
    --image ubuntu:24.04 \
    --max-parallel 8 \
    --report

# Focused rerun of only failed tests (fresh, independently cleanable namespace):
cargo run -p sandbox-e2e-live-test --bin eos-e2e -- \
    --gateway-socket /path/to/real-runtime-gateway.sock \
    --rerun-failed-from "$TMPDIR/eos-e2e/<run_id>/summary.json" \
    --max-parallel 4

# On any failure the orchestrator prints the exact focused-rerun line, e.g.:
#   EOS_E2E_RUN_ROOT=<dir> cargo test -p sandbox-e2e-live-test --test runtime -- <name>
```

## Open Items (carried, not blockers)

1. **Real Docker-runtime gateway wiring (hard prerequisite, unshipped).** The
   shipped `sandbox-gateway` wires `Unconfigured*` stubs
   (`crates/sandbox-gateway/src/gateway/main.rs:94-146`), so the live suite is
   non-executable until a gateway with a configured `SandboxRuntime` +
   `SandboxDaemonInstaller` is started externally and attached via
   `--gateway-socket`. Spawn mode and `package-fast` binary discovery
   (`CARGO_BIN_EXE_*` vs `target/{profile}/...`) are deferred to this work.
2. **Docker run-label backstop (deferred).** A label-based orphan reaper for
   hard-killed runs requires the runtime to accept a run-id label through
   `create_sandbox` and apply it as a container label — neither the CLI args nor
   `CreateSandboxRequest` carry one today (`runtime.rs:6-14`). Until then cleanup
   relies on captured ids + path namespacing, and a SIGKILL mid-run can leak
   containers. This is a runtime change tied to item #1, not part of this crate.
