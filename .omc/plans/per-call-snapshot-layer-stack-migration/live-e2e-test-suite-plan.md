# Live E2E Test Suite - Per-Call Snapshot Layer Stack

Companion plan to `per-call-snapshot-layer-stack.md`. Defines the sandbox-only
live test shape for the per-call snapshot layer stack migration. **Plan only -
implementation lands in test files after this document is correct.**

Rooted at `backend/tests/live_e2e_test/sandbox/`. The live suite is opt-in by
directory and must run against this exact Daytona image:

```bash
EPHEMERALOS_SANDBOX_DEFAULT_IMAGE=registry:6000/daytona/sweevo-psf-requests-3738:v1
```

The live suite must never create host-local layer-stack/OCC/overlay state.
Every behavior under test runs **inside the sandbox**, in one of two shapes:

1. **Public tool boundary** — drives `backend/src/sandbox/api/tool/`
   verbs (`read_file`, `write_file`, `edit_file`, `shell`).
2. **Native subsystem probe** — drives `sandbox.layer_stack`,
   `sandbox.occ`, or `sandbox.overlay` from a probe script that runs
   *inside the Daytona sandbox*, importing the deployed runtime bundle at
   `/tmp/eos-sandbox-runtime/`.

Both shapes are required. The pytest process never imports those modules
directly — that is enforced by the import fence in `conftest.py`.

---

## 1. Goals And Non-Goals

**Goals**

1. Validate the layer-stack migration against a real Daytona sandbox brought up
   by `sandbox.control.ops.setup.setup_after_create`.
2. Keep all live behavior inside the sandbox created from
   `registry:6000/daytona/sweevo-psf-requests-3738:v1`.
3. Cover `sandbox.layer_stack`, `sandbox.occ`, and `sandbox.overlay`
   natively — functionality, resource consumption, performance under load,
   and edge cases — by running probe scripts inside the sandbox that import
   the modules from the deployed runtime bundle.
4. Drive integrated mutation behavior through `backend/src/sandbox/api/tool/`:
   `read_file`, `write_file`, `edit_file`, and `shell`.
5. Preserve the real production boundary:
   `runtime.overlay_shell.capture_to_changeset -> occ.client.OCCClient ->
   occ.service.OccService`.
6. Treat gitignored/LWW routing as an OCC policy decision from
   `sandbox.occ.content.gitignore_oracle.GitignoreOracle`, evaluated against
   the sandbox workspace snapshot. Tests must not fake this with caller-supplied
   `ignored_paths`.
7. Map every migration experiment (E1-E13) to at least one native probe **and**
   one integrated assertion. Native and integrated coverage are complementary:
   the native suite catches subsystem regressions early; the integrated suite
   catches wiring breakage that lets a green native suite ship a broken
   product.

**Non-goals**

- No pytest-process-local construction of `LayerStackManager`, `OccService`,
  `OverlayClient`, `SerialMerger`, `Publisher`, or any other runtime object.
- No direct imports of `sandbox.layer_stack`, `sandbox.overlay`, or
  `sandbox.occ` from any file under `backend/tests/live_e2e_test/`. The
  fence in `conftest.py` rejects collection.
- No caller-declared "ignored path" policy in public API tests. Gitignore
  classification belongs to OCC's `GitignoreOracle`.
- No cross-host or multi-sandbox concurrency. Live load is single sandbox, many
  async callers (host-side) or many in-sandbox processes (probe-side).
- No replacement of unit tests under `backend/tests/unit_test/test_sandbox/`.
  Unit tests remain the place for direct in-process internal coverage.

---

## 2. Directory Layout

