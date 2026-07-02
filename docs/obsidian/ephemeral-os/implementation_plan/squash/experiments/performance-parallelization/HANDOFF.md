# Handoff — Squash sweep benchmark harness + W tuning

Follow-up to the shipped remount-sweep parallelization. Implements the
**highest-leverage subset** of `E2E_BENCHMARK_SPEC.md` so the correctness and
scaling checks can run, and tunes the default sweep width.

Parent spec: `experiments/performance-parallelization/E2E_BENCHMARK_SPEC.md`.
Evidence + design: `perf-20260703-052525/{RESULTS,DESIGN}.md`.

## 0. TL;DR — three deliverables, in priority order

1. **`M`/`B` scenario knobs** — deterministic-enough control of *migrated* session
   count `M` and squashable *block* count `B` (§3).
2. **A/B driver** — run the same workload at `EOS_REMOUNT_SWEEP_WIDTH=1` (serial
   control) and `=cores` (parallel), then compare. This lights up **CORR-AB-EQUIV**
   (the linchpin correctness check) and **PERF-WIDTH** today (§4).
3. **Tune `W`** — sweep width, pick + justify the default, decide env-knob vs
   config (§5).

Do 1→2→3 in order; 2 depends on 1, and CORR-AB-EQUIV is the biggest correctness
win for the least code.

## 1. What already exists (reuse, don't rebuild)

- **Parallel sweep** (commit `fab1c01b9`): `remount_sweep` + `sweep_width` +
  `EOS_REMOUNT_SWEEP_WIDTH` in
  `crates/sandbox-runtime/operation/src/layerstack/service/impls/squash.rs`;
  phase-split `remount_snapshot`/`execute_remount`/`remount_apply` in
  `crates/sandbox-runtime/workspace/src/lifecycle/remount.rs`. `W=1` is a faithful
  serial control (same binary) — this is what makes the A/B driver cheap.
- **Span harvest**: `SQUASH_HARVEST_OBS=1` → `conftest.py` teardown →
  `helpers.harvest_observability` `docker cp`s `observability.ndjson(.1)` into the
  case report dir. Best-effort, opt-in, no effect on normal runs.
