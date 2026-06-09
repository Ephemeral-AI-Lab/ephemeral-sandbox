# sandbox/ Structure & Reduction Review — SPEC

Status: Proposed (Draft 2026-06-09). A workspace-wide aggressive-but-clean pass,
adversarially verified against the dependency-DAG guards. Baseline: current HEAD
(the `eos-daemon → eos-sandbox-host` extraction in
`eos_daemon_sandbox_host_extraction_SPEC.md` is **proposed, not landed**, so the
daemon is counted at its current 50 modules).
Owner: sandbox (workspace architecture)
Related: `eos_daemon_sandbox_host_extraction_SPEC.md` (interaction in §6),
`eos_daemon_srp_optimization_PLAN.md` (the dead-code sweeps this builds on).

## 0. The honest headline (read first)

**The workspace is already dense and clean (~117 src LOC/type), and recent audits
already swept the obvious dead code.** So the aggressive *count* reduction the brief
asked for is **not available without regressing a contract, the DAG, or a load-bearing
port.** The adversarially-confirmed TRUE-DELETION surface is tiny:

> **0 types · −2 methods · −1 field · 0 modules** removed (≈ 0.19% of items,
> 0.38% of methods, 0% of types) — a dead error variant, a dead accessor, and a
> duplicated test poller. **Zero types are deletable**, so there is no type-density
> win to harvest.

**The real cleaner-structure lever is a different axis — cohesion — and it raises the
module count (+5 net), not lowers it.** Splitting one mega-file (902 LOC) and three
routing-bloated daemon `mod.rs` files (484 / 370 / 358 LOC) trades four oversized
mixed-concern files for more small, single-concept, routing-honest modules. **That is
the product of this pass.** A reduction review whose module count *rises* must say so
plainly; this one does.

The gate throughout is **cleaner structure**, never a smaller number: every item below
passes the workspace-guard DAG, keeps typed contracts, and clears the repo's own
800-LOC mega / 200-LOC routing smells.

## 1. Reduction table — two columns, never summed

`TRUE DELETION` lowers the workspace total. `RELOCATION/INLINE/MERGE` redistributes
within a crate; the total is unchanged. They are not addable.

| graphify unit | TRUE DELETION (total drops) | RELOCATION / INLINE / MERGE (redistribution) |
|---|---|---|
| types | 0 | 0 |
| methods | **−2** (`OverlayMount::workspace_root`; e2e `wait_for_active_leases` dup) | **−1** (daemon skip-list lockstep merge) |
| fields | **−1** (`DaemonError::UnknownOp` variant) | 0 |
| modules | 0 | **−1** (plugin `registry.rs` fold) **+6** (4 file splits) = **+5 net** |
| src LOC | ~−45 | ~0 net (logic moves; ~6 new file headers) |

Baseline (graphify): 194 modules · 1,613 items · 526 methods · ~303 types · 35.4k src LOC.

## 2. Verified findings (each spot-checked with whole-repo grep)

| id | crate | finding | tag | evidence |
|---|---|---|---|---|
| **D1** | eos-daemon | `DaemonError::UnknownOp(String)` + its `wire_kind` arm are dead | TRUE_DELETION (−1 field) | zero constructors repo-wide; the dispatcher builds `ErrorKind::UnknownOp` directly (`dispatch/dispatcher.rs:222`). `ErrorKind` enum untouched → no wire change. `DaemonError` is `#[non_exhaustive]`. |
| **OV‑F1** | eos-overlay | `OverlayMount::workspace_root()` accessor is dead | TRUE_DELETION (−1 method) | zero callers (`kernel_mount.rs:64`); the `workspace_root` *field* stays (read by `Drop`). The 10+ `.workspace_root()` grep hits are all `NodeLease::workspace_root()` in e2e — a different type. |
| **E2E‑F2** | eos-e2e-test | local `wait_for_active_leases` duplicates `support::wait_for_active_leases` | TRUE_DELETION (−1 method) | near-verbatim of `support/mod.rs:27`. **Behavior caveat: the local copy's deadline is 3s vs support's 5s — confirm the loosening is acceptable before deleting.** |
| **D2** | eos-daemon | `should_emit_tool_call_event` (`transport/tool_call_events.rs`) is the exact complement of `skip_dispatch_audit` (`audit/events.rs`) | MERGE (−1 method) | two hand-synced op lists (same prefix + same 4 ops, flipped polarity) for the started/completed halves of one audit lifecycle. Collapse to one predicate so the lockstep is structural, not a convention a future edit can desync. |
| **PLUG‑registry** | eos-plugin | fold the export-less, mis-named 39-LOC `registry.rs` | MERGE (−1 module) | two `pub(crate)` fns (`public_op_name`, `is_valid_plugin_name`); the name collides with the unrelated `service_registry.rs` (108 LOC). 2 callers (`service.rs:10`, `host/ensure_args.rs:14`) re-point; drop `pub mod registry;` (`lib.rs:42`). |
| **D3** | eos-daemon | 3 audit-only cast helpers (`usize_to_i64_saturating`, `u64_to_usize_saturating`, `f64_to_i64_rounded_saturating`) live in `response_timings.rs` but every caller is in `audit/` + `ops/audit.rs` | RELOCATION (0 Δ) | cohesion-only; **optional, lowest priority.** |