```
backend/tests/live_e2e_test/sandbox/
|-- README.md
|-- load_testing_standard.md
|-- _harness/
|   |-- sandbox_fixture.py      # real Daytona sandbox fixture
|   |-- overlay_probe.py        # raw mount(2) syscall probes (kernel boundary)
|   |-- native_probe.py         # in-sandbox runtime-bundle probe wrappers
|   |-- resource_metrics.py     # /proc samplers (fd, RSS, inodes, mounts)
|   |-- concurrency.py          # async fan-out helpers
|   `-- load_profiles.py        # smoke / sustained / burst / soak shapes
|-- overlay/
|   |-- syscall/                # P0 — kernel-boundary probes (existing)
|   |   |-- test_mount_depth.py
|   |   |-- test_snapshot_latency.py
|   |   |-- test_read_latency.py
|   |   |-- test_concurrent_mounts.py
|   |   `-- test_heavy_write_copy_up.py
|   `-- native/                 # P0/P1 — sandbox.overlay module probes
|       |-- test_snapshot_overlay_runner.py     # P0
|       |-- test_namespace_command.py           # P1
|       |-- test_namespace_mounts.py            # P1
|       |-- test_capture_upperdir.py            # P0
|       |-- test_capture_changes.py             # P0
|       |-- test_runtime_invoker.py             # P1
|       |-- test_overlay_runner_load.py         # P1
|       |-- test_overlay_resource.py            # P1
|       `-- test_overlay_edge_cases.py          # P1
|-- layer_stack/                # P0/P1 — sandbox.layer_stack module probes
|   |-- test_manifest_lifecycle.py              # P0
|   |-- test_publisher.py                       # P0
|   |-- test_merged_view.py                     # P0
|   |-- test_squash.py                          # P0
|   |-- test_changes_aggregation.py             # P0
|   |-- test_lease_registry.py                  # P0
|   |-- test_lease_budget.py                    # P0
|   |-- test_stack_manager_integration.py       # P0
|   |-- test_layer_stack_load.py                # P1
|   |-- test_layer_stack_resource.py            # P1
|   `-- test_layer_stack_edge_cases.py          # P1
|-- occ/                        # P0/P1 — sandbox.occ module probes
|   |-- test_orchestrator.py                    # P0
|   |-- test_serial_merger.py                   # P0
|   |-- test_commit_transaction.py              # P0
|   |-- test_routing.py                         # P0
|   |-- test_content_gitignore_oracle.py        # P0
|   |-- test_occ_skipped_merge.py               # P0
|   |-- test_gated_route.py                     # P0
|   |-- test_merge_engine.py                    # P0
|   |-- test_patching.py                        # P1
|   |-- test_changeset_model.py                 # P1
|   |-- test_overlay_capture_to_changeset.py    # P0
|   |-- test_occ_load.py                        # P1
|   |-- test_occ_resource.py                    # P1
|   `-- test_occ_edge_cases.py                  # P1
`-- layer_stack_overlay_occ/    # public-tool integrated boundary
    |-- test_public_tool_runtime_smoke.py
    |-- test_shell_call_isolation.py
    |-- test_concurrent_agents.py
    |-- test_codegen_race.py
    |-- test_failure_recovery.py
    `-- test_load_profiles.py
```

**Tagging.** P0 = required for cutover (functional invariants). P1 = required
for full migration sign-off (resource + load + edge cases). User can phase by
landing P0 first, P1 second.

**On the existing overlay/ tree.** The current tests under `overlay/` measure
direct `mount(2)` syscall behavior, not the `sandbox.overlay` module. Renamed
to `overlay/syscall/` for clarity. Module-level probes live under
`overlay/native/`.

**Why `layer_stack/` and `occ/` exist now.** They were forbidden in the
previous revision because all known coverage modes were host-local. Native
in-sandbox probing through the runtime bundle is a new third mode. It runs
inside the sandbox, imports the modules from the bundle, and emits structured
JSON back through `raw_exec`. The pytest process still never imports
`sandbox.layer_stack` or `sandbox.occ`.

---

## 3. Sandbox Harness Contract

### 3.1 `SandboxHandle`

| Field | Purpose |
|---|---|
| `sandbox_id` | Provider id for the real Daytona sandbox. |
| `workspace_root` | Always `/testbed`. |
| `caller` | `SandboxCaller` identity for public tool calls. |
| `raw_exec` | Public raw-exec transport for in-sandbox probes only. |
| `tool` | Bound public verbs: `read_file`, `write_file`, `edit_file`, `shell`. |

The handle must not expose `LayerStackManager`, `OccService`, or
`OverlayClient` objects to live tests.

### 3.2 Fixture Surface

| Fixture | Scope | Purpose |
|---|---|---|
| `live_sandbox` | session | One Daytona sandbox from the required image. |
| `overlay_sandbox` | function | Resets `/testbed`, purges `OVERLAY_ROOT`; for raw-syscall probes. |
| `native_sandbox` | function | Resets `/testbed` and `DEFAULT_LAYER_STACK_ROOT`; for runtime-bundle native probes. Confirms `/tmp/eos-sandbox-runtime/.bundle-hash` exists before yielding. |
| `integrated_sandbox` | function | Resets `/testbed` and runtime layer-stack state, exposes public tool verbs. |

