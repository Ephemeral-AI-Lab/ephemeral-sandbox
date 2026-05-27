# Sandbox Performance — Experiment-First Optimization Plan

**Scope:** `backend/src/sandbox/{occ,layer_stack,overlay,ephemeral_workspace,isolated_workspace}`
**Status:** Draft for consensus review (Planner pass 1)
**Source signals:**
- `docs/sandbox_complexity_analysis.md` (2026-05-27 audit)
- 176 perf reports under `.sweevo_runs/scenario_logs/*/performance_report.json` (21,681 tool-call samples)
- Direct code read of OCC commit queue / transaction, layer-stack publisher / view / squash, overlay capture, ephemeral pipeline

---

## 0. Verification of doc claims against code

| Doc claim | Code anchor | Verified? | Notes |
|---|---|---|---|
| `MergedView.read_bytes` is O(L) per layer (cached index) | `layer_stack/view.py:67-118`, `layer_stack/layer_index.py:30-60` | ✓ | Newest-first iteration over `manifest.layers`; per-layer `LayerIndex` cached by `layer_id`. |
| `MergedView.project` does full-byte copies, share_inodes flag exists but unused on hot paths | `layer_stack/view.py:196-275`, `squash.py:107` | ✓ (partial) | `build_checkpoint` calls `_view.project(..., share_inodes=False)` — opportunity present. `commit_to_workspace` also uses `share_inodes=False` (`stack.py:327`) but that path **refuses to run with active leases** (`stack.py:319`) — explicit teardown, not steady-state hot. |
| `_FileSystemLayerChangeStager.write` does 1 file per change + per-file fsync | `occ/commit_transaction.py:208-258`, `layer_stack/publisher.py:96-101,161-168` | ✓ | Each call writes `NNNNNN.bin` and `_fsync_tree_files` walks all files inside publisher critical section. |
| `_commit_batch` O(N) fix (path→FileResult dict) is in place | `occ/commit_queue.py:189-200` | ✓ | Already shipped. |
| `_disjoint_batches` O(B²) bounded by `max_batch_size=64` | `occ/commit_queue.py:224-252` | ✓ | Greedy bin-pack as documented; not a hot path. |
| OCC revalidation reads paths serially under publisher lock | `occ/commit_transaction.py:74-83`, `occ/path_staging.py:185-189` | ✓ | `_validate_group` per path reads via `LayerSnapshotReader.read_bytes` synchronously inside `begin_transaction()`. |
| Squash is reactive depth-triggered, blocks shell | `ephemeral_workspace/pipeline.py:137,243-274` | ✓ | `_run_shell_pre_mount_maintenance` calls `self._layer_stack.squash(...)` synchronously before mount when `manifest.depth > EOS_SHELL_MOUNT_SQUASH_MAX_DEPTH` (default 64). |
| `walk_upperdir` already optimized off `sorted(rglob)` to `os.walk` | `overlay/capture.py:49-89` | ✓ | Streamed `os.walk(topdown=True)`; `command_exec.capture_upperdir_s` p99 ≈ 0.75ms confirms. |

**Stale-or-out-of-scope claims:** the "10k-write commit → 20k syscalls under publisher lock" justification in doc §Structural #2 is hypothetical — **largest observed batch is `occ.commit.stager_write_count`=1.0 mean (i.e. ≈1 change per commit in practice)** and `layer_stack.publish.write_changes_s` p99=7.5ms / max=52ms. The doc claim about bundled-tar / io_uring 10–100× wins is unsupported by current workloads.

---

## 1. Aggregated hotspot evidence (176 reports, 21,681 tool-call samples)

### Per-call ceiling (max ≥ 100ms) — what the user's threshold actually qualifies

| Metric | n | p50 ms | p95 ms | p99 ms | max ms | Σ (s) | Worst scenarios |
|---|---:|---:|---:|---:|---:|---:|---|
| `occ.apply.total_s` (edit_file) | 5935 | 9.9 | 27.8 | 84.2 | **437** | 76.7 | heavy_io_zoned_concurrent, full_case_user_input, ephemeral_workspace_concurrent_writes |
| `occ.apply.commit_queue_wait_s` / `occ.serial.queue_wait_s` | 5935 | 3.0 | 5.0 | 47.5 | **332** *(804 in heavy_io)* | 28.3 | heavy_io_zoned_concurrent (804ms), full_case_user_input (332ms), high_concurrency_layerstack_overlay_occ (169ms) |
| `layer_stack.shell_pre_mount_squash.total_s` | 67 | 16.8 | 169 | 204 | **204** | 3.08 | full_case_user_input (max 204ms, sum 502ms), capacity.full_system_capacity_matrix, full_stack_adversarial |
| `workspace.mount_s` | 5470 | 22.8 | 76 | **102** | **208** | 162 | background_shell_exhaustion (208ms), background_many_small_writes (192ms), full_case_user_input (137ms ×219 calls = 13s) |
| `layer_stack.auto_squash.total_s` | 33 | 25.6 | 30.8 | 31.3 | **142** | 0.85 | full_case_user_input (142ms × 2 calls × 4 runs) |
| `api.edit.total_s` | 5935 | 10.3 | 28.4 | 84.8 | **445** | 79.3 | (mirrors `occ.apply.total_s`) |
| `api.shell.total_s` | 2570 | 735 | 1365 | 5675 | **35,873** | 2871 | heavy_io_zoned_concurrent — dominated by `workspace.tool_s` (user command), not sandbox boundary |

