# `sandbox-e2e-live-test` — Implementation Phases Note

Quick reference for what each phase delivers. Source of truth is
`docs/e2e/sandbox-e2e-live-test-spec.md` (`## Implementation Phases`); this note
expands each phase into deliverables, files, acceptance, and gates. Phases are
additive and ordered — each builds on the prior.

## At a glance

| Phase | Theme | Outcome | Live-gateway needed? |
|-------|-------|---------|----------------------|
| 0 | Scaffold the crate | Workspace builds with the new member | No |
| 1 | Harness core + one operation | Two leaf tests run (skip-safe; green vs real gateway) | Yes, to prove green |
| 2 | Full per-operation tree + assertions | All M/R/N leaves + assertion helpers | Yes, to prove green |
| 3 | Orchestrator + reproducibility + artifacts + cleanup | `eos-e2e` runs end-to-end, aggregates, cleans up | Yes, to prove green |
| 4 | Observability monitoring | Per-sandbox observability snapshots + P1/P2 consumption | Yes, to prove green |

**Standing gate (Phases 1–4):** a *green* live run needs an externally started
`sandbox-gateway` wired with the **real Docker runtime**, attached via
`--gateway-socket`. The shipped gateway wires `Unconfigured*` stubs that always
error (`crates/sandbox-gateway/src/gateway/main.rs:94-146`). Code for every phase
can be *built and unit-exercised* without it; only the live assertions are gated.
v1 is **attach-only** — spawn mode is deferred (Open Items #1).

---

## Two-stage delivery (runtime-migration gate)

The `sandbox-runtime` crates are mid-migration, so the **runtime CLI operations**
— the R-series `exec_command`, `write_command_stdin`, `read_command_lines`,
`create_workspace_session`, `destroy_workspace_session`, `squash` — cannot be
driven yet. The phases above are therefore delivered in **two stages**, split on
that one fault line. Manager lifecycle + observability provisioning is assumed
live (only the runtime *operations* are not ready); if provisioning is also down,
every Stage 1 item is still code-complete and skip-safe and only its *green proof*
waits for provisioning to return.

| Stage | Work it contains | Green when |
|-------|------------------|-----------|
| **Stage 1 — everything but runtime ops** | Phase 0; Phase 1 harness + M1 green (the R1 leaf ships but is dormant); Phase 2 manager half (M2–M5, N1, `err_kind_at`, `exchange.jsonl`); Phase 3 orchestrator + artifacts + cleanup with the default test target pinned to `--test manager`; Phase 4 snapshot poller + `observability.json` (+ P1 iff surfaced) | a gateway that provisions/destroys sandboxes and serves `get_observability_tree` |
| **Stage 2 — resume once runtime ops land** | Phase 1 remainder (green R1); Phase 2 runtime half (author R2–R8, N2, and `err_detail` / `offsets_monotonic` / `non_decreasing`); Phase 4 P2 queue-wait + runtime command traces; flip the orchestrator default to the full suite | the migrated real Docker runtime serves the R-series |

**Single stage-boundary switch.** The only structural change between the stages is
the orchestrator's default test target — Stage 1 invokes `cargo test --test
manager`, Stage 2 drops the restriction and runs the full suite (`--test manager`
*and* `--test runtime`). Everything else is **additive leaf files**: the generated
`#[path]` include list (`build.rs`) discovers each new `tests/runtime/**` leaf with
no orchestrator or registry edit — exactly the cross-phase "add-a-test-case = add
one file" invariant.

**Why a binary-level gate, not per-leaf skips.** A runtime leaf run against a
gateway whose runtime ops fail would *fail* (`operation_failed`), not skip — the
only skip path is `EOS_E2E_RUN_ROOT` unset (`tests/support/mod.rs`). So Stage 1
keeps the runtime binary out of the green target rather than adding a
runtime-readiness probe + skip guard (new state meaning "not implemented yet" —
avoided per *prefer less*). Only the existing R1 `one_shot.rs` smoke leaf is
authored pre-migration; R2–R8 are written in **Stage 2** against the migrated
runtime's settled response shapes, so the suite is not rewritten against a moving
target.

### Stage map per phase

- **Phase 0** — Stage 1 (done).
- **Phase 1** — Stage 1 for the harness core and M1 green. The **R1 green proof is
  the first Stage 2 item**: `one_shot.rs` already compiles and ships, it is just
  not in the Stage 1 green target.
- **Phase 2** — *split*. Stage 1: M2–M5, N1, `err_kind_at`. Stage 2: R1 green +
  authoring R2–R8, N2, and the runtime assertion helpers `err_detail`,
  `offsets_monotonic`, `non_decreasing`.
- **Phase 3** — Stage 1 in full; the one stage-aware line is the default test
  target (manager-only → full suite at the boundary).
- **Phase 4** — *split*. Stage 1: the snapshot poller, `observability.json`, and
  P1 (cgroup) consumption (`recent_traces` is present but carries little or no
  runtime-command activity pre-migration). Stage 2: P2 (namespace queue-wait) and
  runtime command traces.

---

## Phase 0 — Scaffold the crate

**Goal:** make `crates/sandbox-e2e-live-test` a real, building workspace member
without breaking the workspace.

**Deliverables**
- Add `"crates/sandbox-e2e-live-test"` to `Cargo.toml` `members` **and**, in the
  same change, create the crate so the build still passes.
- `Cargo.toml` (lib + `[[bin]] eos-e2e`; workspace deps via `dep.workspace = true`).
- `build.rs` skeleton (generates the per-leaf `#[path]` include list; tolerates an
  empty `tests/` tree).
- `src/lib.rs` (re-export surface stub), `src/bin/eos-e2e.rs` (stub main).
- Empty `tests/` tree skeleton (`support/`, `manager/`, `runtime/`).

**Acceptance**
- `cargo build -p sandbox-e2e-live-test` and a workspace-wide `cargo build` succeed.
- `cargo clippy -p sandbox-e2e-live-test --all-targets` passes the workspace lints.

**Out of scope:** any real harness logic, tests, or orchestration.

---

## Phase 1 — Harness core + one operation

**Goal:** the minimal black-box driving path end-to-end for a single op, proven
against a real gateway, and skip-safe without one.

**Deliverables (files → responsibility)**
- `src/config.rs` — minimal `RunConfig` + `run-manifest.json` load (only the fields
  fixtures need: schema_version, gateway socket, `run_id`, image).
- `src/cli_client.rs` — invoke `sandbox-cli`; capture the call record
  `{ argv, request_json?, response_json, exit_code, stdout, stderr, latency_ms }`;
  parse the single NDJSON response line; locate `error` on stdout-or-stderr.
- `src/fixtures.rs` — `Harness` (lazy; reads `EOS_E2E_RUN_ROOT` → manifest),
  `provision_sandbox(slug, image)` capturing the runtime-assigned `/id`, RAII
  `Sandbox` drop → `destroy_sandbox`.
- `src/gateway.rs` — **attach mode only**: validate/await `--gateway-socket`.
- `src/assertion.rs` — only the helpers the two leaves use (`ok`, `field`, and the
  exit/stream helper if exercised).
- `tests/support/mod.rs` — skip-safe harness entry (returns `Option`; tests
  early-return and record `skipped` when `EOS_E2E_RUN_ROOT` is unset).
- `build.rs` — generate `$OUT_DIR/<scope>_mods.rs` from `tests/<scope>/**/*.rs`.
- Leaf tests: `tests/manager/lifecycle/create_sandbox/returns_ready.rs` (M1),
  `tests/runtime/command/exec_command/one_shot.rs` (R1).

**Acceptance**
- Crate compiles; bare `cargo test -p sandbox-e2e-live-test` (no env) **skips
  cleanly**, no panic.
- With `EOS_E2E_RUN_ROOT` → a hand-written `run-manifest.json` for a real-runtime
  gateway, the **M1** leaf passes under `--test-threads=1`. The **R1** leaf
  (`one_shot.rs`) compiles and ships this phase, but its green proof is the first
  **Stage 2** item — runtime ops are mid-migration (see *Two-stage delivery
  (runtime-migration gate)*).

**Note:** `eos-e2e` stays a stub; the run env is set by hand this phase.

---

## Phase 2 — Full per-operation tree + assertions

**Goal:** complete black-box coverage of the public surface, one leaf file per test case.

**Deliverables**
- All manager leaves: `lifecycle/{create,inspect,list,destroy}_sandbox/<case>.rs`,
  `observability/get_observability_tree/<case>.rs` (M1–M5).
- All runtime leaves: `command/{exec_command,write_command_stdin,
  read_command_lines}/<case>.rs`, `workspace_session/{create,destroy}_workspace_session/<case>.rs`
  (clean + busy), `layerstack/squash/<case>.rs` (R1–R8).
- Negatives in `tests/<scope>/routing/scope_and_dispatch/<case>.rs` (N1 unknown system op
  → `unknown_op`/exit 1; N2 runtime op without sandbox id → exit 2/stderr).
- Full `assertion.rs`: `err_kind_at`, `err_detail` (runtime `operation_failed`
  details), `offsets_monotonic`, `non_decreasing`.
- Per-test `exchange.jsonl` capture.

**Staging:** the manager half (M2–M5, N1, `err_kind_at`) is **Stage 1**; the
runtime half (R1 green + authoring R2–R8, N2, and `err_detail` /
`offsets_monotonic` / `non_decreasing`) is **Stage 2**, written against the
migrated runtime's settled shapes. See *Two-stage delivery (runtime-migration gate)*.

**Acceptance:** all leaves compile; skip-safe without env; green vs a real gateway.
Stateful chains (R3/R5/R6) capture and round-trip `command_session_id`; conditional
rows (R7a/b, R8) drive deterministic fixtures.

**Out of scope:** orchestration, aggregation, cleanup automation, observability
polling.

---

## Phase 3 — Orchestrator, reproducibility, artifacts, cleanup

**Goal:** `eos-e2e` becomes the single command an operator/CI runs.

**Deliverables**
- `src/bin/eos-e2e.rs`: preflight (Linux → docker → image inspect → runtime probe)
  → build (or `BuildSource::Prebuilt`) → attach gateway → write `run-manifest.json`
  → `cargo test` → aggregate → cleanup.
- Aggregation from globbed `reports/*/result.json` + `cargo test` exit code (no
  libtest-stdout parsing); `summary.json` with `timing` + `cleanup` sub-objects;
  `schema_version` on every artifact.
- Deterministic `run_id`/paths; one `EOS_E2E_RUN_ROOT` contract.
- Cleanup: captured-id `destroy_sandbox` + `remove_dir_all(run_root)`, RAII guard,
  `CleanupPolicy`, `--clean-run`, `--rerun-failed-from`.
- Daemon-binary/gateway-config prerequisites enumerated for the attach gateway.

**Acceptance:** `cargo run --bin eos-e2e -- --gateway-socket <real> --image … `
runs the full suite, produces the artifact tree, and cleans up per policy.

**Known limit:** no orphan-reaping on hard kill (SIGKILL/abort) — the Docker-label
backstop is deferred (Open Items #2).

---

## Phase 4 — Observability monitoring

**Goal:** turn the runner from correctness-only into performance-aware.

**Deliverables**
- `report.rs` polls `get_observability_tree` during the run; writes per-sandbox
  `observability.json` (latest tree node + bounded recent-trace summaries).
- Assertions over existing daemon spans surfaced in the tree.
- Consume **P1** (cgroup CPU/mem) and **P2** (namespace queue-wait) automatically
  *iff* they surface in the tree (additive builder); their P1/P2 unit assertions
  live in the daemon crates, not here (the `*_for_test` SQLite path is off-limits
  by the black-box boundary).

**Staging:** the snapshot poller, `observability.json`, and P1 (cgroup) consumption
are **Stage 1** (pre-runtime, `recent_traces` is present but carries little or no
runtime-command activity); P2 (namespace queue-wait) and runtime command traces are
**Stage 2**. See *Two-stage delivery (runtime-migration gate)*.

**Acceptance:** observability snapshots written per sandbox; absence of P1/P2 only
lowers diagnostic resolution, never blocks the run.

**Out of scope (permanently):** manager-side observability sink or a second
classification axis; gateway/manager/forwarding spans + manager trace store.

---

## Cross-phase invariants (hold from Phase 1 on)

- Black-box only: all sandbox/runtime ops via `sandbox-cli` over the gateway socket.
- Runtime-assigned sandbox ids, captured from the create response and round-tripped.
- Single env contract `EOS_E2E_RUN_ROOT`; everything else from `run-manifest.json`.
- Add-a-test-case = add one leaf file (the `#[path]` include list is generated).
- Linux + Docker only; off-Linux/no-Docker exits `2` at preflight.