### 3.3 Setup Sequence

```text
1. bootstrap_daytona_provider()
2. assert settings.sandbox.default_image ==
   registry:6000/daytona/sweevo-psf-requests-3738:v1
3. provider.create(name=..., image=settings.sandbox.default_image,
                   labels={"project_dir": "/testbed"})
4. register_adapter(sandbox_id, provider)
5. setup_after_create(sandbox_id, "/testbed")
        -> ensure_git
        -> runtime bundle upload (-> /tmp/eos-sandbox-runtime/)
        -> bootstrap_in_sandbox_runtime
6. yield SandboxHandle(...)
7. delete_sandbox(sandbox_id) in finally
```

### 3.4 Native Probe Contract

Native probes import the runtime bundle from inside the sandbox. The bundle is
extracted flat at `BUNDLE_REMOTE_DIR = "/tmp/eos-sandbox-runtime"` with
modules under `/tmp/eos-sandbox-runtime/sandbox/...`. Probes therefore run as:

```bash
cd /tmp/eos-sandbox-runtime && python3 -c "<probe source>"
```

`_harness/native_probe.py` provides the wrapper. Each probe:

- Receives one JSON config object (rendered into the source via
  `__CFG_JSON__`, same pattern as `overlay_probe.py`).
- Imports only from `sandbox.layer_stack`, `sandbox.occ`,
  `sandbox.overlay`, plus stdlib. No top-level imports of `sandbox.api.tool`
  or `sandbox.control` — those are host-side packages even when staged in
  the bundle.
- Emits exactly one trailing JSON line on stdout (`{"results": [...], ...}`).
- Includes a `resource` block in the JSON (see §3.5) for every probe whose
  pass bar references resources.
- Cleans up its own state under `/tmp/eos-sandbox-runtime/layer-stack/`
  before exiting (or relies on the function-scoped fixture reset).

**Namespace wrapping.** Probes that exercise namespace primitives directly
(`namespace.command.run_in_namespace`, `namespace.mounts.bind_mount`, or any
direct `mount(2)` call) are wrapped with `unshare -Urm` by
`native_probe.wrap_unshare`. Probes that drive
`runner.snapshot_overlay_runner` run un-wrapped because the runner spawns its
own namespaced child via `unshare -Urm` internally; double-wrapping breaks the
inner exec.

### 3.5 Resource Metrics

`_harness/resource_metrics.py` ships a small in-sandbox sampler embedded into
each probe. Every native probe emits this `resource` block alongside its
results:

```json
{
  "fd_open":     <int from len(os.listdir("/proc/self/fd"))>,
  "rss_kb":      <int from /proc/self/status VmRSS>,
  "rss_peak_kb": <int from /proc/self/status VmHWM>,
  "threads":     <int from /proc/self/status Threads>,
  "mounts":      <int from /proc/self/mounts line count>,
  "overlay_mounts": <int — lines containing " overlay ">,
  "inodes_used": <int from `df -i /tmp/eos-sandbox-runtime` Used>,
  "wall_ms":     <float — probe wall clock>,
  "cpu_user_ms": <float from os.times>,
  "cpu_sys_ms":  <float from os.times>
}
```

A probe records `resource_before` and `resource_after`, plus a `resource_peak`
sample taken at the workload midpoint. Pass bars reference deltas, not
absolute values.

**tmpfs caveat.** `/tmp` is tmpfs inside the Daytona image; `df -i` on tmpfs
returns `0` or `-` on some kernels. Treat an absent or zero `inodes_used`
value as N/A and skip that line of the budget; do not fail a probe on it.

### 3.6 Cleanup Invariants

- `delete_sandbox` always runs in `finally`.
- Every function-scoped fixture resets `/testbed` with
  `git reset --hard HEAD && git clean -fdx`.
- `native_sandbox` and `integrated_sandbox` clear
  `/tmp/eos-sandbox-runtime/layer-stack/` before each test.
- `overlay_sandbox` purges leaked overlay mounts under `OVERLAY_ROOT`.
- Tests that need gitignored paths create or modify `.gitignore` inside
  `/testbed`, then rely on OCC `GitignoreOracle` to classify paths from that
  sandbox snapshot.

---

## 4. Per-Suite Plan