## 3. Structural splits (the real win — cohesion; module count rises)

### 3a. `eos-e2e-test` — split the 902-LOC mega-file at its stateful/stateless seam

```text
src/
  lib.rs  audit.rs  cas.rs  client.rs  pool.rs  bin/e2e-reap.rs
  container.rs   # 902 -> ~430: DaemonContainer lifecycle only (impl at :43)
  docker.rs      # NEW ~340: docker()/exec/put_archive/http/percent_encode/parse_published_addr (seam at :416 `fn docker`)
  tar.rs         # NEW ~130: tar_single_file/write_octal/write_checksum + their unit tests
```

### 3b. `eos-daemon` — restore routing-only role to three `mod.rs` files

```text
adapters/workspace_run/isolated/
  mod.rs     # 484 -> ~60: mod decls + pub op_* dispatch entry points
  state.rs   # NEW ~210: DaemonIsolatedState, ensure_state, with_state/lock_state_cell, config cells, resource_caps_from_config
  errors.rs  # NEW ~70: setup_error/error_payload/error_json + require_arg/env_true
  runtime.rs ns_runner.rs   (unchanged)

adapters/plugins/
  mod.rs     # 370 -> ~70: mod decls + dispatch_registered_op + op_ensure/op_status routing
  setup.rs   # NEW ~120: config cells, ppc_socket_root, ensure_plugin_family_allowed, package_report_value, record_setup_failure, stop_services_for_layer_stack_root
  (connected/dispatch/occ_callbacks/overlay/process/refresh/service/state unchanged)

adapters/overlay/
  mod.rs     # 358 (~316 prod) -> ~70: DaemonPublisherPort + WorkspacePublisherPort impl + role
  convert.rs # NEW ~180: manifest_from_snapshot, path_changes_to_wire, changeset<->publish mapping, file_result_{to,from}_value, *_daemon_error  (+ the inline overlay tests)
```

### 3c. Module before → after

| crate | before | after | Δ | column |
|---|---:|---:|---:|---|
| eos-e2e-test | 7 | 9 | +2 | RELOCATION |
| eos-daemon | 50 | 54 | +4 | RELOCATION |
| eos-plugin | n | n−1 | −1 | MERGE |
| **net** | | | **+5** | |

Untouched crates also gain narrowings with **no file move**: `eos-layerstack` makes
`LayerStackLeaseRecord`/`LeaseRegistry` `pub(crate)` (`lib.rs:54`, zero external
referrers) — surface-narrowing, not deletion.

### 3d. Complete resulting workspace tree (all 17 crates)

The full end-state. **3 crates change file layout** (`eos-e2e-test`, `eos-daemon`,
`eos-plugin`); the other 14 keep their layout (some lose a dead method/variant or
narrow an export — no file moves). No crate added/removed; no new dependency edge.