### Per-call ceiling — FAILS the 100ms gate (per user rule)

| Metric | p99 ms | max ms | Σ (s) | Verdict |
|---|---:|---:|---:|---|
| `layer_stack.publish.write_changes_s` (fsync per file) | 7.5 | 52 | 14.1 | ✗ Skip per user rule |
| `layer_stack.publish.write_manifest_s` | 6.4 | 29 | 17.5 | ✗ Skip |
| `layer_stack.publish.replace_staging_s` | 6.5 | 39 | 14.3 | ✗ Skip |
| `occ.commit.validate_groups_s` | 2.4 | 30 | 3.4 | ✗ Skip |
| `command_exec.capture_upperdir_s` | 0.75 | 83 | 1.4 | ✗ Skip (already optimized) |

### Mocked-not-sandbox latency — out of scope

`api.grep.total_s p50=234ms, sum=484s` and `api.glob.total_s p50=237ms, sum=200s` come from the mock-client framework (`mock.client.execute_tool_once_usec` ≈ 240ms with no sandbox-side phase timings recorded; sandbox-internal keys like `workspace.mount_s` / `command_exec.capture_upperdir_s` are entirely absent for these tool calls). The 240ms is sweevo's synthetic dispatch delay, not real sandbox work. **Not a sandbox optimization target — re-opening this would chase mock-framework overhead.**

### Per-scenario aggregate (advisor #1 — alternative reading of the rule)

If the user's "≥100ms" rule is per-scenario rather than per-call, three additional aggregate targets cross the bar:
- `workspace.mount_s` Σ=162s globally; 13s on full_case_user_input alone → mount-time savings of even 10ms × 5470 calls = 55s.
- `occ.apply.commit_queue_wait_s` Σ=28s globally; **single 804ms tail on heavy_io_zoned_concurrent dominates**.
- `layer_stack.publish.total_s` Σ=56s across 10k+ commits; per-call <100ms but aggregate noteworthy.

**This plan adopts per-call ceiling.** It is the stricter, more user-meaningful reading and avoids chasing micro-wins. We surface the per-scenario view in the ADR so reviewers can override.

---

## 2. Decision Drivers (top 3)

1. **Per-call ≥100ms ceiling, plus a load-divergence co-gate.** Per user instruction, no experiment proceeds unless it has at least one observed >100ms tail to attack. **Additional co-gate for any experiment that *defers* work (E1, E2/B):** under sustained-load the deferred cost must not exhibit monotonic upward drift — converting a bounded p99 spike into unbounded load-correlated divergence violates the spirit of the per-call ceiling even if any single call stays under 100ms.
2. **Docker provider capability gate.** Optimizations relying on `io_uring`, `syncfs`, `linkat` cross-fs, `O_TMPFILE`, `RWF_DSYNC`, or process-priority adjustment (`nice(2)`, `pthread_setschedprio`) must pass a capability probe inside the actual provider env (`backend/src/sandbox/provider/bootstrap.py` Docker path) **before** any integration work. If seccomp / capabilities deny the syscall, the option dies at probe stage.
3. **Evidence-of-delta before integration.** No code change touches `sandbox/{occ,layer_stack,overlay}` unless a microbench in isolation reproduces a ≥100ms delta on the targeted hotspot. Microbench failure = experiment killed, not "let's try anyway."

---

## 3. Principles

- **Behavior preservation first.** Each candidate optimization must keep:
  - OCC correctness (CAS retry semantics, atomic batch invariants, gated/direct staging hash chains).
  - Layer-stack immutability (published layers never mutated; only manifest CAS swaps).
  - Lease-based GC (snapshot lease-head set decides which layers may be squashed).
- **One experiment per opportunity, never bundled.** Each lives in `backend/scripts/perf_experiments/<name>/` and produces a self-contained `report.md` with baseline-vs-treatment numbers from the same harness on the same machine.
- **Microbench in isolation before integration PR.** Each experiment ships a `bench.py` that runs both legacy and treatment code paths against synthetic input — no daemon, no Docker, no provider. Integration only follows passing microbench + capability gate + correctness regression run.
- **Quantitative go / no-go.** A clear `bench.threshold` line decides promotion. No "vibes" advancement.
- **Falsifiability over advocacy.** If two experiments target the same hotspot, only one survives the gate; we do not stack.

---

## 4. Viable Options

### Option A — Ship Experiments E1+E2 only (highest evidence)