### 4.1 `overlay/syscall/` — Kernel Boundary Probes

Boundary: `handle.raw_exec(...)` runs scripts from
`_harness/overlay_probe.py`. The pytest process must not import
`sandbox.overlay`. These prove kernel-level invariants the `sandbox.overlay`
module relies on.

| File | Backs | Test cases | Pass bar |
|---|---|---|---|
| `test_mount_depth.py` | E1, E1.1 | direct-syscall depths {1,5,10,30,50,80,100,200}; mount(8) negative control; unshare -Urm namespace isolation | `mount(2)` rc=0 at all depths; `mount(8)` documented failure |
| `test_snapshot_latency.py` | E2 | p99 mount(2) latency at depth 100; depth 200 overshoot; 1000-iter zero-failure sweep | p99 < 5 ms at depth 100; 0 mount failures |
| `test_read_latency.py` | E3 | warm read at depth 100; cold read at depth 50 | warm <= 2× baseline; cold <= 5× or skip with reason |
| `test_concurrent_mounts.py` | E2.1 | hold N mounts at depth 50 | N=100 concurrent mounts succeed |
| `test_heavy_write_copy_up.py` | E2.2 | copy-up at depth 100, 1000 files | p99 < 50 ms; throughput > 200 writes/s |

### 4.2 `overlay/native/` — `sandbox.overlay` Module Probes

Boundary: `_harness/native_probe.py` renders a probe that runs inside the
sandbox and imports `sandbox.overlay` from the bundle. The pytest process
runs `raw_exec`, parses JSON.

| File | Functionality | Resource | Edge cases |
|---|---|---|---|
| `test_snapshot_overlay_runner.py` | mount + run + unmount round-trip via `runner.snapshot_overlay_runner`; assert callee output captured | fd/mount delta = 0 after teardown | empty lower; lower with whiteouts; nested overlay; runner crash mid-run |
| `test_namespace_command.py` | `unshare -Urm` wrapping via `namespace.command` | RSS delta bounded | invalid namespace, missing CAP_SYS_ADMIN, signal propagation |
| `test_namespace_mounts.py` | mount tracking + cleanup-on-exit via `namespace.mounts` | mount delta = 0 even on abort | force-kill mid-mount; orphaned upperdir; double-umount |
| `test_capture_upperdir.py` | `capture.upperdir` walks upper, emits canonical changeset | inode count bounded | binary files, sparse files, symlinks, hardlinks, long paths, unicode names, empty upper |
| `test_capture_changes.py` | `capture.changes` aggregation, ordering, dedup | RSS bounded for 10k-entry upper | whiteouts, opaque dirs, rename detection, charset edge cases |
| `test_runtime_invoker.py` | `runner.runtime_invoker` IPC contract | fd delta = 0 | exec failure, stdout overflow, timeout, non-UTF8 output |
| `test_overlay_runner_load.py` | sustained snapshot fan-out (N runners parallel) | mount/fd deltas after run; peak RSS | N=20 concurrent runners no leaks |
| `test_overlay_resource.py` | dedicated resource regression | per-probe budgets (see §6.2) | n/a |
| `test_overlay_edge_cases.py` | catch-all | n/a | depth=0; depth=1; depth>cap; ENOSPC injection on workdir; EBUSY on lower swap; ENOMEM injection via cgroup; missing lowerdir; dirty workdir on second mount |

### 4.3 `layer_stack/` — `sandbox.layer_stack` Module Probes

Boundary: native probes import from `sandbox.layer_stack`. The probe writes
its layer-stack root under `/tmp/eos-sandbox-runtime/layer-stack-test-<pid>/`
to keep the runtime's actual root clean; the function-scoped fixture removes
it.