- **Analyzer**: `perf-20260703-052525/scripts/analyze_spans.py` — groups spans by
  trace, emits `sweep_wall_ms`, `sweep_serial_sum_ms`, `overlap_factor`,
  per-disposition distribution, per invocation. Extend it (don't fork it).
- **Driver**: `perf-20260703-052525/scripts/run_combo.sh <label> <sessions>`.
- **Baselines**: `perf-20260703-052525/logs/attribution-{baseline,proto}-s{50,200}.*`.

## 2. Ground truth — read the code before building the knobs

The migrate/identity and block behavior is emergent from the lease/layer topology.
Do not guess it; these are the authorities:

- **Migrate vs Identity** — `acquire_rewritten_lease`
  (`layerstack/src/stack/lease/rewrite.rs`): a session resolves **Identity** iff
  `contract_layers(session.manifest, substitutions)` leaves its manifest
  unchanged (no replaced layer is in its manifest); otherwise **Migrated
  (Replaced)**. So "a session migrates" ⇔ "the squash replaced ≥1 layer that is in
  that session's pinned manifest".
- **Blocks & boundaries** — `partition_blocks`
  (`layerstack/src/stack/squash.rs`): squashable blocks are maximal contiguous
  runs of **≥2** non-base (`B*`) layers that contain **no boundary**; boundaries =
  `lease_newest_layers()` — the newest layer of **every live lease**. Adding a
  session adds a boundary at its newest layer. This is why `M`/`B` are topology-
  driven, not free parameters.
- **Squashed layer ids carry a nonce** — `allocate_layer_dirs`
  (`layerstack/src/storage/fs.rs`): `format!("{prefix}{version:06}-{:08x}",
  next_unique())`. Consequence: the final active manifest and
  `manifest_root_hash` (which hashes layer ids, `model/mod.rs:163`) are **not
  stable across runs**. The A/B comparator must compare *logical* outcome, not
  bytes — see §4.3. **(This corrects the spec's "byte-identical manifest"
  wording; update it.)**
- **Dispositions are observable** — each swept session emits a
  `workspace_session.remount` span with `attrs.disposition` ∈
  {Identity, Migrated, Leased{…}, Faulty{…}}. The harness can therefore **measure
  the achieved `M`** rather than trusting it blindly.

Scenario extension point: `_scenario_load_combo_http`
(`cli-operation-e2e-live-test/manager/management/squash/helpers.py:2119`). Its
per-round shape is: publish small/edit/large layers → create a *tranche* of
sessions → start background commands → `_squash_with_http_fleet`. Sessions created
in a round pin the manifest *after* that round's publishes; a later squash that
flattens those layers migrates the sessions that pinned them.

## 3. Deliverable 1 — `M`/`B` knobs

### 3.1 `M` — `SQUASH_MIGRATE_RATIO` (fraction of `N` that must migrate)

Exact `M` is topology-dependent (boundaries derive from live leases), so **aim then
measure**:

- **Construction (deterministic ordering):** to make a session *migrate*, create it
  while a squashable block sits below its newest layer (publish the block, then
  create the session). To make a session *Identity*, create it on a stack whose
  non-base layers are already all boundaries / already squashed (e.g. create it
  immediately after a squash, before any new multi-layer run is published). A clean
  builder:
  1. publish a ≥2-layer run `R`;
  2. create `round(M_ratio·N)` sessions → they pin a manifest containing `R`;
  3. squash → those sessions migrate; now the stack top is a single squashed layer;
  4. create the remaining `N − M` sessions → their manifest's non-base content is
     the single squashed layer (a boundary for them), so the next squash cannot
     contract them → Identity.
- **Acceptance:** assert the **measured** migrated count (from harvested
  dispositions) is within tolerance of the target (e.g. `|M_measured − M_target| ≤
  max(2, 0.1·N)`) across `K` runs. For A/B, exact `M` is unnecessary — identical
  workload is what matters.

### 3.2 `B` — `SQUASH_BLOCK_COUNT` (squashable blocks per invocation)

- **Construction:** repeat `B` times: publish a ≥2-layer run, then create one
  "boundary" session (its newest layer plants a boundary that separates this run
  from the next). `B` runs separated by `B` boundaries → `B` blocks.
- **Acceptance:** the daemon already returns `squashed_blocks`; assert
  `len(result["squashed_blocks"]) == B` exactly (existing `_assert_contract`
  surfaces it).

### 3.3 Where

Add the knobs to `_scenario_load_combo_http`, or (cleaner) add a focused
`_scenario_ab` scenario + `AB-*` case ids that build a controlled `(N, M, B)`
topology without the HTTP fleet noise, and keep `_scenario_load_combo_http` for the
HTTP-disruption axis. Additive edits only — `helpers.py` is also edited by the
finalize-policy agent.

## 4. Deliverable 2 — A/B driver (enables CORR-AB-EQUIV + PERF-WIDTH)

### 4.1 Determinism

Both arms must run the **identical** workload: fixed workspace variant, fixed
publish sequence/content, no timestamps/random bytes in file content, fixed knob
values. The only difference between arms is `EOS_REMOUNT_SWEEP_WIDTH`.

### 4.2 Two arms

Run the case twice as separate `sandbox-cli`/pytest invocations, exporting
`EOS_REMOUNT_SWEEP_WIDTH=1` then `=<cores>` (the daemon reads it per squash call —
no rebuild between arms). Harvest each arm's `observability.ndjson*` and a final
`sandbox-cli observability layerstack` snapshot.

### 4.3 Comparator — the CORR-AB-EQUIV assertion (refined)

Squashed layer ids are nonce-named (§2), so **do not** diff manifests byte-for-byte
or by `manifest_root_hash`. Assert **logical equivalence**:

- **disposition multiset identical** — exact per-class counts
  (Identity/Migrated/Leased-by-reason/Faulty) match between arms;
- **surviving pre-squash layer-id set identical** — the set of original (non-`S*`)
  layer ids still present after squash is the same;
- **block count identical** — `len(squashed_blocks)` matches;
- **final manifest layer count identical**;
- **space/teardown identical** — same layer-dir delta, staging empty, no residue.

Emit `ab-diff.json` with the per-field verdict; the case passes iff all hold. This
is stronger than a single-run check: it proves parallelism changes *timing only*.

### 4.4 PERF-WIDTH falls out

The same driver, run across `W ∈ {1,2,4,8}`, feeds §5. Reuse it.

## 5. Deliverable 3 — tune `W`

### 5.1 Experiment

Fix `N=200` (and a second point `N=400`), `M`≈`1.0` for the cleanest signal (all
migrate). For `W ∈ {1,2,4,8,16}`, `K≥3` repeats each:

```bash
for W in 1 2 4 8 16; do
  EOS_REMOUNT_SWEEP_WIDTH=$W \
    scripts/run_combo.sh width-$W 200   # harvests spans
  python3 scripts/analyze_spans.py <report>/observability.ndjson* --json width-$W.json
done
```

Record per `W`: `overlap_factor`, `sweep_wall` p50/p95, per-migrated `dur_ms` p95,
`T_squash`, `T_http_disconnect`.

### 5.2 What to look for

- `overlap_factor` rises toward `min(W, cores, M)` then **plateaus** past
  `W=cores` (the CPU-bound share of quiesce/runner saturates the cores). The work
  is partly wait-bound (subprocess `wait()`, freeze poll, fsync), so a *modest*
  gain past `cores` is possible — find the knee.
- `sweep_wall` minimum and where it stops improving.
- **Oversubscription tax**: per-migrated p95 inflation as `W` grows (measured 7.3→
  ~9 ms at 50/W=4, ~14 ms at 200/W=4). Past the knee this rises with no wall gain.
- `T_http_disconnect` vs `W`: denser concurrent freezes may raise it — must stay
  ≪ 1500 ms.

### 5.3 Decide the default

Current default = `available_parallelism()` (4 in-container). Options: keep it;
`min(available_parallelism, CAP)`; or a small wait-bound multiple
(`min(2·cores, CAP)`) if the knee is past `cores`. Express as a function of
`cores`, not a constant (CI hardware differs). If you promote `W` from the
`EOS_REMOUNT_SWEEP_WIDTH` env knob to real config, plumb it through
`SandboxRuntimeConfig` → the operation layer (do **not** put it in
`ObservabilityConfig`); keep the env override for benchmarking.

### 5.4 Deliverable

A short `W-tuning.md` in this dir: the curve, the knee, the chosen default + one-
line justification, and whether it became config.

## 6. Constraints (unchanged from the project)

- No test code in `src/`; harness lives in `cli-operation-e2e-live-test/`.
- Work on `main`, commit directly; **parallel agents are active** — additive,
  localized edits; never revert others' work (`helpers.py` and `runtime/*` have
  concurrent edits).