Run only the two experiments with the strongest per-call data:
- **E1: Async / pre-emptive squasher daemon** — pulls squash off shell critical path.
- **E2: OCC publisher pipelining under contention** — addresses the `heavy_io_zoned_concurrent` 804ms queue-wait tail.

**Pros:**
- Each targets a verified ≥100ms hotspot.
- Smallest blast radius for hot-path code.
- Both have natural microbench shapes (single-process replay against `LayerStack` and `CommitQueue`).

**Cons:**
- Leaves the MergedView aggregate-index opportunity (doc §1) unattempted. That win could be larger but is a much bigger PR.
- Doesn't address `workspace.mount_s` p99=102ms (208ms max) outside of its squash-correlated component.

### Option B — A + E3 (MergedView aggregate index spike)

Adds:
- **E3: MergedView path→layer manifest index spike** — *experiment only*, time-boxed to 2 days. Build a prototype that answers: does a manifest-wide `path → (layer_id, kind)` index materially cut shell pre-mount cost when squash is disabled? If yes, separate plan follows.

**Pros:**
- Tests the doc's claimed "biggest structural win" with a bounded spike, not full implementation.
- If spike confirms <100ms delta, we kill the structural-redesign cost outright.

**Cons:**
- The data does not yet justify a full index implementation; spike is the gate.
- Two experiments share squash as their target — risk of finding both work and needing a tiebreaker.

### Option C — Skip experiments, optimize directly per doc

Implement doc Structural #1, #2, #4 in PRs, defer measurement to post-merge.

**Pros (steelman):**
- Faster wall-clock to claimed wins if doc is right.

**Cons:**
- Violates user instruction explicitly ("experiment before implementation").
- Doc §Structural #2 (bulk-write staging) and §#4 (parallel revalidation) **fail the 100ms per-call gate by 10–40×** on observed data. They are 5–10ms hotspots in the actual workloads, not 100ms+.
- Doc §Structural #3 (`share_inodes` for projection) targets `auto_squash` (max 142ms, sum 264ms) but the cost of `share_inodes` *correctness* (read-only invariant, EXDEV fallback) is non-trivial; the upside is bounded by the sum.
- **Invalidated.**

**Recommended: Option B.**
- A is too conservative if E3 turns up evidence; we'd repeat the spike later.
- C contradicts the user's rule and the observed data; it is the only outright-bad branch.

---

## 5. Pre-mortem (deliberate mode — 3 scenarios)

**Failure 1 — Experiment passes microbench, integration regresses tail.** E1 (async squasher) microbench shows clean wins on a single-process replay, but integration on full_case_user_input regresses `workspace.mount_s` p99 because foreground shell now races background squash that hasn't published its checkpoint yet — mount picks the deeper manifest. **Mitigation:** integration gate must include the full `full_case_user_input` scenario; the regression test must compare `workspace.mount_s` p99 before/after and block on ≥10% degradation.