```text
crates/eos-e2e-test/src/            # 7 -> 9 modules
  lib.rs  audit.rs  cas.rs  client.rs  pool.rs  bin/e2e-reap.rs
  container.rs                       # 902 -> ~430  (DaemonContainer lifecycle only)
  docker.rs                          # NEW ~340  <- split from container.rs (docker/exec/http/tar-driver/encode)
  tar.rs                             # NEW ~130  <- split from container.rs (ustar builder + unit tests)

crates/eos-daemon/src/              # 50 -> 54 modules
  lib.rs
  transport/   { framing, server, tool_call_events, mod }.rs   # D2: skip-list predicate consolidated (no file Δ)
  dispatch/    { dispatcher, mod }.rs
  runtime/     { error, invocation_registry, request_args, response_timings, mod }.rs
                                     #   error.rs: dead DaemonError::UnknownOp variant removed (D1)
                                     #   response_timings.rs: 3 audit-only casts -> audit/ (opt. D3)
  audit/       { buffer, events, mod }.rs
  ops/         { audit, checkpoint, command_sessions, control, files,
                 isolated_workspace, plugins, registry, workspace_run, mod }.rs
  adapters/
    mod.rs  checkpoint.rs
    occ/         { mod, service_cache }.rs
    overlay/
      mod.rs                         # 358 -> ~70  (DaemonPublisherPort + role only)
      convert.rs                     # NEW ~180  <- split from mod.rs (wire<->domain mapping + inline tests)
    workspace/   { file_ports, mod }.rs
    workspace_run/
      mod.rs  commands.rs  cancel.rs  wire.rs  config.rs  host_ports.rs
      isolated/
        mod.rs                       # 484 -> ~60  (mod decls + pub op_* dispatch only)
        state.rs                     # NEW ~210  <- split (DaemonIsolatedState, ensure_state, cells, caps)
        errors.rs                    # NEW ~70   <- split (error_json/payload, require_arg, env_true)
        ns_runner.rs  runtime.rs
    plugins/
      mod.rs                         # 370 -> ~70  (mod decls + dispatch_registered_op + op_ensure/op_status)
      setup.rs                       # NEW ~120  <- split (config cells, ensure_plugin_family_allowed, setup helpers)
      connected.rs  dispatch.rs  occ_callbacks.rs  overlay.rs
      process.rs  refresh.rs  service.rs  state.rs

crates/eos-plugin/src/              # n -> n-1 modules
  error.rs  lib.rs  manifest.rs  ppc.rs  refresh.rs  service.rs  service_registry.rs
  registry.rs                        # REMOVED (folded: public_op_name/is_valid_plugin_name -> service.rs / host/ensure_args.rs)
  host/  { mod, ensure_args, package, route, ppc_client }.rs + ppc_client/{ frame_io, pending }.rs

# ───────── UNCHANGED file layout (14 crates) ─────────
crates/eos-overlay/src/     { error, kernel_mount, lib, path_change, writable_dirs }.rs
                            #   kernel_mount.rs: dead OverlayMount::workspace_root() accessor removed (no file Δ)
crates/eos-layerstack/src/  { error, fsutil, lease, lib, metrics, squash, storage_lock,
                              workspace_base, workspace_binding, stack }.rs
                            + stack/{ fs, manifest_io, projection, whiteout }.rs
                            #   lib.rs: LeaseRegistry/LayerStackLeaseRecord -> pub(crate) (no file Δ)
crates/eos-protocol/src/    { audit, canonical, cas, envelope, ids, lib, models, ops, version }.rs   # OFF-LIMITS contract
crates/eos-config/src/      { document, error, lib, merge, paths }.rs
                            + configs/{ command-session, daemon, e2e-test, isolated-workspace, mod, runner, validate }.rs   # OFF-LIMITS contract
crates/eos-occ/src/         { commit_queue, error, lib, route, service }.rs
crates/eos-occ-layerstack/src/ { lib, publish, route }.rs
crates/eos-checkpoint/src/  { commit, lib }.rs
crates/eos-command-session/src/ { error, lib, output, request, response, session, transcript, wait }.rs
                            + process/{ mod, pty, runner, signal }.rs
crates/eos-workspace/src/   { command_session, file_ops, lease, lib, mode, mutation, read_view, response }.rs
crates/eos-workspace-run/src/ { command_handle, lib, manager, ports, registry }.rs
crates/eos-workspace-modes/src/ ephemeral/{ capture, command, dirs, error, finalize, mod, ops, ports, timings, types }.rs
                            isolated/{ audit, caps, command, error, mod, network, ops, session }.rs
                            isolated/network/{ rtnl, netfilter/{ exprs, mod, wire } }.rs
                            isolated/session/{ capacity, gc, lifecycle, persistence, ports, support, types }.rs  lib.rs
crates/eos-runner/src/      { error, fresh_ns, lib, mount_mask, path, request, setns }.rs + fresh_ns/{ child, command }.rs
crates/eos-ns-holder/src/   { handshake, lib, namespace, network }.rs
crates/eosd/src/            { daemon, main, runner }.rs
```

End-state guarantees: same 17 crates, no new dependency edge, **zero files > ~800 LOC**,
**no `mod.rs` > ~200 production LOC**, workspace modules **194 → 199 (+5)**.

> If the `eos-sandbox-host` extraction also lands, the entire `eos-daemon/src/adapters/`
> subtree above (now in clean per-concept seams) relocates wholesale into the new
> `eos-sandbox-host` crate — see `eos_daemon_sandbox_host_extraction_SPEC.md` §3.5; these
> splits make that move mechanical rather than a re-split.

## 4. Already clean — UNTOUCHED (12 crates) and why

`eos-protocol`, `eos-config`, `eos-workspace`, `eos-ns-holder`, `eos-occ`,
`eos-occ-layerstack`, `eos-checkpoint`, `eosd`, `eos-runner`, `eos-command-session`,
`eos-workspace-modes`, `eos-workspace-run`.

- `eos-layerstack/stack.rs` (748): cohesive `LayerStack`+`MergedView`+`Lease` state
  machine, under 800, already factored into 4 children — splitting fragments one type.