- Preserve every correctness invariant (DESIGN §1/§4): commit serialization/
  durability, per-session gates, C1/C5, pin-overlap, lease-GC order.
- **No RAM-for-speed**: any harness aggregation stays `O(W)`/`O(1)` per session,
  not `O(N)` buffering; the memory gate (daemon RSS slope vs `N` ≈ 0) is in the
  spec — implement the RSS sampler for it (spec §8.5).

## 7. Gotchas (paid for already — don't re-discover)

- **Span rotation**: `observability.max_file_bytes` defaults to 8 MiB; ~31 k
  periodic sample records at `N=200` rotate away early invocations (only one
  rotated generation is kept). Raise it via a bench config YAML passed through
  `SANDBOX_GATEWAY_CONFIG_YAML`; `ObservabilityConfig` has `deny_unknown_fields`,
  so place the `observability:` section correctly and validate the daemon still
  boots before a long run.
- **Nonce layer ids** → logical (not byte) A/B comparison (§4.3).
- **Exact `M`** is topology-driven → aim + measure (§3.1).
- **Straggler injection** (PERF-STRAGGLER) needs an uninterruptible/slow-freeze
  task; it is fiddly — lower priority than A/B, defer if time-boxed.
- **4-core container** caps CPU-bound overlap near 4×; judge `W>4` on the
  wait-bound margin only.

## 8. Definition of done

- `M`/`B` knobs land; a run reports measured `M` within tolerance and exact `B`.
- A/B driver runs **CORR-AB-EQUIV** on a controlled case (and LOAD-COMBO `N=200`):
  logical equivalence PASS for `W=1` vs `W=cores`, `ab-diff.json` emitted.
- **PERF-WIDTH** curve produced; default `W` chosen + justified in `W-tuning.md`;
  if promoted to config, plumbing landed with `fmt`/`clippy` clean and a smoke run.
- `E2E_BENCHMARK_SPEC.md` updated: CORR-AB-EQUIV wording changed from "byte-
  identical" to the §4.3 logical-equivalence set; chosen `W` recorded.