| File | Functionality | Resource | Edge cases |
|---|---|---|---|
| `test_manifest_lifecycle.py` | open / append / seal / list / load round-trip via `manifest`; survive process restart | fd delta = 0; manifest size bound | manifest at depth 0, 1, 100, 200; corrupted manifest; concurrent append |
| `test_publisher.py` | `publisher.publish_layer` atomicity; retry; idempotency under same digest | RSS delta bounded for 1k-entry layer | publish-then-kill: no dangling refs; double-publish: dedup; full-disk |
| `test_merged_view.py` | `merged_view` materializes correct path-to-content map at depth 100 | inode/RSS bounded | conflicting paths; whiteouts override; opaque dirs; deeply nested keys |
| `test_squash.py` | `squash` coalesces N shallow layers; correctness pre/post; idempotency | layer-count drop ratio | squash with no-ops; squash mid-publish; squash kill recovery |
| `test_changes_aggregation.py` | `changes` dedup + ordering invariants | bounded for 10k entries | duplicate paths; out-of-order writes; rename pairs |
| `test_lease_registry.py` | register / release / expire / killed-shell sweep | fd delta = 0 across 100 cycles | expired-but-not-released; double-release; concurrent register |
| `test_lease_budget.py` | budget enforcement; over-budget reject; budget refresh | n/a | budget=0, budget=1, budget=∞; off-by-one at boundary |
| `test_stack_manager_integration.py` | `stack_manager` end-to-end (mount + publish + squash + lease) | end-to-end resource delta | full happy path; failure injection at each phase |
| `test_layer_stack_load.py` | sustained publish/squash mix | per-profile resource ceiling | SUSTAINED + BURST profiles (subsystem variant — see §6.3) |
| `test_layer_stack_resource.py` | dedicated resource regression at depth 100, depth 200 | per-probe budgets (see §6.2) | n/a |
| `test_layer_stack_edge_cases.py` | catch-all | n/a | empty layer; layer with one whiteout; gigantic single file (>1 GiB); unicode + long paths; symlink loops; hardlink fanout |

### 4.4 `occ/` — `sandbox.occ` Module Probes

Boundary: native probes import from `sandbox.occ`. Tests that need git-ignore
behavior write a real `.gitignore` under `/testbed` and call
`GitignoreOracle` against that snapshot — never `ignored_paths=[...]`.

| File | Functionality | Resource | Edge cases |
|---|---|---|---|
| `test_orchestrator.py` | `orchestrator` happy / conflict / abort flow | RSS delta bounded | abort during merge; abort during commit; orchestrator restart |
| `test_serial_merger.py` | `serial_merger` ordering, fairness, no-starvation | queue depth bounded | empty queue; single waiter; priority interleave; cancel mid-wait |
| `test_commit_transaction.py` | `commit_transaction` atomicity; rollback on failure | fd / lock-wait bounded | crash mid-commit; partial fsync failure; quota exceeded |
| `test_routing.py` | `routing` `occ_skipped_merge` vs `occ_gated_merge` decision from gitignore classification | n/a | mixed payload; unknown route; route override priority |
| `test_content_gitignore_oracle.py` | real `git check-ignore` binding | n/a | nested .gitignore; negation rules; `!` re-include; case-folding fs |
| `test_occ_skipped_merge.py` | `occ_skipped_merge` commits with no contention | end-to-end resource delta | empty changeset; 10k-path changeset |
| `test_gated_route.py` | `gated.*` commits + conflict | wait-time distribution | first-commits-wins; both-reject; partial overlap |
| `test_merge_engine.py` | `merge.*` three-way merge correctness | bounded for 10k-line file | non-conflicting hunks; conflicting hunks; binary files; CRLF/LF |
| `test_patching.py` | `patching.*` apply + reject | n/a | apply success; reject hunk; whitespace-only diff; EOF-no-newline |
| `test_changeset_model.py` | `changeset` invariants | n/a | empty; max size; mixed add/modify/delete; unicode normalization |
| `test_overlay_capture_to_changeset.py` | `overlay_capture.capture_to_changeset` round-trip from a real overlay upper | inode delta bounded | overlay with whiteouts; opaque dirs; renames; mixed tracked/gitignored |
| `test_occ_load.py` | sustained concurrent commits | per-profile resource ceiling | SUSTAINED + BURST profiles (subsystem variant — see §6.3) |
| `test_occ_resource.py` | dedicated resource regression | per-probe budgets (see §6.2) | n/a |
| `test_occ_edge_cases.py` | catch-all | n/a | huge changeset (10k paths); empty; conflicting concurrent commits; gitignored partial commit; mixed tracked + gitignored; UTF-8 boundary; long paths |

### 4.5 `layer_stack_overlay_occ/` — Public Tool Integration

Boundary: tests use only `handle.tool.read_file/write_file/edit_file/shell`.
This proves the production wiring `public tool API -> sandbox runtime API ->
overlay shell snapshot/capture -> OCC changeset routing -> OCC commit
transaction -> layer-stack publish` is intact. **Required even when the
native suite is green** — see §1 goal 7.