**Failure 2 — Docker capability probe gives false-positive on dev workstation, then dies in CI.** A syscall like `io_uring_setup` returns success on dev macOS (Docker Desktop's Linux VM) but is blocked by seccomp profile in the production Docker provider used by `EOS_SANDBOX_PROVIDER`. We over-invest in E-anything-that-needs-uring. **Mitigation:** capability probe is run inside the exact provider container image (`backend/src/sandbox/provider/bootstrap.py` Docker mode) for each target deployment env. Probe output committed as `capability_matrix.json`; experiments referencing a capability that isn't in the matrix are auto-killed in CI.

**Failure 3 — Hidden behavioral change in async squasher breaks OCC commit invariants.** E1 squashing concurrently with an open `LayerSnapshotReader` could cause stale layer-dir reads or `LayerStackStorageError("layer no longer present")` (`view.py:307-311`) raised mid-OCC-revalidate. **Mitigation:** lease semantics already prevent this — `_segment_around_lease_heads` explicitly excludes lease-held layers from any plan (`squash.py:142-164`). Test plan **must** include a regression run of `tests/test_occ/` + `tests/test_layer_stack/lease/` after E1 integration. Any new test failure blocks the merge regardless of bench delta.

---

## 6. Experiments (Option B)

> All experiments live under `backend/scripts/perf_experiments/<exp_id>/` with:
> - `README.md` — problem statement, threshold, capability gate, design constraints, dependency on other experiments.
> - `bench.py` — runs baseline vs treatment; invokable via `uv run python backend/scripts/perf_experiments/<exp>/bench.py --output report.md`.
> - `harness.py` — **shared contract** (mandatory):
>   1. Emit `(median, p95, p99, max, n, ci95)` per condition; ci95 computed via bootstrap (≥1000 resamples).
>   2. ≥30 iterations per condition + 5 warmup iterations (warmup excluded from stats).
>   3. Capture machine identity into `report.md` YAML front-matter: `kernel`, `python_version`, `docker_version` (if applicable), `provider_image_tag` (if E0 used), `cpu_model`, `wall_clock_at_start`.
>   4. Defeat JIT / cache effects: between conditions, clear `LayerStack` cached indexes (`MergedView._layer_index_cache`) and force OS page-cache drop on the staging dir (`echo 3 > /proc/sys/vm/drop_caches` only if running as root; otherwise note "uncached" in report).
> - `report.md` — filled after run; baseline vs treatment table + signed % deltas + 95% CI; ends with one-line VERDICT (`PROMOTED` / `KILLED` / `INCONCLUSIVE`).

### E0 — Docker capability matrix (gate for E1/E2/E3)

**Goal:** Enumerate which kernel syscalls + caps the actual provider container allows.

**Probes (each: success/EPERM/ENOSYS, with errno + caps dump):**
- `io_uring_setup(8, ...)`
- `syncfs(fd)` against an upperdir mount
- `linkat(AT_FDCWD, src, AT_FDCWD, dst, 0)` across the storage_root → workspace_root mounts
- `open(O_TMPFILE | O_RDWR)`
- `pwritev2(..., RWF_DSYNC)`
- `unshare(CLONE_NEWNS | CLONE_NEWUSER)` (we already use it)
- `mount(MS_MOVE)` on a tmpfs
- `nice(+10)` and `pthread_setschedprio(SCHED_IDLE)` — **required by E1** to actually deprioritize the async squasher under Docker seccomp; if denied, E1's "low-priority background task" is a misnomer and the squasher will compete with foreground for CPU.

**Harness:** small Python script invoked via `docker run --rm <provider-image> python3 /probe.py`. Output as JSON dict {syscall: {ok|errno|caps_required}}.

**Pass criterion:** matrix written to `backend/scripts/perf_experiments/E0_capabilities/capability_matrix.<target_env>.json` where `<target_env> ∈ {dev, ci, prod}`. **Each deployment env needs its own matrix** — developer's Docker Desktop ≠ CI runner ≠ prod Docker provider; pre-mortem F2 names this risk explicitly. Output JSON includes a `target_env` field; downstream experiments auto-kill if they cannot find a matrix tagged for their target. No threshold — this is a gate.

**Effort:** ½ day.

### E1 — Asynchronous / pre-emptive squash daemon

**Hotspot attacked:** `layer_stack.shell_pre_mount_squash.total_s` p99=204ms / max=204ms / Σ=502ms in worst scenario.

**Hypothesis:** Moving squash off shell's critical path saves ≥100ms in the p99 tail when manifest depth >64.

**Experiment:**
- `bench.py` builds a `LayerStack` with 80 layers, simulates a steady-state shell-mount loop while a background squasher runs.
- Baseline = current `_run_shell_pre_mount_maintenance` path.
- Treatment = squash runs in a low-priority background task triggered by depth observer; foreground shell mounts the manifest as-is.

**Threshold (go/no-go) — all three required:**
1. `workspace.mount_s` p99 of foreground shell mounts ≤ baseline's p99 minus 100ms.
2. No regression in `workspace.mount_s` p50 (the un-squashed mount must not be slower because of deeper layer count).
3. **Load-divergence co-gate (deliberate-mode addition):** under sustained write load (10 writes/sec for 60s, simulating full_case_user_input), foreground `workspace.mount_s` p99 must remain ≤ baseline + 50ms across the full duration **AND** `async_squasher.lag_s` (new TimingKey) must stay bounded — no monotonic upward drift across the 60s window. This catches the failure mode where async squash converts a bounded 204ms tail into unbounded load-correlated divergence.

**Capability gate:** `nice(+10)` and `pthread_setschedprio(SCHED_IDLE)` must be `ok` in `capability_matrix.json`. If denied, async squasher will run at foreground priority and the lag co-gate will likely fail — kill E1, fall back to E3 if it passed.

**Risks resolved by gate:**
- If treatment p50 regresses (deeper mount slower than expected), kill — doc §1's "the reason squash exists is depth is first-class cost" was right.
- If background squasher can't keep up at observed write rate (lag drifts upward), kill — implies we need the merged-index option (E3) instead.

**Ordering:** E1 integration starts **only after E3 spike report is filed**. If E3 spike passes both thresholds, E1 is dropped (subsumed). This avoids shipping E1 then reverting it.

**Effort:** 1 day microbench, 2 days integration if green (and E3 spike does not subsume).

### E2 — OCC publisher pipelining / batched-window tuning

**Hotspot attacked:** `occ.apply.commit_queue_wait_s` max=804ms on heavy_io_zoned_concurrent, max=332ms on full_case_user_input.

**Hypothesis:** Either (a) `batch_window_s=0.002` is starving the batcher under steady write pressure, or (b) the single-thread publisher is the wall — pipelining `_combine_prepared` / `_disjoint_batches` with the prior batch's publish would smooth the tail.

**Sub-experiments — treated as competing options, not bundled (per §3 falsifiability):**

**E2/A — Batch-window sweep.** Single-line config change to `CommitQueue.batch_window_s`. No correctness risk. Lower blast radius → lower threshold acceptable.

**E2/B — Double-buffered pipeline.** Reorders OCC commit / revalidate phases. Real correctness risk per `commit_transaction.py:65-83`. Must satisfy the design-constraint clause below.

**Experiment:**
- `bench.py` drives `CommitQueue` directly with N concurrent submitters (N ∈ {2, 4, 8, 16}) writing disjoint paths.
- Baseline = current `_run` loop (`commit_queue.py:132-167`).
- Treatment A = batch-window tuning sweep (10µs–10ms).
- Treatment B = double-buffered pipeline: prepare batch K+1 while publish K runs.

**E2/A and E2/B compete.** If both pass their respective thresholds (below), the better p99 reduction wins and the other is dropped — never both shipped.

**Thresholds (separate per treatment, justified by blast radius):**

| Treatment | Reduction threshold | Co-gate | Justification |
|---|---|---|---|
| E2/A — batch-window sweep | `commit_queue_wait_s` p99 reduction **≥50ms** on N=8 disjoint workload | none beyond correctness regression | Single-line config change, zero correctness risk → a 50ms win is worth shipping. Below 50ms = kill. |
| E2/B — double-buffered pipeline | `commit_queue_wait_s` p99 reduction **≥100ms** on N=8 disjoint workload | CAS retry pressure co-gate (below) **AND** design-constraint clause (below) | Reorders OCC commit phases under publisher lock → real correctness risk. Higher reward bar required to justify the risk. |

**CAS retry pressure co-gate (Treatment B only):** measure `occ.serial.cas_attempts` distribution under N=8 mixed disjoint/overlapping workload. Treatment B must keep p99 of CAS attempts ≤ baseline + 1 (currently capped at `MAX_OCC_CAS_RETRIES=3`). Rationale: pipelined K+1 preparation against the pre-K manifest snapshot, then revalidating against post-K manifest at `commit_transaction.py:65-83`, will systematically increase the chance of `ManifestConflictError` at `publisher.py:62-66`. If retry pressure rises, the apparent queue-wait win is paid for by retry latency.

**Design constraint for Treatment B:** the experiment design must explicitly either (a) re-validate K+1's `_disjoint_batches` against K's *published* manifest before K+1's commit (collapsing back to serial in conflict cases), or (b) document and bench a path-collision predicate between adjacent pipelined batches. Without one of these, Treatment B is unsafe even if microbench numbers look good. The experiment README must cite `commit_queue.py:224-252` (per-call-only path-set conflict detection) and `publisher.py:122-128` (CAS-retry safety net) and state which approach it takes.

**Capability gate:** none (pure Python).

**Risks:** Treatment B reorders `monotonic_now()` boundaries; assertions in `_combine_prepared` (`commit_queue.py:255-264`) about `atomic` invariant must hold. Microbench must include atomic + overlay-capture interleaving cases.

**Effort:** 1 day microbench, 2 days integration if green.

### E3 — MergedView aggregate-index spike

**Hotspot attacked:** same as E1 (shell pre-mount squash) but via a *structural* path.

**Hypothesis:** A manifest-wide `path → (layer_id, kind)` index, invalidated on publish, lets shell mount skip squash entirely without slowing reads.

**Experiment (spike, 2 days max):**
- Build a throw-away `IndexedMergedView` that exposes the same surface as `MergedView` (read_bytes, list_dir, iter_paths) but maintains an aggregate dict keyed by path.
- `bench.py` compares `read_bytes` at L=10, 50, 100, 200 against existing `MergedView` for 10k random path lookups.
- Measure rebuild cost on `publish_layer` (`layer_stack/publisher.py:96-118`).

**Threshold:** at L≥100, **demonstrate sublinear scaling of `read_bytes` latency vs L** (median latency growth from L=10 to L=200 must be ≤ 2× — current implementation is O(L) so grows ~20×). Concretely, ≥ 5× throughput at L=100 vs current MergedView is the operational proxy, derived as: current `MergedView.read_bytes` walks ~L cached-index lookups serially; a flat-index lookup is O(1), so at L=100 the asymptotic ceiling is ~100× faster — we ask for 5× to leave a margin for hash-table constants and cache effects. **AND** **incremental update cost** added to publish-layer p99 ≤ 20ms (apply only the new layer's changes to the existing index — *not* full rebuild; full rebuild trivially blows the 20ms budget at L=200). Both required.

**Capability gate:** none.

**Ordering:** E3 spike runs **before** E1 integration (per §3 falsifiability principle — both target the same hotspot; we decide which lives before we ship either).

**Decision tree:**
- Both pass → write a separate full implementation plan, **drop E1** (subsumed; no E1 integration ever ships).
- Only read-throughput passes (invalidation cost too high) → keep E1, kill E3.
- Both fail → kill E3, keep E1.

**Effort:** 2 days spike, no integration in this plan.

### Explicitly NOT pursued (with reason)

- **Bulk-write fsync staging (doc §2):** publish.write_changes_s p99=7.5ms / max=52ms / Σ=14s globally. Fails 100ms per-call gate by 13×; aggregate not large enough to override under user's stricter rule.
- **Parallel OCC revalidation (doc §4):** `occ.commit.validate_groups_s` p99=2.4ms / max=30ms / Σ=3.4s globally. Fails 100ms per-call gate by 40×.
- **`share_inodes=True` on `commit_to_workspace` / `build_checkpoint` (doc §3):** `auto_squash.total_s` max=142ms, but only 33 occurrences globally (Σ=0.85s). `commit_to_workspace` itself refuses to run with active leases (`stack.py:319`) — explicit teardown, not steady-state. Re-evaluate only if E1 confirms checkpoint build is the squash-time bottleneck.

---

## 7. Expanded test plan (deliberate mode)

| Layer | Coverage | Pass gate |
|---|---|---|
| **Unit (microbench)** | E1: `tests/perf/test_async_squasher_bench.py` reproduces ≥100ms p99 delta on synthetic 80-layer stack with 100 mount iterations. E2: `tests/perf/test_commit_queue_pipelining_bench.py` reproduces ≥100ms p99 delta on N=8 disjoint submitters. E3: `tests/perf/test_indexed_merged_view_bench.py` reports L=10/50/100/200 throughput table. | Each bench's `report.md` must show signed % deltas with 95% CI; CI ≤ 20% of median. |
| **Unit (correctness)** | `tests/test_occ/` (68 tests) + `tests/test_layer_stack/` lease, squash, publish, view subsuites. | Zero regressions. |
| **Integration** | Real `LayerStack` + `CommitQueue` driven through `EphemeralPipeline.run_tool_call`; full `tests/test_sandbox/scenarios/test_ephemeral_workspace_*` suite. | Zero regressions; verify `auto_squash` and `pre_mount_squash` timings appear in audit. |
| **E2E** | `python -m sweevo run ... --scenario full_case_user_input` and `--scenario heavy_io_zoned_concurrent`, comparing `performance_report.json` for the two changed scenarios. | `workspace.mount_s p99` regression ≤10%; `occ.apply.commit_queue_wait_s p99` improvement ≥100ms (E2) or `shell_pre_mount_squash` p99 reduced by ≥100ms (E1). |
| **Observability** | Verify these existing TimingKey fields still record (and add only if needed):  `layer_stack.shell_pre_mount_squash.total_s`, `occ.apply.commit_queue_wait_s`, `occ.commit.publish_layer_s`, `layer_stack.auto_squash.total_s`. For E1 add `layer_stack.async_squasher.{queued,started,completed,lag}_s`. | Each new TimingKey must appear in `backend/src/sandbox/shared/timing_keys.py` and have at least one assertion in the scenario test that values are emitted. |

---

## 8. Verification steps (per experiment)

1. **Baseline capture:** run the targeted hotspot scenarios on HEAD and save `performance_report.json` under `backend/scripts/perf_experiments/<exp>/baseline/`.
2. **Capability probe:** ensure required entries in `capability_matrix.json` are `ok`. If not, mark experiment dead in `report.md` and STOP.
3. **Microbench:** `uv run python backend/scripts/perf_experiments/<exp>/bench.py --output report.md` produces a markdown report with baseline-vs-treatment medians, p95, p99, max, n, CI.
4. **Decision:** if threshold met, proceed to integration PR. If not, write a one-paragraph killed-because in `report.md` and close the experiment.
5. **Integration regression:** rerun the same scenarios and re-record `performance_report.json` under `<exp>/treatment/`. Diff via the same aggregator script used in §1.
6. **Sign-off requirements:** integration PR description must link both `report.md` files and quote the bench delta + the scenario p99 delta.

---

## 8.5. Post-merge rollback procedure (operational risk control)

Pre-merge gates (§7 / §8) catch regressions before integration; this section names the contract for catching regressions *after* a change is in main.

**Required for every integrated experiment (E1, E2/A, E2/B if any reach integration):**

1. **Feature flag default off.** Each integration introduces an env var:
   - E1: `EOS_LAYER_STACK_ASYNC_SQUASHER=0|1` (default `0`).
   - E2/A: `EOS_OCC_BATCH_WINDOW_S=<float>` (default = current `0.002`).
   - E2/B: `EOS_OCC_PIPELINE_PUBLISHER=0|1` (default `0`).
   Production rollout flips the flag in a separate config-only PR after one week of canary observation in `sweevo` runs.

2. **Bypass path.** Setting the flag to its default value must trace the exact same code path as HEAD-before-integration — no residual reordering, no leaked state. Reviewer must verify this explicitly in the integration PR description.

3. **Post-merge monitoring window.** After each flag flip, the `sweevo` daily run compares the targeted scenario's `performance_report.json` against the pre-flip baseline. Window length differs by failure-mode shape:
   - **E2/A, E2/B: 7 days** — failure modes are CAS pressure / latency tail; both emerge inside a single workload run.
   - **E1: 14 days** — load-correlated divergence of the async squasher (the iteration-1 Architect concern) is a multi-day pattern as manifest depth slowly outpaces squash throughput. A 7-day window can miss it.
   Any of the following triggers automatic revert:
   - Targeted hotspot p99 regresses by >10% (E1: `workspace.mount_s`; E2: `occ.apply.commit_queue_wait_s`).
   - Any other scenario p99 regresses by >25%.
   - New `ManifestConflictError` rate (`occ.serial.cas_attempts > 1` ratio) doubles for E2/B.
   - `layer_stack.async_squasher.lag_s` exhibits monotonic upward drift across the 14-day window for E1, or `resource.layer_stack.manifest_depth` rolling-median climbs >20% over the window.

4. **Revert mechanics.**
   - E1 (daemon lifecycle introduced): revert is `git revert <SHA>` of the integration commit **plus** verification that no daemon process is left running (the async squasher must exit cleanly on flag-off). Integration PR must include `test_async_squasher_clean_shutdown` covering this.
   - E2/A: revert is the env-var flip — no code revert needed.
   - E2/B: `git revert <SHA>`. Integration PR must avoid touching `MAX_OCC_CAS_RETRIES` or any persisted manifest schema so revert is purely behavioral.

5. **No schema migrations.** None of the listed experiments may introduce a persistent on-disk schema change (manifest format, layer-storage layout, OCC staging dir layout) that would survive a revert. Reviewer gates on this explicitly.

---

## 9. ADR

**Decision:** Adopt Option B — run E0 (capability probe), then **E3 spike** (MergedView index, 2-day-boxed) **before** E1 integration begins, then E1 (async squasher) if E3 did not subsume, and E2 (OCC publisher pipelining) independently. No integration without microbench-passing.

**Drivers:**
1. User-imposed 100ms per-call ceiling.
2. Docker capability uncertainty (probe required first).
3. Evidence over advocacy — doc §2/§3/§4 fail the data gate.

**Alternatives considered:**
- A: ship E1+E2 only — too cautious if E3 spike confirms the structural win.
- C: implement per doc directly — contradicts user rule and contradicts observed data.

**Why chosen:** Option B is the smallest plan that respects the user's evidentiary rule, covers the two strongest hotspots (squash tail + commit-queue wait tail), and bounds the structural-redesign question with a 2-day spike instead of an upfront commitment.

**Consequences:**
- Plan delivers two integration PRs (E1, E2) + one spike report (E3).
- Doc §2 / §3 / §4 remain documented opportunities but are explicitly deprioritized with quantitative reasons; if workloads change (e.g., 10k-write batches become common), re-open.
- Each experiment writes a durable `report.md` so future agents can audit the kill or keep decision without re-running.

**Follow-ups:**
- If E3 spike passes both thresholds, write a separate `sandbox_merged_index_PLAN.md` and re-prioritize.
- If E2 reveals the heavy_io_zoned_concurrent 804ms tail is genuinely batch-window-bound (Treatment A wins), the integration is a single-line change; defer Treatment B.
- Re-run §1 aggregation after E1+E2 ship to confirm the global Σ deltas matched per-call deltas (anti-Goodhart sanity check).

---

## 10. RALPLAN-DR summary (compact)

- **Principles:** behavior preservation; one experiment per opportunity; microbench-then-integrate; quantitative gates; falsifiability.
- **Decision Drivers:** per-call ≥100ms ceiling + load-divergence co-gate; Docker capability gate (incl. `nice(2)` / `pthread_setschedprio`); evidence-of-delta before integration.
- **Viable Options:** A (E1+E2 only) / B (E0 → E3 spike → E1 if not subsumed → E2 independent, recommended) / C (skip experiments, dead).
- **Pre-mortem:** integration tail regression in E1; Docker capability false-positive; OCC invariant break in async squash.
- **Test plan:** microbench → unit correctness → integration → E2E scenario diff → observability.
- **Output artifacts:** `capability_matrix.json`, `E3/report.md` (gates E1), `E1/report.md` (if not subsumed), `E2/report.md`, one or two integration PRs (if green).
- **Ordering:** E0 first; E3 spike before E1 integration; E2 may run in parallel with E3 (different hotspot).

---

## 11. Experiment results and decision (2026-05-27)

**Top-level decision: DO NOT PROCEED with code implementation.** No experiment delivered a result that meets its plan-defined integration gate. The strongest 100ms+ candidate (E4 — raising the squash cap) needs a Docker validation run that this macOS session cannot perform, and shipping it without that validation violates §3 ("evidence-of-delta before integration").

### What ran (macOS, no Docker)

| ID | Verdict | Plan threshold | Observed |
|---|---|---|---|
| **E3** (IndexedMergedView spike) | **PROMOTED at microbench, does NOT subsume E1** | scaling ≤2× AND index update p99 ≤20ms | scaling **1.02×**, index update p99 **3.04ms**, baseline scaling 18.08× confirming the O(L) realism gate |
| **E2/A** (batch-window sweep) | **KILLED** | `commit_queue_wait_s` p99 reduction ≥50ms | best window (0.0) reduces p99 by **3.73ms** vs the 0.002 default (baseline p99 = 30.97ms, realism gate PASS) |
| **E0**, **E1**, **E2/B** | Not run | — | Blocked on macOS: E0/E1 capability gates (`nice(+10)`/`pthread_setschedprio`) require Linux; E2/B's 100ms reduction threshold is mathematically unreachable on a workstation baseline of 31ms |

### Surprise finding — E4 (added during execution)

Auditing all 176 production `performance_report.json` files and joining each `workspace.mount_s` event with the closest preceding `resource.layer_stack.manifest_depth` observation (5406 mount events; see `backend/scripts/perf_experiments/E4_squash_cap_audit/audit_bucket.py`) gives:

| depth bucket | n     | p50 ms | p95 ms | p99 ms  | max ms  |
|--------------|------:|-------:|-------:|--------:|--------:|
| 1-7          |  713 | 23.36  | 108.20 | **178.24** | 192.14 |
| 8-15         |  703 | 22.51  | 79.08  | 95.24   | 120.22 |
| 16-31        |  905 | 23.13  | 78.80  | 94.74   | 136.84 |
| 32-63        | 1863 | 22.77  | 80.30  | 97.06   | 118.06 |
| **64-99**    | **1210** | **22.64** | **25.55** | **30.43** | **96.05** |
| 100-199      |   12 | 23.08  | 26.26  | 26.26   | 26.26 |

`workspace.mount_s` p99 is roughly **6× lower at depth 64-99 than at depth 1-7**, and p50 is flat across the range. The 64-layer pre-mount squash trigger is empirically over-conservative — there is no kernel-imposed mount-latency penalty for deeper manifests in the observed range. The codebase already calls `fsmount(2)`/`move_mount(2)` directly (`backend/src/sandbox/overlay/kernel_mount.py:49-75`), so the util-linux mount(8) 16-layer cap historically cited for this trigger does **not** apply.

**Why E4 is not "PROMOTED" yet:** selection-bias risk. Depth>64 only appears in the data when the squash trigger gates on it; the dataset is conditioned on squash NOT firing. The finding is consistent with the kernel facts (mount(2) takes 200+ layers) but a controlled validation is required before shipping.

### Why E3 is not being implemented despite its microbench pass

Per plan §6 E3 decision tree: "Both pass → write a separate full implementation plan, drop E1 (subsumed)." The chain assumption underpinning that subsumption (faster reads → skip squash → 204ms hotspot disappears) is **empirically false**. `_run_shell_pre_mount_maintenance` (`pipeline.py:243-274`) docstring is *"Collapse deep manifests before shell enters the kernel mount path"* — the trigger is mount-pressure-driven, not read-driven. IndexedMergedView's 175×/6× read-perf wins are real but do not address the 204ms tail. Since the cleaner alternative (E4 — raise the cap entirely) is one Docker run away, **defer E3 integration** until E4 validation closes; otherwise E3 may ship for a benefit that overlaps with what E4 makes unnecessary.

### Why E2/A is not being shipped despite the small win

The `batch_window_s=0` config produces a real 3.7ms p99 reduction with zero correctness risk. Plan §6 framing was *"single-line config change, zero correctness risk → a 50ms win is worth shipping. Below 50ms = kill."* The 3.7ms is below the gate; the plan's own falsifiability principle says do not ship. Ship-or-skip on whether the marginal win justifies one config PR is left as an explicit deferral, not an auto-promotion.

### Followups (not executed in this session)

1. **E4 validation (recommended next step, ~200ms tail elimination if it confirms):** run one Docker scenario (`heavy_io_zoned_concurrent` or `full_case_user_input`) twice — once with default `EOS_SHELL_MOUNT_SQUASH_MAX_DEPTH=64`, once with `=200`. Ship the cap raise only if `workspace.mount_s` p99 regression ≤10% AND `layer_stack.shell_pre_mount_squash.total_s` p99 drops to near zero.
2. **E1 microbench (only if E4 validation fails):** the original async-squasher path remains the fallback. Requires Linux+Docker for the capability matrix (E0) and the load-divergence co-gate.
3. **E3 integration plan (separate, lower-priority):** if read-perf wins are wanted for non-squash-related reasons (e.g. shell-startup latency, large-manifest projection costs), write `sandbox_merged_index_PLAN.md`. The microbench is durable evidence.
4. **E2/A ship/skip:** explicit one-line decision (yes/no on the config change). The data is the data; the user makes the call.

### Artifacts

- `backend/scripts/perf_experiments/harness.py` — shared microbench harness (Stats, bootstrap CI, machine-identity capture, realism gate, soak-mode primitives).
- `backend/scripts/perf_experiments/E3_indexed_merged_view/{README.md, bench.py, report.md}`.
- `backend/scripts/perf_experiments/E2A_batch_window/{README.md, bench.py, report.md}`.
- `backend/scripts/perf_experiments/E4_squash_cap_audit/{audit_analysis.md, audit_bucket.py}`.