- `eos-workspace-modes/isolated/session/{7 files}`: 7 distinct concepts; flattening
  yields a ~1,400-LOC subtree. Leave. Same for `isolated/network/netfilter/{3}`.
- `eos-command-session/process/{pty,signal}`: each wraps a distinct OS primitive.
- `eos-config/lib.rs` (276): ~46 routing LOC + ~230 inline tests — routing-clean.

## 5. Deliberately NOT touched (the aggressive moves that fail the gate)

| item | verdict | why |
|---|---|---|
| `usize_to_f64_saturating` ×5 cross-crate dedup | REJECT | occ/occ-layerstack/checkpoint saturate at **`u32::MAX`**, eos-workspace at **`f64::MAX`** — consolidating *changes* CAS/commit metric semantics; also needs 3 backward DAG edges. Not a clean win. |
| inline `WorkspaceFileOps` | REJECT | cross-crate trait, 3-crate span (def eos-workspace, impl eos-workspace-modes ×2, call eos-daemon) — fails the single-crate inline gate. |
| reduce `eos-protocol` / `eos-config` fields/types | OFF-LIMITS | mirror the wire protocol / `prd.yml` — high counts are correct contracts. |
| inline `MountIo` / `SyscallResult` (1-impl) | KEEP | extension traits on a foreign `rustix` `Result` — can't add inherent methods to a foreign type; idiomatic. |
| the 7 1-impl injection ports (`AuditSink`, `LayerStackSnapshotPort`, `NamespaceRuntimePort`, `WorkspacePublisherPort`, `WorkspaceRunHostPorts`, …) | KEEP | load-bearing cross-crate ports with test-double impls; typed ports are preserved. |
| any crate merge | NOT TOUCHED | adds/removes DAG edges or cycles, or crosses the nix/leaf boundary. Out of scope for a within-crate cleanliness pass. |
| `eos-daemon → eos-sandbox-host` extraction | SEPARATE SPEC | wholesale adapter relocation handled elsewhere (see §6). |

## 6. Phasing

Phases A–E are pure within-crate relocations/narrowings (no DAG edge, no public-surface
change). F–I are the count-affecting deletions/merges. Each phase is independently green
and revertable.

| phase | content | crate | verify (green before next) |
|---|---|---|---|
| A | container split (+docker.rs +tar.rs) | eos-e2e-test | `cargo check -p eos-e2e-test --all-targets` |
| B | isolated/mod.rs split (+state.rs +errors.rs) | eos-daemon | `cargo check -p eos-daemon --all-targets` |
| C | plugins/mod.rs split (+setup.rs) | eos-daemon | `cargo check -p eos-daemon --all-targets` |
| D | overlay/mod.rs split (+convert.rs) | eos-daemon | `cargo check -p eos-daemon --all-targets` |
| E | plugin registry.rs fold (−1 module) | eos-plugin | `cargo check -p eos-plugin --all-targets` |
| F | delete dead `DaemonError::UnknownOp` | eos-daemon | `cargo test -p eos-daemon` |
| G | delete dead `OverlayMount::workspace_root` | eos-overlay | `cargo check -p eos-overlay --all-targets` |
| H | skip-list lockstep merge | eos-daemon | `cargo test -p eos-daemon` |
| I | e2e poller dedup (**confirm 3s→5s deadline first**) | eos-e2e-test | live E2E harness |
| opt J | D3 cast-helper relocation | eos-daemon | `cargo check -p eos-daemon --all-targets` |
| final | lint sweep | workspace | `cargo clippy --workspace --all-targets -- -D warnings` (report pre-existing noise, don't suppress) |

**Interaction with the host-extraction spec:** Phases B–D (and F, H) touch daemon
adapter internals that the host-extraction spec relocates *wholesale*. **Land B–D
first** — the per-concept seams (`state.rs`/`errors.rs`/`setup.rs`/`convert.rs` vs three
mixed 300–500-LOC `mod.rs` files) make the host spec's module moves mechanical rather
than a re-split.

## 7. Acceptance criteria

- No DAG change: `cargo tree` edge set identical before/after (no crate added/removed,
  no new internal edge). `workspace-guard` `dependency_dag` + `public_surface` green.
- No file exceeds ~800 LOC; no `mod.rs` exceeds ~200 production LOC.
- TRUE-DELETION column exactly `0 types / −2 methods / −1 field`; no contract
  (`eos-protocol`/`eos-config`) field or type removed.
- `cargo check --workspace --all-targets` green; `cargo clippy --workspace
  --all-targets -- -D warnings` clean (pre-existing noise reported, not suppressed).
- Behavior preserved: daemon + e2e suites green against a rebuilt `eosd`; the E2E
  poller-dedup deadline change explicitly signed off.