| File | Backs | Test cases | Pass bar |
|---|---|---|---|
| `test_public_tool_runtime_smoke.py` | cutover smoke | `test_public_tools_commit_through_sandbox_runtime` | Write/edit/shell/read all commit through sandbox-runtime state |
| `test_shell_call_isolation.py` | drift | in-flight shell isolation; pre-edit view; first-commits-wins | leased snapshot upheld; one publish, one OCC conflict |
| `test_concurrent_agents.py` | E4 | sustained mixed shell+edit; replay; rejected-write absence; 50 % overlap | 0 correctness violations |
| `test_codegen_race.py` | E13 | tracked race rejects; gitignored race LWW; mixed partial-commit | tracked OCC-gated; gitignored LWW via `GitignoreOracle` |
| `test_failure_recovery.py` | E9, E12 | kill mid-publish; kill mid-squash; killed-shell lease cleanup | fsck 0 dangling; killed leases reaped |
| `test_load_profiles.py` | E7, E8 | smoke / sustained / burst / soak | §6.3 integrated budgets |

### 4.6 Gitignored Classification Rule

For E13, "gitignored" means:

```text
sandbox.occ.content.gitignore_oracle.GitignoreOracle
  -> git -C <sandbox snapshot/workspace> check-ignore ...
  -> OCC route decision occ_skipped_merge / LWW
```

The live test setup must create the ignore rule inside the sandbox workspace,
for example:

```text
write_file(".gitignore", "dist/\n")
shell/write/edit changes to dist/app.js
```

The test passes only if the runtime routes `dist/app.js` through OCC's
gitignore oracle and accepts concurrent LWW writes. A test-only parameter such
as `ignored_paths=["dist"]` is not valid live coverage for E13 because it
skips the OCC oracle.

---

## 5. Coverage Status

| Suite | Files planned | Active | Pending |
|---|---:|---:|---:|
| `overlay/syscall/` | 5 | 5 | 0 |
| `overlay/native/` | 9 | 0 | 9 (P0=3, P1=6) |
| `layer_stack/` | 11 | 0 | 11 (P0=8, P1=3) |
| `occ/` | 14 | 0 | 14 (P0=10, P1=4) |
| `layer_stack_overlay_occ/` | 6 | 1 | 5 |

P0 first; P1 lands before sign-off. Do not fill native coverage by adding
host-local module imports — the import fence rejects collection.

---

## 6. Load Testing Standard

### 6.1 Profile Definitions

Profiles are defined by `_harness/load_profiles.py`:

```text
LoadProfile(name, shells_per_sec, edits_per_sec, duration_s,
            overlap_ratio, gitignored_ratio, max_p99_ms, max_drift,
            max_emergency_depth_events)
```

The `gitignored_ratio` portion of an integrated workload must be implemented
by writing real `.gitignore` rules into the sandbox workspace and letting OCC
`GitignoreOracle` classify paths.

### 6.2 Per-Probe Resource Budgets

These cap *delta* between `resource_before` and `resource_after` on a single
probe run. Per-test ceilings can tighten but must not loosen.

| Subsystem | Δ fd_open | Δ overlay_mounts | Δ rss_kb (peak) | Δ inodes_used |
|---|---:|---:|---:|---:|
| `overlay/native/` | 0 | 0 | < 50_000 | < 5_000 |
| `layer_stack/` | 0 | 0 | < 100_000 | < 50_000 |
| `occ/` | 0 | 0 | < 100_000 | < 50_000 |

Steady-state RSS at probe end must return within 20 % of `resource_before`.
A probe that exceeds any budget fails — no soft "warning" flag.

### 6.3 Subsystem vs Integrated Load

Two distinct shapes of load coverage:

| Shape | Driver | Measures | Scope |
|---|---|---|---|
| **Subsystem load** (`*_load.py` under `overlay/native`, `layer_stack`, `occ`) | native probe inside sandbox | lock contention, manifest publish rate, squash coalesce ratio, serial-merger queue depth | one subsystem in isolation, no public API in the path |
| **Integrated load** (`layer_stack_overlay_occ/test_load_profiles.py`) | host-side fan-out through public tools | end-to-end p99, drift, accepted-vs-rejected-write reconciliation | full production wiring |

Subsystem load uses a separate dataclass — different fields, different shape.
Add to `_harness/load_profiles.py`:

```text
SubsystemLoadProfile(name, op, op_rate_per_sec, duration_s, concurrency,
                     payload_shape, max_p99_ms, max_resource_delta)
```

| File | `op` | rate (ops/s) | duration | concurrency | payload | p99 budget | extra invariants |
|---|---|---:|---:|---:|---|---:|---|
| `overlay/native/test_overlay_runner_load.py` | `runner.run_snapshot` | 20 | 30 s | 20 | empty mount + no-op cmd | 100 ms | mount delta = 0 after run |
| `layer_stack/test_layer_stack_load.py` | `manifest.append` + `publisher.publish` mix (1:1) | 100 | 60 s | 32 | 10-path layer | append p99 < 1 ms at depth 100; publish p99 < 50 ms; squash coalesce ≤ 20 layers/s |
| `occ/test_occ_load.py` | `orchestrator.commit` | 50 | 60 s | 16 | 5-path changeset, 50 % overlap | 200 ms | serial-merger queue depth ≤ 64; 0 starvation |

### 6.3.1 Subsystem Stress (push-to-failure / saturation)

Stress tests find the ceiling, not just confirm the envelope. Tagged P2 and
gated to nightly runs because they cost CI time. Pass bar is *graceful
degradation*, not a fixed budget.

| File | Pattern | Pass bar |
|---|---|---|
| `layer_stack/test_layer_stack_stress.py` | publish-rate ramp 100 → 2000 ops/s until first p99 > 500 ms or first error; record knee | knee ≥ 500 ops/s; no crash; no orphan layers; squash keeps up under knee |
| `occ/test_occ_stress.py` | concurrency ramp 16 → 256 with 100 % path overlap | rejection rate scales linearly with concurrency; serial-merger queue bounded < 1024; no starvation > 30 s |

Stress probes also exercise resource-boundary inputs: 10k stacked layers,
100k-path changeset, GB-scale upperdir capture. These run as in-test stages
inside the same file, after the ramp completes.

Each subsystem-load file emits one JSONL record per op (see §6.5). Stress
files emit additional `knee` and `degraded_at` fields per stage.

The integrated profiles below stay as the production wall-clock contract:

| Profile | Shells/s | Edits/s | Duration | Overlap | Gitignored | Integrated p99 | Drift | Emerg. depth |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `smoke`     |  2 |  4 | 30 s   | 25 % | 40 % | 500 ms  | 0 | 0 |
| `sustained` |  8 | 16 | 60 s   | 50 % | 40 % | 1000 ms | 0 | 0 |
| `burst`     | 30 | 60 | 20 s   | 50 % | 40 % | 2500 ms | 0 | 0 |
| `soak`      |  4 |  8 | 15 min | 35 % | 40 % | 1200 ms | 0 | 0 |

**Extreme soak (P2, nightly).** `extreme_soak` profile runs 4 hours at the
sustained rate (8 shells/s + 16 edits/s) to surface slow-leak regressions
that 15-min soak misses. Pass bar: drift = 0; RSS regression < 5 % between
hour 1 and hour 4; zero orphan refs at end. Implemented in
`layer_stack_overlay_occ/test_extreme_soak.py`.

### 6.4 Pass Bars (Integrated)

- Correctness: zero drift; accepted writes visible; rejected writes absent.
- Latency: per-call p99 ≤ profile budget.
- Depth: stack depth stays below emergency depth.
- Squash: coalesce ratio ≤ 20 layers/s under sustained or burst.
- Lease budget: zero forced kills unless the profile intentionally overrides
  a lease cap.
- Telemetry: `manifest_lag` and `shell_age_seconds` are present on every
  committed result.

### 6.5 JSONL Output

Every load run emits one JSONL record per call to:

```text
.omc/results/live-e2e-<suite>-<profile>-<utc>.jsonl
```

Subsystem load uses `<suite>` ∈ {`overlay`, `layer_stack`, `occ`};
integrated uses `<suite> = integrated`.

---

## 7. Mapping Back To Migration Experiments

Native and integrated coverage are both required per §1 goal 7, with one
explicit carve-out: **E1, E2, E3 are kernel-syscall invariants.** They prove
that `mount(2)` behaves correctly at deep stacks before any module-level
code is exercised. The native syscall probe is the only verification path —
no integrated equivalent is required, and contributors should not add one.
Integrated load (§6.3) will cross these depths in passing but does not assert
the syscall budget; that is by design.

| Plan exp. | Native suite | Integrated suite | Status |
|---|---|---|---|
| E1 | `overlay/syscall/test_mount_depth.py` | n/a — kernel invariant (§7 carve-out) | Native active |
| E2 | `overlay/syscall/test_snapshot_latency.py` | n/a — kernel invariant (§7 carve-out) | Native active |
| E3 | `overlay/syscall/test_read_latency.py` | n/a — kernel invariant (§7 carve-out) | Native active |
| E4 | `occ/test_orchestrator.py` + `occ/test_serial_merger.py` + `occ/test_occ_load.py` | `layer_stack_overlay_occ/test_concurrent_agents.py` | Both pending |
| E5 | `layer_stack/test_layer_stack_load.py` (depth/squash) | covered via integrated load | Both pending |
| E6 | `layer_stack/test_publisher.py` (fsck/GC) | `layer_stack_overlay_occ/test_failure_recovery.py` | Both pending |
| E7 | `layer_stack/test_layer_stack_load.py` (soak) | `layer_stack_overlay_occ/test_load_profiles.py` (soak) | Both pending |
| E8 | `occ/test_occ_load.py` + `layer_stack/test_layer_stack_load.py` | `layer_stack_overlay_occ/test_load_profiles.py` (all) | Both pending |
| E9 | `layer_stack/test_publisher.py` + `layer_stack/test_squash.py` (kill recovery) | `layer_stack_overlay_occ/test_failure_recovery.py` | Both pending |
| E10 | `occ/test_gated_route.py` (tracked-race) | `layer_stack_overlay_occ/test_codegen_race.py` (tracked) | Both pending |
| E11 | `layer_stack/test_manifest_lifecycle.py` + `occ/test_commit_transaction.py` (telemetry fields) | (`shell_age_seconds` on integrated result) | Both pending |
| E12 | `layer_stack/test_lease_registry.py` + `layer_stack/test_lease_budget.py` | `layer_stack_overlay_occ/test_failure_recovery.py` | Both pending |
| E13 | `occ/test_content_gitignore_oracle.py` + `occ/test_routing.py` | `layer_stack_overlay_occ/test_codegen_race.py` (gitignored) | Both pending |

A green native cell does not unblock its E without the matching integrated
cell — and vice versa. Both are required because:

- Native catches subsystem regressions early and isolates the failing module.
- Integrated catches wiring breakage that lets a green native suite ship a
  product where `read_file` returns stale bytes.

---

## 8. Resolved Defaults

| Question | Decision |
|---|---|
| Sandbox lifecycle | Session-scoped Daytona sandbox with function-scoped `/testbed` reset. |
| Image | Exact `registry:6000/daytona/sweevo-psf-requests-3738:v1`; fail fast otherwise. |
| Native probe import root | `cd /tmp/eos-sandbox-runtime && python3 -c "..."` — bundle is extracted flat at `BUNDLE_REMOTE_DIR`. |
| Layer-stack probe root | Per-probe `/tmp/eos-sandbox-runtime/layer-stack-test-<pid>/` (NOT the runtime's `DEFAULT_LAYER_STACK_ROOT`). |
| Import fence | Enforced by collection hook in `conftest.py`; pytest-process imports of `sandbox.{layer_stack,overlay,occ}` are a collection error. In-sandbox `python3 -c` imports are the supported path. |
| Resource samplers | `/proc/self/fd`, `/proc/self/status` (VmRSS, VmHWM, Threads), `/proc/self/mounts`, `df -i`, `os.times`. |
| Subsystem load JSONL | `.omc/results/live-e2e-<suite>-<profile>-<utc>.jsonl`. |
| Integrated load JSONL | `.omc/results/live-e2e-integrated-<profile>-<utc>.jsonl`. |
| Drift definition | Realtime checks plus post-run replay reconciliation. |
| Burst emergency depth | 0 emergency-depth touches. |
| Gitignored routing | OCC `GitignoreOracle`, never caller-supplied ignored path lists. |

---

## 9. Out Of Scope

- Host-local live tests. Any `import sandbox.layer_stack` (or `.overlay`,
  `.occ`) at module scope inside the live tree fails collection.
- New public API knobs that exist only to fake gitignore classification.
- CI wiring.
- Cross-session or cross-host load.
- Replacement of unit tests under `backend/tests/unit_test/test_sandbox/`.
