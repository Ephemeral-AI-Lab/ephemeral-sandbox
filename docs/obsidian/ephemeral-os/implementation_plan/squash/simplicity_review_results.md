---
title: LayerStack Squash + Live Remount — Adversarial Simplicity Review Results
tags:
  - ephemeral-os
  - layerstack
  - review
status: review_results
updated: 2026-07-02
---

# Adversarial Multi-Agent Simplicity Review — Results

Target: `spec.md` (revision of 2026-07-02, post first adversarial review).
Six agents (A–F) attacked the spec in parallel, each grounding claims in the
actual crates before proposing deletions. Coordinator synthesis first; full
agent reports follow.

# Coordinator

Overall verdict: **simplify** (unanimous — all six agents returned simplify;
none returned drop; the correctness skeleton survives every attack).

The storage-commit boundary, pin-overlap lease ordering, singleflight,
never-straddle, PONR discipline, quiesce discovery union, fail-closed boot
sweep, and reap-then-sweep all held. What did not survive is roughly a third
of the spec's surface: durable remount-adjacent metadata, a bespoke manager
dispatch layer, a persist-failure fallback defending a write-only file, a
mount-verification mode built on a false kernel premise, and an fsync plan
three orders of magnitude more expensive than the one syscall that gives the
same guarantee.

## Top deletions

1. **Durable substitution ledger → in-process map** (D). `<S-id>.sources.json`
   is persisted state whose only consumer is best-effort remount in the same
   daemon lifetime — the spec's own environment fact 3 says "No remount state
   is ever persisted", and this is remount state. No lease survives restart
   (fact 2), boot sweep reclaims by manifest keep-set and never reads ledgers.
   Replace with an in-memory per-root `{S-id → replaced raw run}` map beside
   `shared_registry_for_root` (the exact precedent, `lease/registry.rs:21`);
   rewrite becomes oldest-generation-first contraction of raw runs (verified
   equivalent on B4's generation-crossing case). Deletes with it:
   `schema_version`, the missing/unknown/degenerate fail-closed triad,
   build-time inner-S L-expansion, the "expand-then-contract" vocabulary and
   its sidecar-liveness reasoning, the `fs.rs (+30)` sources helpers, the
   `.sources.json` arm of every cleanup path, and half of test 8.
2. **The manager `checkpoint_squash` wrapper — trimmed, not deleted**
   (C + F findings, partially overridden by product decision 2026-07-02:
   squash is checkpoint behaviour and belongs to the manager, so the surface
   stays `sandbox-cli manager checkpoint_squash --sandbox-id ID`). What
   still deletes: the one-member `"checkpoint"` family (appears nowhere in
   the codebase; every existing manager spec uses `family: "management"` —
   use that), and `OperationEntry::internal` (+8) — the `cli: Option<…>`
   field already exists on `OperationEntry`, so `squash_layerstack`
   registers with `cli: None` and stays out of the runtime catalog with
   zero new mechanism. What the impl must fix: the spec models
   `checkpoint_squash.rs` on `destroy_sandbox`, but `destroy_sandbox` is a
   manager-**local** lifecycle op that forwards nothing
   (store transition + `stop_daemon` + `runtime.destroy_sandbox`); the impl
   should delegate to the existing generic forward path
   (`router/forward.rs` — endpoint lookup, Ready check, timeout) with the
   renamed op, not grow a bespoke client sequence. Same single wire round
   trip; the `checkpoint_squash` → `squash_layerstack` name pair is
   retained by decision.
3. **The active-handle persist fallback and the new workdir state** (B + A +
   C + E, converging from four directions). `manager.json` is write-only:
   zero production readers beyond boot reap, which uses only run-dir location
   and destroys wholesale ("never used to recover sessions" — fact 2). Delete
   the C5 persist-failure row, `mount_uncertain:active_persist_failed`,
   invariant 6's "never release the old lease without the new handle durably
   persisted" clause, and test 12; demote the per-migration rewrite to one
   best-effort `persist_handles()` call. Separately, `MountedWorkspace`
   already carries `dirs.workdir` (`session/state.rs:11`) and
   `persist_handles` already serializes it (`persistence.rs:30`) — delete the
   `state.rs (+6)` field and `persistence.rs (+4)` diff; the switch mutates
   `dirs.workdir` in place. Delete eager old-workdir deletion (invariant 5
   clause): retired workdirs live under `run_dir` and die with it.
4. **Real-path lowerdir mode, the exact-mountinfo probe, and the ≈97-lowerdir
   ceiling** (E). All three rest on the legacy `mount(2)` one-page
   option-string premise. The production builder uses fsopen/fsconfig with
   `lowerdir+` per layer (`userxattr` verified at `kernel_mount.rs:132`) —
   there is no silent truncation for a probe to catch; the only cap is
   `OVL_MAX_STACK` (500), and an over-limit staging mount fails the mount
   syscall itself as a clean pre-PONR skip. Delete real-path mode, the
   post-switch exact `lowerdir=` proof, the entire staged-path chain-length
   analysis in §D, and `stage_failed:lowerdir_limit` as a probed condition
   (derive the reason from the mount errno). `kernel_mount.rs` keeps only
   `move_mountpoint` + `strict_unmount` (+70 → ~+35). The read probe still
   gates PONR.
5. **The restore ladder → a two-line post-PONR rule** (E, overriding A/B's
   accepts — see Conflicts). (a) Strict-unmount EBUSY after a *verified*
   switch ⇒ resume on NEW, keep both leases in memory, release at destroy,
   report `leased(pinned:rollback_unmount_busy)` — next run is Identity.
   (b) Any other post-PONR failure ⇒ faulty + ordinary destroy, tasks still
   frozen so nothing observes partial state. Deletes the 4-step ladder, the
   "restore ladder"/"verified restore" vocabulary, and the per-run futile
   freeze→switch→EBUSY loop for parked-fd sessions (e2e test 7's scenario
   currently repeats forever; under (a) it converges once).
6. **The per-entry fsync walk → one `syncfs(2)`** (B). The durability
   requirement is real; the mechanism is O(E+D)+7b fsyncs that the spec
   itself names the wall-clock dominator ("~1–10 s per 1000 entries"). One
   `syncfs` on the storage-root fd immediately before the manifest rename
   covers staging trees, promote renames, and sidecars — strictly stronger,
   one syscall. Deletes the `fsync_tree` extension; fsync `layers/` once per
   commit, not per block.
7. **The shared commit-tail extraction (`publish.rs +40`)** (C block + D
   revise, same conclusion). The spec contradicts itself on the tail's order
   (phase 3: recheck→promote→sidecars→rename; the extraction item:
   promote→sidecars→recheck→write, which is publish's order at
   `ops/publish.rs:82-117` — with an equality recheck, not run-presence, and
   best-effort bytes *after* the manifest). A parameterized abstraction with
   1.5 users that forces reordering a proven durability sequence. Squash owns
   its own ~25-line commit built from the existing `fs.rs` primitives;
   `publish.rs` is untouched (0 LoC).
8. **S-layer `.digest`/`.bytes` sidecars** (D). No reader needs them: publish
   dedup tolerates NotFound (`ops/publish.rs:129-139`), binding validation is
   base-only, observability regenerates `.bytes` by walking
   (`collect/layerstack.rs:70-76`). With deletion 1, S layers get **zero**
   sidecars. Keep the `lease/cleanup.rs` fix — the leaked `.bytes` is real
   and verified (`cleanup.rs:38-48` removes dir + `.digest` only).
9. **Output/error surface** (A + F): delete `layers` (duplicates
   `LayerStack::observe()`) and `leases` (duplicates per-layer
   `active_lease_count`, stale at print time); demote `blocked_reasons` to
   free-form diagnostics (contract only "non-empty when leased") — no
   caller branches on class prefixes; delete the `manifest_conflict` error
   kind (unreachable under singleflight — verified `amend` also only
   prepends; surface as existing `operation_failed`); delete the
   `--progress` example (zero protocol/daemon progress support exists and §A
   budgets those crates +0) and add `faulty_sessions:
   [{session_id, class_detail, lease_errors}]` to the stdout result — F's
   one **block**: the mandated faulty report currently has no field to live
   in, and a faulty session's destroy can flip its block to `"reclaimed"`,
   vanishing the session from the JSON entirely. Delete `upperdir_bytes`
   from the faulty report (requires a du-walk on the failure path; violates
   the spec's own byte-accounting rule).
10. **File plan: 12 new files → 7** (C). Delete `stack/sweep.rs` (the sweep
    is `remove_layers` with candidates = disk listing; put it beside
    `unreferenced_layers`/`remove_layers` in `lease/cleanup.rs` so both
    deletion paths share one sidecar-set definition); merge `recover.rs`
    into `lifecycle/persistence.rs` (sole owner of the `manager.json` path
    and schema); fold `squash/rewrite.rs` into the lease module (it is a
    lease operation, and with deletion 1 it shrinks to raw-run contraction);
    manager file and publish extraction gone per deletions 2 and 7.
11. **Two admission mechanisms → one** (A + B, same finding). The existing
    global `session_lifecycle_lock` (`command/service/core.rs:28`) coexists
    with the new per-session gate, unordered by the spec's lock-discipline
    line — a deadlock surface and duplicate serializer. Subsume it into the
    per-session gate; nothing it protects is cross-session.
12. **Phase-tagged runner report → two booleans + free-form detail** (A,
    tightened by deletion 5). C5 is a pure function of
    `first_move_succeeded` and `mount_verified` (restore-verified died with
    the ladder); the phase enum is consumed only by reporting and survives
    inside the free-form detail string. Missing report at/past
    first-move-success ⇒ faulty, unchanged.

## Minimal spec patch plan

1. Delete the `LayerSubstitution` durable ledger; replace with the in-memory
   per-root raw-run map; rewrite = oldest-first contraction; simplify the
   "rewritten manifest" and `acquire_rewritten_lease` vocabulary accordingly.
2. Keep the manager surface `sandbox-cli manager checkpoint_squash
   --sandbox-id ID` (product decision); delete `OperationEntry::internal`
   (register `squash_layerstack` with the existing `cli: None`) and the
   one-member `"checkpoint"` family (use `"management"`); the manager impl
   delegates to the existing generic forward (`router/forward.rs`), not a
   bespoke client sequence modeled on the non-forwarding `destroy_sandbox`.
3. Delete the persist-failure C5 row, invariant 6's persist clause,
   `active_persist_failed`, test 12, `state.rs (+6)`, `persistence.rs (+4)`,
   and eager old-workdir deletion; keep one best-effort `persist_handles()`.
4. Delete real-path mode, exact-mountinfo probe, §D staged chain-length
   analysis, `lowerdir_limit` probing; keep `move_mountpoint` +
   `strict_unmount`.
5. Replace the restore ladder with the two-rule post-PONR policy
   (EBUSY-after-verified-switch ⇒ both-leases + resume on NEW; anything else
   ⇒ faulty-destroy); narrow `RemountMaskGuard` to the staged build only
   (moves via pre-opened dirfds + `move_mount(MOVE_MOUNT_F_EMPTY_PATH)`),
   making remask failure a clean pre-PONR skip.
6. Replace the bottom-up fsync walk + per-block `fsync layers/` + sidecar
   fsyncs with one `syncfs` before the manifest rename (+ the existing
   `write_atomic` manifest commit).
7. Delete the `publish.rs (+40)` shared-tail extraction; pin the phase-3
   "commit-time GC" to be literally `release_lease` on the plan lease
   (extend it to return the removed set; forbid a second deletion routine).
8. Delete S-layer `.digest`/`.bytes` writes; keep the `cleanup.rs` sidecar
   removal fix (now `.digest` + `.bytes` only).
9. Result JSON: `manifest_version` + `squashed_blocks{squashed_layer_id,
   replaced_layer_ids, replaced_layers, blocked_reasons}` + `faulty_sessions`
   (omitted when empty); `blocked_reasons` free-form; drop `layers`, `leases`,
   `manifest_conflict`, `--progress`, `upperdir_bytes`.
10. Merge `sweep.rs` → `lease/cleanup.rs`, `recover.rs` → `persistence.rs`,
    `rewrite.rs` → lease module; subsume `session_lifecycle_lock` into the
    per-session gate and state the complete lock order; drop the boot-sweep
    "never crosses a mount boundary" clause (triple-covered: fail-closed
    parse, keep-set + B*, kernel-enforced `:ro` base volume); drop per-task
    `mountinfo` reads (one holder read; `ns/mnt` equality makes the rest
    identical); runner report = two booleans + detail.
11. Terminology: delete the "clean remount skip" vocab row (C5 row 1 is the
    source of truth); fix the "pin" overload (lease boundaries are
    "boundary", quiesce references are "pin"); fix the plan-lease rationale
    (its real job is the zero-new-code GC trigger at commit, not build-time
    pinning — sources stay in the active manifest through the build).

Net: ≈ −250 source LoC and 5 fewer new files against a plan of ~1,580 new
LoC; two durable artifacts (ledger sidecar, per-migration handle rewrite)
reduced to zero-or-best-effort; the publish commit path untouched; fsync
count per squash from O(E+D)+7b to ≈3.

## Fallbacks to remove

- Persist-failure keep-both-leases row (defends a write-only file).
- Restore ladder (replaced by both-leases-on-EBUSY / faulty otherwise).
- Missing/unknown-version/degenerate ledger triad (unreachable with the
  in-memory map).
- `manifest_conflict` as contract vocabulary (recheck stays, internal).
- `stage_failed:lowerdir_limit` as a probed condition (mount errno suffices).
- Boot-sweep mount-boundary detection (triple-covered).
- Post-PONR "restore masks" step (remask pre-PONR via dirfd moves).
- Pin-inspection of allowlisted infrastructure (daemon-owned; a missed infra
  pin surfaces as EBUSY and is absorbed by the new post-PONR rule (a)).
- `--progress` streaming dependency (no protocol support exists; +0 budget).

## Fallbacks that are truly required

- Fail-closed boot sweep guard (missing `manifest.json` parses as empty v0 at
  `fs.rs:170-172`; without the guard, keep-set = ∅ deletes every layer).
- Both quiesce discovery legs: cgroup placement is best-effort/`Option`
  (`launcher.rs:344-348`) so the ns-scan is the freeze-set proof; the cgroup
  union is the only detector of mnt-ns-escaped tasks (escape does NOT
  surface as rollback EBUSY — a copied vfsmount pins layers invisibly).
- Plan lease + its release as the commit GC (deleting it forces a new
  candidate-list GC entry point; `unreferenced_layers` only evaluates the
  released lease's own layers, so ex-manifest sources would leak unboundedly).
- Boot sweep AND lease-release GC (two mechanisms, not three: commit GC ==
  plan-lease release; crash orphans are invisible to the release path).
- Pin-overlap lease ordering and per-session lock scoping (shared
  `acquire_rewritten_lease` / exclusive release; batching the shared side
  would hold the writer lock across quiesce — forbidden by the spec's own
  discipline).
- Both-leases-in-memory with exactly one trigger: strict-unmount EBUSY after
  a verified switch (the old overlay superblock is alive and reading
  lowerdirs; releasing the old lease would delete layer dirs in use).
- Missing/ambiguous runner report at/past first-move-success ⇒ faulty (the
  minimum that disambiguates clean skip from partial switch).
- Identity short-circuit stays; "no registry short-circuit for predictably
  pinned sessions" also stays (Identity is durable data already in hand; a
  pinned-prediction skip is a second, staleness-prone classification path).

## Terms / files / states to remove

- Terms: `LayerSubstitution`, `schema_version`, "expand-then-contract" (as
  vocabulary; the operation becomes raw-run contraction), "restore ladder",
  "verified restore", "clean remount skip" (row duplicate), the
  `"checkpoint"` family label (the `checkpoint_squash` name stays by
  decision; the family is `"management"`), `manifest_conflict`,
  `active_persist_failed`, `lowerdir_limit` (as probed class detail), phase
  enum names in the runner protocol, "pin" for lease boundaries (use
  "boundary").
- Files: `stack/sweep.rs`, `workspace/lifecycle/recover.rs`,
  `stack/squash/rewrite.rs` (as standalone); diffs `publish.rs (+40)`,
  `session/state.rs (+6)`, `persistence.rs (+4)`, `operation.rs (+8
  OperationEntry::internal)`. The manager block
  (`checkpoint_squash.rs`, `cli_definition +30`, registration) stays by
  product decision, minus the new family and modeled on the generic forward.
- States: the second workdir field on `MountedWorkspace`; the durable ledger
  sidecar; the persist-failure dual-lease state (survives only as the
  EBUSY-triggered in-memory pair); `restore_verified` as a report field.

## Conflicts adjudicated

- **Restore ladder** — A and B said keep (deleting it destroys healthy
  sessions on benign EBUSY and leaves a half-switched tree); E said delete.
  E wins because E's replacement isn't "destroy on EBUSY": at EBUSY time the
  switch is already verified (steps 2–6 passed), so the tree is *fully*
  switched — NEW at root, OLD parked at the masked rollback point — and
  resuming on NEW with both leases held is safe, converges to Identity next
  run, and reuses a pattern the spec already had. A/B's objection targeted a
  proposal nobody is making.
- **Keep-both-leases** — B deleted it (persist-failure trigger defends
  nothing); E reused it (EBUSY trigger). Both are right: the mechanism
  survives with exactly one trigger, where it is correctness-bearing (live
  old superblock), and the persist trigger dies.
- **`replaced_layer_ids`** — A deleted it (recoverable from the durable
  ledger); F kept it. F wins *because of D*: with the ledger in memory, the
  result line is the only surviving record of what a squash replaced, it is
  plan-time data already in hand (zero extra round trips), and it is the one
  field not recoverable later from `observe()`.
- **CLI shape** — A renamed the manager op in place; C and F deleted the
  manager layer entirely. C/F won on the evidence (generic forwarding
  exists; `destroy_sandbox` is not a forwarding template), but the product
  decision overrode the maximal deletion: squash is checkpoint behaviour
  and belongs to the manager. Resolution: keep
  `manager checkpoint_squash --sandbox-id ID`, apply the evidence anyway —
  the impl rides `router/forward.rs`, the family is the existing
  `"management"`, and `OperationEntry::internal` still dies (`cli: None`).

## Decisions (resolved 2026-07-02)

1. **CLI surface**: `sandbox-cli manager checkpoint_squash --sandbox-id ID`
   stays — squash is checkpoint behaviour owned by the manager. Trims that
   survive the decision: no `"checkpoint"` family, no
   `OperationEntry::internal`, impl delegates to the generic forward.
2. **`syncfs`**: supported Docker kernels are ≥ 6.0, comfortably past the
   5.8 writeback-error-reporting floor. The single-`syncfs` commit
   (deletion 6) is confirmed viable. The boot/e2e harness still asserts
   `uname -r` ≥ 5.8 once at startup as a cheap environment gate.
3. **e2e depth**: expanded into the detailed plan below (§Detailed live
   Docker e2e plan) — per-test purpose, setup, induction technique,
   assertions, and teardown, with no test-only code in `src/`.
4. **No auto-squash policy exists** — confirmed against both the spec
   ("zero options, zero trigger policy"; "there is no trigger policy") and
   the codebase (a full-tree grep finds no squash/checkpoint/trigger
   machinery anywhere; the sole match is a comment in
   `layerstack/tests/occ_merge_bench.rs`). This spec designs only the
   squash + live-remount algorithm behind one explicit manual invocation;
   any future auto-squash trigger is a separate spec with its own review.

## Consolidated required tests (proving deleted complexity unnecessary)

From the agents, deduplicated; each pins a deletion:

1. `in_memory_substitutions_match_expand_then_contract` (D) — B4 replay via
   the map; post-restart squash never reads a ledger.
2. `squash_commits_with_no_s_layer_sidecars` (D) — publish dedup-miss is
   silent; observability self-heals `.bytes`.
3. `persist_failure_still_migrates` + `stale_manager_json_harmless_at_boot`
   (B) — the deleted keep-both-leases persist row was unnecessary.
4. `both_leases_on_rollback_unmount_busy_converges_next_run` (E) — parked-fd
   session: one switch, both leases, Identity next run; no ladder.
5. `staged_mount_over_ovl_max_stack_is_clean_skip` (E) — no lowerdir-limit
   probe needed; mount errno classifies.
6. `syncfs_commit_durability` (B) — power-fail equivalence with the deleted
   per-entry walk (replaces test 7's fsync-recording shim).
7. `checkpoint_squash_forwards_via_generic_router` (C/F, adjusted for the
   CLI decision; replaces test 17) — manager catalog exposes
   `checkpoint_squash` under `"management"`; runtime catalog does not
   expose `squash_layerstack` (`cli: None`, no `OperationEntry::internal`);
   the impl reaches the daemon through `router/forward.rs`.
8. `single_gate_serializes_exec_and_remount` /
   `single_admission_path_orders_destroy_and_remount` (A/B) —
   `session_lifecycle_lock` subsumed, no deadlock, complete lock order.
9. `squash_result_minimal_contract` (A/F, replaces test 16) — result carries
   exactly the surviving fields; `observe()` serves the rest.
10. `faulty_session_appears_in_result_json` (F) — `faulty_sessions` present
    with no dependency on progress or observability; no byte walk (A).
11. `runner_report_two_booleans_drive_policy` (A, adjusted) — every C5 row
    reproduced from `first_move_succeeded` + `mount_verified` + detail.
12. `commit_gc_is_plan_lease_release` (D) — no second deletion routine.
13. `boot_sweep_safe_without_mount_boundary_check` +
    `boot_sweep_and_lease_release_share_deletion_set` (D/C) — fail-closed +
    keep-set + B* suffice; one deletion routine, no leaked sidecars.
14. `remount_updates_existing_workdir_field` /
    `remount_persists_workdir_via_existing_handle_schema` (C/E) — existing
    `dirs.workdir` + `persist_handles`, no schema change.
15. `single_holder_mountinfo_read_blocks_child_mounts` (E) — per-task
    mountinfo reads deleted.
16. `bytes_sidecar_removed_on_lease_gc` (D) — regression test for the real,
    pre-existing `.bytes` leak.
17. `publish_path_untouched_by_squash` (C) — `stack/ops/publish.rs` has zero
    diff after the feature lands.

## Detailed live Docker e2e plan (revised for the adopted deletions)

Replaces the spec's ten-item "Live Docker e2e" list. Structure: three **gate
tests** (G1–G3) that must pass in the supported Docker environment before
live remount is enabled — any gate failure leaves squash commit-only, with
every session reported `leased(unsupported:…)` — then ten **feature tests**
(E1–E10). Every test is written to need **zero test-only code in `src/`**
(repo rule): failures are induced naturally or by killing/observing the
runner from outside, never via in-source fault-injection flags.

### Harness ground rules (apply to every test)

- **Environment preconditions, asserted once per suite, hard-fail not
  skip**: `uname -r` ≥ 5.8 (syncfs error reporting; supported kernels are
  ≥ 6.0); the layer-stack root's backing filesystem is not overlayfs
  (`findmnt -no FSTYPE` — overlay-on-overlay is unsupported and would make
  every result meaningless; the Docker gateway's seeded shared base volume
  provides this); `userxattr` overlay mounts work unprivileged in the
  sandbox userns.
- **Phase observation without src hooks**: the daemon-side test observes
  runner progress by polling `/proc/<holder-pid>/mountinfo` from outside
  the namespace — the staging mount appearing = staged build done; the
  workspace root's mount ID changing = first `MS_MOVE` landed. This gives
  deterministic kill points for E8/E10 with no protocol or runner changes.
- **Timing discipline**: tests that assert a *successful* freeze use a
  generous budget (≥ 2 s) so loaded CI cannot flake them; tests that assert
  `quiesce_failed:freeze_timeout` construct the straggler explicitly (see
  E4) rather than relying on load.
- **Teardown is part of the assertion**: every test ends by destroying its
  sessions, then asserting the lease registry is empty (`observe()`:
  `active_lease_count == 0` on every layer), no `.remount-staging-*` or
  `.remount-rollback-*` entries remain in the holder's mountinfo, and
  `staging/` is empty. Teardown must use strict unmount only — a lazy
  detach in teardown would mask exactly the leak class these tests exist
  to catch. A teardown failure fails the test loudly.
- **Witness-file convention** (used by G1/G2/E5): each source layer `Li`
  carries `wit/only-in-Li`, one file deleted-in-`Li+1` (whiteout winner),
  one dir created-then-emptied, and one file whose mode is non-default —
  so "merged view equivalence" is asserted by concrete reads
  (presence, absence, dir shape, mode), not by mount-option introspection.

### Gate tests

- test name: G1 `same_upperdir_fresh_workdir_kernel_gate`
  purpose: the one load-bearing kernel assumption — OLD and NEW overlays
  coexist on the same upperdir (NEW with a fresh sibling workdir) long
  enough for the staged `MS_MOVE` switch, with production options. Failure
  ⇒ wrong filesystem view (copy-up corruption); this is the test that
  fails if the workdir is shared (the removed experiment's defect).
  setup: two lowerdir sets with equivalent merged content — `L_old = [l2,
  l1]`, `L_new = [S(l2,l1)]` built by a flatten of the witness layers; one
  upperdir `U`; workdirs `W_old` and fresh sibling `W_new`. All mounts via
  the production fsconfig builder (`userxattr`, no `index`).
  steps: (1) mount OLD at a workspace-shaped path; (2) write through OLD to
  force a copy-up (witness `cow-before`); (3) mount NEW at staging with the
  same `U`, fresh `W_new`; (4) read-probe NEW against the witness set;
  (5) `MS_MOVE` OLD→rollback, staging→root; (6) probe visible mount;
  (7) `umount2(rollback, 0)` strict; (8) **abort leg**: repeat (1)–(3) in a
  fresh tree, unmount NEW *without* any move, then write through OLD again
  (witness `cow-after-abort`).
  expected result: every witness read exact on NEW and on the visible
  post-switch mount; step 7 returns 0 with no residual users; step 8's
  copy-up succeeds and `cow-after-abort` is durable — this is the assertion
  that catches a shared workdir. On ANY failure: the suite asserts squash
  still commits compact layers and reports all sessions
  `leased(unsupported:kernel_gate_not_proven)` — the gate gates, it does
  not crash.
- test name: G2 `production_builder_parity_no_resurrection`
  purpose: replaces the deleted `real_path_mode_parity` test. Parity
  (`userxattr`, no `index`) holds by construction because staging uses the
  same production builder as creation; this proves it *behaviorally* —
  specifically that a file deleted through OLD stays deleted on NEW (the
  removed helper's missing-`userxattr` defect resurrected files).
  setup: overlay OLD via the production builder; delete `wit/only-in-l1`
  through the mount (upperdir whiteout, userxattr encoding); flatten
  sources into `S`; staged NEW = `[S]` + same upperdir + fresh workdir.
  steps: probe NEW for the deleted file and for a whiteout that flatten
  re-emitted inside `S`; negative control: rebuild NEW once with a
  deliberately misconfigured *test-local* mount (no `userxattr`) and assert
  the deleted file resurfaces — proving the assertion has teeth.
  expected result: absent on NEW via the production builder; present under
  the negative control; no `index=on` artifacts in the upperdir. No
  mountinfo lowerdir introspection anywhere — behavioral witnesses only
  (per the resolved open question, lowerdir-list introspection is not
  needed even as test tooling).
- test name: G3 `startup_cleanup_reap_then_sweep`
  purpose: startup cleanup proof — leftover handles and run dirs are reaped
  before any boot storage sweep; PDEATHSIG makes holders provably dead.
  setup: daemon with two sessions (one idle, one with a live PTY command),
  plus a hand-planted orphan `staging/S…-nonce.staging/` tree and an orphan
  promoted `layers/S…/` dir not in the manifest.
  steps: `SIGKILL` the daemon; poll `/proc` until holder and pid-ns init
  are gone (bounded wait — this asserts PDEATHSIG, not deployment luck);
  restart the daemon; capture observability records in order.
  expected result: every reap record precedes the first sweep deletion
  record; run dirs and handles gone; `staging/` empty; orphan S dir and its
  sidecars gone; layers on disk == active manifest exactly; a new session
  creates and runs a command afterwards. Repeat once with `manifest.json`
  made unreadable: nothing is deleted (fail-closed), daemon still serves.

### Feature tests

- test name: E1 `all_task_quiesce_blocks_escaped_pgid_child`
  scenario: a batch command `setsid()`s a child that holds an open fd on
  `/workspace/f`; the child is thus outside the command pgid. Invoke
  `checkpoint_squash`.
  expected result: discovery (cgroup ∪ ns-scan) finds the child; it reaches
  state `T` within the budget; inspection reports
  `pinned:fd_pinned_workspace`; block reports `leased`; both parent and
  child are `SIGCONT`ed (assert state `S`/`R` after) and the command
  completes normally; old lease intact; next squash after the command exits
  migrates the session.
- test name: E2 `nested_mount_namespace_blocks_remount`
  scenario: session runs `unshare -m sleep inf` — the escapee holds **zero**
  workspace fds; its copied vfsmount is the only pin.
  expected result: `pinned:mount_namespace_escaped`; no `MS_MOVE` is ever
  attempted (assert the workspace root's mount ID never changes across the
  sweep — the outside-observation technique); old layers retained; after
  killing the escapee, the next run migrates. This test exists because
  strict-unmount EBUSY does NOT subsume escape detection — a copied
  vfsmount pins layers without making the holder's rollback unmount busy.
- test name: E3 `masks_never_observable_and_mask_failure_is_clean_skip`
  scenario: (a) happy path: live migration of a batch session while an
  allowlisted-infra-shaped observer (the holder itself, via a daemon-side
  read through `/proc/<holder>/root`) stats the hidden daemon paths in a
  tight loop for the whole sweep; (b) failure leg: make remask impossible
  before the moves by exhausting the mask tmpfs (mount the mask source
  read-only under the test's control), naturally forcing the
  narrowed `RemountMaskGuard` to fail *pre-PONR*.
  expected result: (a) the observer never sees a hidden path unmasked from
  any resumable task's perspective — every non-allowlisted task is frozen
  for the entire unmask window, and post-resume stats return the masked
  view; (b) clean skip `leased(stage_failed:…)`, masks verifiably restored
  (stat from inside the session after resume), no move attempted, no
  faulty outcome possible from mask handling (this pins the
  remask-before-moves narrowing — under the old design this failure was
  post-PONR).
- test name: E4 `proc_pin_matrix_blocks_uncertainty`
  scenario: one sub-case per pin class, each in its own session, one sweep:
  cwd inside workspace (interactive PTY bash — the physics case); `chroot`
  into the workspace (root pin); open fd; `mmap` of `/workspace/a b.txt`
  (space in path — offset parsing, never "last column"); a mapping whose
  backing file was deleted (`(deleted)` suffix ⇒ pinned); a bind mount
  created inside the workspace by a task that has already *exited* — the
  child mount must still block via the ONE holder mountinfo read, proving
  per-task mountinfo reads are gone; an `io_uring` anon fd; a `ptrace`
  tracer outside the frozen set (`t` state); an unreadable `/proc` entry
  (drop read permission via a setuid victim — any read error = pinned);
  membership churn (a fork loop ⇒ `quiesce_failed:membership_changed`); a
  freeze straggler for `freeze_timeout` (a task writing into a
  test-controlled `fsfreeze`-frozen nested fs enters D-state; mark this
  sub-case best-effort/optional — D-state construction is inherently
  environment-sensitive).
  expected result: each sub-case yields its expected `class:detail`
  (free-form per the demoted contract, but asserted here for coverage);
  every session resumes and runs a follow-up command; zero `MS_MOVE`s
  (mount IDs unchanged); old leases intact; no replacement lease leaked
  (registry count returns to baseline).
- test name: E5 `live_migration_under_running_batch_command`
  scenario: B5's ws-3 — a batch command blocked on a socket with no
  workspace pins, upperdir writes made before the freeze; full live
  protocol.
  expected result: migrated; the command never errors and its post-resume
  reads see pre-freeze upperdir writes; absolute-path lookups land on the
  NEW mount; the holder's workspace overlay now lists the compact chain
  (assert lowerdir count dropped via the witness reads, not mountinfo
  options); old source dirs deleted from disk after the sweep; result JSON
  block flips to `reclaimed` when this was the last pinning session.
- test name: E6 `strict_unmount_ebusy_keeps_both_leases_and_converges`
  scenario: the SCM_RIGHTS trick — the command opens `/workspace/f`, sends
  the fd to itself over a socketpair, closes the local copy (the fd now
  lives only in the socket queue, invisible to `/proc/*/fd` inspection),
  and blocks. Freeze finds zero pins; switch verifies; strict
  `umount2(rollback, 0)` returns EBUSY.
  expected result: per adopted rule (a): session resumes on NEW; report
  `leased(pinned:rollback_unmount_busy)`; BOTH leases held (registry shows
  two); reads and copy-up work on NEW; old run NOT deleted. Second
  `checkpoint_squash`: the session short-circuits as Identity — **no
  second freeze, no second switch attempt** (this is the assertion that
  kills the old ladder's every-run retry loop). Destroy the session:
  namespace death releases both leases; old run and the parked rollback
  mount are gone (mountinfo clean); layers reclaim. This test pins
  deletion 5 end to end.
- test name: E7 `post_ponr_unverified_failure_is_faulty_destroy`
  scenario: induce a post-PONR failure with no in-src injection: after the
  staged build is observed (staging mount present), the test `SIGKILL`s the
  runner the instant the workspace root's mount ID changes (first move
  landed) — the runner dies between the moves, its report never arrives.
  expected result: missing report at/past first-move-success ⇒ faulty;
  stdout JSON carries `faulty_sessions: [{session_id, class_detail,
  lease_errors}]` (no `upperdir_bytes`, no phase enum — free-form detail
  only); the session is destroyed via the ordinary destroy path; namespace
  death releases both leases; all layers only it pinned reclaim; the
  committed manifest is untouched; every other session in the same sweep
  is unaffected; exit code 0 (the squash committed).
- test name: E8 `ponr_boundary_two_boolean_report`
  scenario: three kill/failure points, all externally induced: (i) a
  *failed first move* — arrange the rollback scratch mountpoint on a
  shared-propagation mount so `MS_MOVE` fails `EINVAL` (moves out of
  shared parents are rejected by the kernel); (ii) runner killed after the
  staged build but before any mount-ID change; (iii) runner killed after
  the visible probe (mount ID changed, staging gone, report suppressed by
  the kill).
  expected result: (i) and (ii) are clean skips — `first_move_succeeded`
  false or report present-and-pre-PONR, session untouched, `leased(…)`;
  (iii) goes faulty (missing report past first-move-success). All three
  outcomes are pure functions of `first_move_succeeded` +
  `mount_verified` + presence — pinning the two-boolean report reduction.
- test name: E9 `staged_mount_over_ovl_max_stack_is_clean_skip`
  scenario: build a session whose rewritten chain still exceeds
  `OVL_MAX_STACK` (500) — e.g. 501 pinned singletons so no block forms
  below the pins; also attempt workspace *creation* at 501 layers.
  expected result: creation fails with the distinct documented error;
  the staged remount mount syscall itself fails (no probe, no separate
  limit detector), classified from the errno as a clean pre-PONR
  `leased(stage_failed:…)`; the old lease intact; the outcome is stable
  across repeated runs with no side effects. Pins deletion 4 (no ≈97
  ceiling exists; no `lowerdir_limit` probing machinery).
- test name: E10 `crash_matrix_recovery`
  scenario: daemon `SIGKILL`ed at four externally-observed points:
  mid-freeze (tasks in `T`); mid-switch (between the moves, detected via
  mount-ID change); after the switch but before old-lease release; and
  between promote and manifest rename (detected by `layers/S…` existing
  while `manifest.json` is old).
  expected result: in every case the holder and pid-ns init die with the
  daemon (poll-asserted — including the mid-freeze case: `SIGKILL`
  works on stopped tasks and pid-ns-init death kills the namespace);
  restart runs reap-then-sweep; disk == active manifest exactly (the old
  manifest for the pre-rename case — the crash-orphan S dir and sidecars
  are swept); no session state resurrects; a fresh session plus a fresh
  `checkpoint_squash` both succeed. No remount-specific recovery branch
  exists to test — that absence is the assertion.

### Explicitly not covered in e2e

- **Commit durability (`syncfs`) under power failure**: not e2e-testable in
  Docker (a container kill does not drop the page cache). It stays at
  unit/integration level with an fsync/syncfs-recording shim in `tests/`
  (consolidated test 6, `syncfs_commit_durability`); a dm-flakey rig is out
  of scope. The e2e suite asserts only the kernel-version floor (≥ 5.8).
- **`manager.json` persist-failure**: deleted from the design (deletion 3);
  its e2e slot is covered by E10's crash matrix plus the unit-level
  `stale_manager_json_harmless_at_boot`.

# Full agent reports

---

## Agent A — State And Terminology Deletion

Agent A verdict: simplify

Findings:
- [revise] Section A `crates/sandbox-runtime/workspace/src/session/state.rs (+6)` + `src/lifecycle/persistence.rs (+4)`: the "MountedWorkspace gains the active workdir path" field is copied state. `MountedWorkspace` already carries `dirs: OverlayDirs { run_dir, upperdir, workdir }` (verified: `session/state.rs:11`, `overlay/dirs.rs`), and `persist_handles` already serializes `"workdir": handle.dirs.workdir` into `manager.json` (verified: `lifecycle/persistence.rs:30`). Adding a second workdir path creates two fields that can drift.
  delete/merge: delete the new field and the persistence diff; the verified switch mutates `dirs.workdir` in place to `work-remount-<n>` (it lives under `run_dir` per C3, so destroy/boot-reap already cover it). C5 row 2 becomes "swap snapshot + dirs.workdir".
  replacement: existing `OverlayDirs.workdir` + existing `persist_handles` (`crates/sandbox-runtime/workspace/src/lifecycle/persistence.rs`).
  why correctness still holds: nothing breaks — no code path re-reads the creation-time workdir after mount; the stale old workdir dir is bounded (one per remount, under `run_dir`) and reclaimed by the ordinary destroy path and boot reap. This also closes the last place remount state could smuggle back into the session struct: the only session-state deltas are the two pre-existing fields (`snapshot`, `dirs.workdir`).

- [revise] Output contract (`layers`, `leases`, `replaced_layer_ids`): three of the five output fields are reporting duplicates of durable or already-exposed state. `layers` duplicates the active manifest, already served by `LayerStack::observe()` (`crates/sandbox-runtime/layerstack/src/observability.rs`, `StackObservation.layers`) via the existing observe/read ops (`crates/sandbox-runtime/operation/src/layerstack/service/impls/{observe,read}.rs`); `leases` duplicates `observe()`'s per-layer `leased_by_workspaces`/`active_lease_count`; `replaced_layer_ids` duplicates the persisted `<S-id>.sources.json` ledger the spec itself makes durable (`LayerSubstitution`). The spec already applies this exact logic to bytes ("byte accounting stays with the daemon `layerstack` observability view") but not to layers/leases.
  delete/merge: delete `layers`, `leases`, `replaced_layer_ids`; result = `manifest_version` + `squashed_blocks: [{squashed_layer_id, replaced_layers, blocked_reasons}]`. Also deletes the post-sweep lease-registry read that exists only to compute `leases`.
  replacement: existing layerstack observe/read operation for stack+lease shape; `.sources.json` ledger for source ids.
  why correctness still holds: nothing breaks — no correctness path consumes the operation output.

- [revise] Vocabulary "faulty remount", C5 faulty row, test 15, `--progress` example (`upperdir_bytes=1832`): "upperdir bytes discarded" requires a du-walk of the upperdir on the failure path and exists only for reporting. It directly contradicts the spec's own rule ("No … byte totals — byte accounting stays with the daemon `layerstack` observability view").
  delete/merge: delete "upperdir bytes" from the faulty report everywhere (vocab row, C5, C2, test 15, progress example); report = session id + `class:detail` + lease-release errors.
  replacement: none needed; if byte accounting is ever wanted it belongs to the observability view per the spec's own rule.
  why correctness still holds: nothing breaks — the bytes inform no recovery decision; the session is destroyed either way.

- [revise] Vocabulary "point of no return" / C3 step 8 "phase-tagged report ALWAYS": the C5 decision table branches on exactly three facts — `first_move_succeeded`, `mount_verified`, restore-verified. The phase enum (which of 8 steps) is consumed only by the faulty report, i.e. reporting. Even the missing-report rule ("missing or ambiguous ⇒ post-PONR") needs no phase tag — C2 already classifies a missing report as faulty outright.
  delete/merge: collapse the phase-tagged report to three booleans + one free-form `class:detail` string; delete the phase vocabulary from the runner protocol (`namespace-process/src/runner/setns/remount_overlay.rs`, protocol fields in `runner/{setns,protocol}.rs`).
  replacement: the three booleans, which the C5 table already fully consumes.
  why correctness still holds: nothing breaks — every C5 outcome is a pure function of the three booleans; the phase string survives only inside the free-form detail if wanted.

- [revise] C1 "there is no separate lifecycle lock for this feature" vs `crates/sandbox-runtime/operation/src/command/service/core.rs:28`: the repo already has a global `session_lifecycle_lock` (used by `destroy_workspace_session_with_admission`), and the spec's own diff touches this file (+15, routing exec/finalize through the new gate) while leaving the old lock in place. Result: two coexisting admission mechanisms, and the spec's lock-discipline line ("sessions-map mutex < per-session admission gate < storage writer lock") does not order the existing lifecycle lock at all — an unordered lock crossing the same destroy path.
  delete/merge: subsume `session_lifecycle_lock` into the per-session admission gate (destroy takes only the gate; the active-command check moves under it), or explicitly add the old lock to the lock-order. Prefer deletion — one admission concept.
  replacement: the spec's own per-session admission gate in `WorkspaceSessionService` (which already owns `sessions: Mutex<HashMap<…>>`, `service/core.rs:13`).
  why correctness still holds: destroy-vs-remount and destroy-vs-finalize mutual exclusion is per-session; the per-session gate provides exactly that. Nothing the global lock protects is cross-session (the live-command check is filtered per session id).

- [revise] CLI surface + Section A manager diff: `checkpoint_squash` (manager name, new single-member family `"checkpoint"`) vs `squash_layerstack` (daemon op) — two names and one new family label for one operation. Every existing manager spec uses `family: "management"` (verified: `cli_definition/management_operations.rs`, all five specs). The `"checkpoint"` family exists only for future extension.
  delete/merge: name the manager command `squash_layerstack` under the existing `"management"` family; delete the `checkpoint` vocabulary and the name translation in `impls/checkpoint_squash.rs`.
  replacement: existing `"management"` family and 1:1 op-name forwarding (modeled on `destroy_sandbox`, as the spec already says).
  why correctness still holds: nothing breaks — pure naming; test 17 asserts one catalog entry either way.

- [revise] Vocabulary table: two doc-level redundancies. (a) "clean remount skip" restates C5 row 1 verbatim ("Abort before the first successful MS_MOVE … clean skip"); (b) "pin" is overloaded — the vocab defines "pin" as a frozen-task workspace reference, while "boundary" and every diagram legend (`◀ pin = newest layer of a live lease's manifest`, B2/B3/B4) use "pin" for lease boundaries. Two unrelated concepts share one word.
  delete/merge: delete the "clean remount skip" vocab row (C5 row 1 is the source of truth); rename lease-boundary usage to "boundary" in all diagrams, reserving "pin" for quiesce inspection.
  replacement: C5 failure table; "boundary" term.
  why correctness still holds: nothing breaks — terminology only.

- [accept] Vocabulary "plan lease" / `acquire_rewritten_lease` / "old-lease release": verified genuine reuse, keep — but fix the plan-lease rationale. `acquire_snapshot` exists (`stack/mod.rs:76`), `LeaseRegistry::acquire` already takes an arbitrary `Manifest` (`lease/registry.rs`) so the rewritten lease needs no new registry API, and `release_lease_locked`'s GC spares active-manifest and leased layers (`lease/cleanup.rs`). Note: the vocab claim "it pins every source for the lock-free build" is redundant — sources stay in the *active manifest* through the whole build (publishes only prepend), and `unreferenced_layers` already spares active-manifest layers, so nothing can delete them mid-build. The plan lease's real (and only) job is to be the zero-new-code GC trigger at commit: releasing it deletes exactly the replaced sources. Keep it for that; deleting it would force a new candidate-list GC entry point, which is more code, and mis-triggered deletion of sources would violate "needed layer released".

- [accept] "restore ladder" (vocab, C3 step 7): keep. Deleting it turns every rollback-unmount EBUSY (e.g. the SCM_RIGHTS-parked fd of e2e test 7, invisible to `/proc` inspection) into destruction of a healthy live session. That is not on the core-rule list, but it converts a provably-recoverable state into session loss on a background storage pass; the ladder is four syscalls on an already-failed path, not a mode, and it produces no state — its outcome collapses into the existing `leased`/faulty classification.

Round trips / fallbacks to delete:
- item: post-sweep lease-registry read to populate the per-block `leases` count.
  reason: exists only for the deleted output field; `replaced_layers: reclaimed|leased` is decidable inside the phase-3 exclusive section from what the plan-lease release GC removed.
- item: upperdir size walk on the faulty path.
  reason: reporting-only; contradicts the spec's own byte-accounting rule.
- item: the "registry refresh" step in `remount_session.rs` should be named as the existing `WorkspaceSession::refresh_after_capture` idiom (`operation/src/workspace_session/service/model.rs:41`), not a new concept — it is required (capture reads the copied `handle.snapshot.lease_id`), but it is the existing pattern, not new machinery.

Required tests:
- test name: `active_workdir_persists_via_overlay_dirs`
  scenario: live migration succeeds; no new MountedWorkspace field exists; inspect `manager.json`.
  expected result: `workdir` equals the fresh `work-remount-<n>` path via the existing `dirs.workdir` serialization; restart reap destroys the whole run dir including stale workdirs.
- test name: `squash_output_minimal_contract`
  scenario: B3-shaped run; read result JSON, then call the existing layerstack observe op.
  expected result: result carries only `manifest_version` + `squashed_blocks{squashed_layer_id, replaced_layers, blocked_reasons}`; stack layout, lease counts, and source ids are fully recoverable from `observe()` + `.sources.json` ledgers.
- test name: `faulty_report_without_byte_walk`
  scenario: post-PONR failure with unverified restore, fs-op recording shim on the upperdir.
  expected result: report carries session id, `class:detail`, lease-release errors; zero upperdir traversal; ordinary destroy runs.
- test name: `runner_report_three_booleans_drive_policy`
  scenario: kill/inject failure at each C3 step; classify with only `first_move_succeeded`/`mount_verified`/`restore_verified`.
  expected result: every C5 row reproduced without a phase enum; missing report at/past first-move-success goes faulty.
- test name: `single_admission_path_orders_destroy_and_remount`
  scenario: concurrent destroy and remount on one session with `session_lifecycle_lock` subsumed by the per-session gate.
  expected result: destroy waits for the attempt to resolve, no deadlock, lock order sessions-map < gate < writer-lock is the complete discipline.

---

## Agent B — Round Trips And Lock Scope

Agent B verdict: simplify

Findings:

- [block] C5 row "Active-handle persist failure after a verified switch" + invariant 6 clause "never release the old lease without the new handle durably persisted" + `mount_uncertain:active_persist_failed` (C4 table) + test 12 `persist_failure_keeps_both_leases`: this entire fallback defends a durable write that has no reader. I verified in the repo: `persist_handles` (`crates/sandbox-runtime/workspace/src/lifecycle/persistence.rs`) is write-only — grep finds zero production readers of `manager.json`; the spec's own `lifecycle/recover.rs` (new) is destroy-only ("destroy its run dir, drop the handle") and environment fact 2 says the file "is never used to recover sessions". A stale `lease_id`/`layer_paths`/`workdir` in `manager.json` cannot be dereferenced destructively by anyone. Worse, "keep BOTH leases in memory, released at session destroy" requires a second per-session lease-id slot that the spec's own file plan never declares (`session/state.rs (+6)` lists only the workdir path, "NO remount state enum") — hidden state.
  delete/merge: delete the C5 persist-failure row, the invariant-6 persist-ordering clause, the `mount_uncertain:active_persist_failed` detail, and test 12. On persist failure: log, resume, release old lease, report `migrated`.
  replacement: existing boot reap semantics — run-dir location in the stale handle is unchanged by remount, so `recover.rs` destroys the right dir regardless; leases are in-memory (`crates/sandbox-runtime/layerstack/src/stack/lease/registry.rs`) and die with the daemon.
  why correctness still holds: mount already verified before persist; new chain is leased; boot never trusts handle contents beyond run-dir location; sweep keep-set is the active manifest. Nothing breaks.

- [revise] Phase-4 table "one `manager.json` active-handle rewrite per migrated session — the sweep's only durable write" + C2 "persist active handle ← the sweep's ONLY durable write" + `lifecycle/persistence.rs (+4)` "persist active workdir with the handle": given finding 1, this write is pure freshness polish. m fsync-pairs per sweep for a file nothing reads.
  delete/merge: demote to one best-effort `persist_handles()` call (or drop entirely); delete the "+4 persist active workdir" delta — boot reap removes the run dir wholesale, so the workdir field is never consumed.
  replacement: existing `persist_handles` already fires on create/destroy (`lifecycle/create.rs:114`, `destroy.rs:104`); that cadence is sufficient for the destroy-only boot reader.
  why correctness still holds: nothing breaks — see finding 1.

- [revise] Invariant 5 "the old workdir is deleted with old-lease cleanup" + `session/state.rs (+6)` "MountedWorkspace gains the active workdir path": a new state field and a new deletion step for a kernel-transient dir that the existing run-dir destroy already reclaims. Both old and fresh workdirs live under `<run_dir>/work-remount-<n>`.
  delete/merge: delete the eager old-workdir deletion and the active-workdir state field; name fresh workdirs by nonce (the `next_unique()` pattern already used by `allocate_layer_dirs`, `crates/sandbox-runtime/layerstack/src/storage/fs.rs:30`) so no counter state is needed.
  replacement: run-dir removal at session destroy / boot reap — the existing cleanup path.
  why correctness still holds: post-unmount workdirs are near-empty (tasks were frozen, no in-flight copy-ups); accumulation is ≤1 tiny dir per successful remount, reclaimed at destroy — existing GC eventually reclaims it, so the core rule's leak clause is not triggered.

- [revise] C1 "Command core routes through it; there is no separate lifecycle lock for this feature": the exec path then holds TWO locks — the existing global `session_lifecycle_lock` (`crates/sandbox-runtime/operation/src/command/service/core.rs:28`, taken across resolve→launch→attach in `exec_command.rs:34-111`) nested with the new per-session admission gate. Two serializers with an undocumented ordering is a deadlock surface and duplicate guarding: every pair the global mutex serializes for one session (launch vs destroy/finalize) is also serialized by the gate once exec, destroy, and `finalize_one_shot` all route through it.
  delete/merge: replace `session_lifecycle_lock` with the per-session gate as the single lifecycle serializer (one-shot creation acquires the gate immediately after the session enters the map); or at minimum add the gate to the C1 lock-discipline chain.
  replacement: the spec's own per-session admission gate.
  why correctness still holds: cross-session serialization (the only thing the global mutex adds) protects nothing — one session's launch and another's destroy are independent. Nothing breaks.

- [revise] Phase 2/3 fsync plan: "fsynced bottom-up including every directory and whiteout" (`fs.rs (+30)` fsync_tree extension) plus per-block "fsync `layers/`" plus 3 `write_atomic` sidecars per block (2 fsyncs each). The durability *requirement* is real (a committed manifest referencing an S layer with non-durable whiteout dir entries = deleted files resurface = wrong view), but the *mechanism* is O(E+D)+6b+b fsyncs, which the spec itself names the wall-clock dominator ("~1–10 s per 1000 entries"). Sidecar durability is additionally not correctness-bearing by the spec's own fail-closed rules (missing `.sources.json` ⇒ identity; missing `.digest` ⇒ one dedup miss; `.bytes` ⇒ observability).
  delete/merge: batch — one `syncfs(2)` on the storage-root fd immediately before the manifest rename covers staging trees, promote renames, and sidecars in one syscall; and fsync `layers/` once per commit, not per block. Deletes the `fsync_tree` extension.
  replacement: `syncfs` + the existing `write_atomic` manifest commit (`storage/fs.rs:113`).
  why correctness still holds: syncfs flushes all dirty data and directory entries on the filesystem before the commit point — strictly stronger than the per-entry walk; crash-after-commit still finds every S entry and whiteout durable. Nothing breaks.

- [accept] Vocabulary "plan lease … pins every source for the lock-free build with zero new machinery": the pinning rationale is actually redundant — sources remain in the active manifest until the phase-3 rename, and `release_lease_locked` (`crates/sandbox-runtime/layerstack/src/stack/lease/cleanup.rs:33`) never deletes active-manifest layers, so nothing can reclaim sources mid-build under singleflight. But the plan lease's *release* is the commit GC itself, reusing `release_lease_locked` instead of a new one-off GC function. Keep: deleting it forces reimplementing `unreferenced_layers` + `remove_layers` as a new commit-time pass. The reentrant writer lock (`storage/lock.rs`, `write_depth`) makes the nested release inside the exclusive commit section legal as speced.

- [accept] Per-session shared guard in `acquire_rewritten_lease` and per-session exclusive `release_lease`: another reviewer will call k+m lock acquisitions batchable. Batching the shared side is forbidden by the spec's own lock discipline (never hold the writer lock across quiesce/staged switch) and would block every publish for the whole sweep; batching the m exclusive releases into one end-of-sweep guard is possible (the lock is reentrant, zero new API) but adds an accumulator, delays reclaim, and saves only m−1 in-process lock hops and m−1 manifest re-reads. Keep per-session; the `cleanup.rs (+12)` set-based membership fix is the part that actually matters (current `retained_layers.contains` is a `Vec` scan — verified at `cleanup.rs:33`).

- [accept] Restore ladder (C3 step 7, vocabulary "restore ladder"): a fallback, but deleting it converts benign rollback-unmount EBUSY (e.g. an SCM_RIGHTS-parked fd, e2e test 7) into forced destruction of a healthy session and leaves no clean resolution of the half-switched mount tree — a visible partial remount under the core rule. It reuses the same MS_MOVE/probe primitives already in the runner; keep.

Round trips / fallbacks to delete:
- item: m × `manager.json` active-handle rewrites (phase 4's only durable write) + the persist-failure keep-both-leases fallback.
  reason: no reader exists (verified); the fallback defends nothing and forces undeclared per-session second-lease state.
- item: eager old-workdir deletion at old-lease cleanup + active-workdir state/persist fields.
  reason: run-dir destroy already reclaims it; nonce naming removes the counter.
- item: per-block `fsync layers/` → one per commit; O(E+D) per-entry fsync walk + 6b sidecar fsyncs → one `syncfs` before the manifest rename.
  reason: same durability guarantee, one syscall instead of thousands; spec's own numbers say the walk dominates wall clock.
- item: global `session_lifecycle_lock` in the exec path once the per-session gate exists.
  reason: duplicate serializer, undocumented lock order.
- item: (verified, keep) manager→daemon stays exactly one call — `checkpoint_squash` forwards one `squash_layerstack` via the existing `router/forward.rs`/`daemon_client.rs` path; the per-session remount is an in-process service call inside the squash op, not a dispatched operation. No cut available or needed.

Per-squash totals (b blocks, k live sessions, m migrated, a aborted, i identity; E/D = staging entries/dirs):
- Before cuts — writer lock: (k+1) shared + (m+1) exclusive; leases: (1+k−i) acquires + (1+a+m) releases (+2-lease destroys on persist-fail); durable renames/write_atomics: 4b+1+m; fsyncs: O(E+D) + 7b + 2 + 2m.
- After cuts — writer lock: (k+1) shared + (m+1) exclusive (m batchable to 1, optional); leases: unchanged (all lease traffic is correctness-required by pin-overlap and abort-keeps-old); durable writes: 4b+1; fsyncs: 1 syncfs + 2 (manifest) ≈ 3 total.

Required tests:
- test name: persist_failure_still_migrates
  scenario: verified switch, injected `persist_handles` failure; then daemon kill + restart.
  expected result: session resumes on NEW mount, old lease released, old run GC'd, report `migrated`; boot reap destroys the run dir from the stale handle with nothing leaked — proving the deleted keep-both-leases path was unnecessary.
- test name: stale_manager_json_harmless_at_boot
  scenario: sweep migrates sessions without rewriting `manager.json`; daemon crashes mid-sweep.
  expected result: reap-then-sweep destroys run dirs and keeps exactly the active manifest's layers; stale `lease_id`/`layer_paths` fields are never dereferenced.
- test name: old_workdir_reclaimed_at_destroy
  scenario: two successive live remounts of one session, then session destroy.
  expected result: retired workdirs persist harmlessly under the run dir and vanish with run-dir removal; no eager-deletion code path exists.
- test name: syncfs_commit_durability
  scenario: power-fail simulation after manifest rename with the single-syncfs commit; whiteout-heavy S layer.
  expected result: every S entry, dir, and whiteout durable — equivalence with the deleted bottom-up fsync walk (replaces test 7's fsync-recording shim).
- test name: single_gate_serializes_exec_and_remount
  scenario: concurrent exec launch, finalize hook, destroy, and remount attempt on one session with `session_lifecycle_lock` removed.
  expected result: per-session gate alone serializes all of them; no interleaving with the MS_MOVE pair; lock-order run under load shows no deadlock.

---

## Agent C — File And Module Count

Agent C verdict: simplify

Findings:

- [revise] §A `crates/sandbox-manager` block (`checkpoint_squash.rs` new ~40, `management_operations.rs` +30, `operation/{mod,dispatch,specs}.rs` +10) + §A `operation.rs` `OperationEntry::internal (+8)`: the manager already forwards **any** sandbox-scoped request whose op it does not own — verified at `crates/sandbox-manager/src/router/dispatch.rs:19-21` (`(CliOperationScope::Sandbox, !manager_owned) → forward_sandbox_request`) and `router/forward.rs` (endpoint lookup + `daemon_client.invoke_with_timeout`). The entire manager surface exists only to rename `squash_layerstack` to `checkpoint_squash`, and `OperationEntry::internal` exists only to hide the op from the runtime CLI catalog ("squash_layerstack is a runtime dispatch operation, not a runtime CLI catalog entry"). Hiding is cosmetic, not correctness.
  delete/merge: delete `sandbox-manager/src/operation/management/service/impls/checkpoint_squash.rs`, the `checkpoint_squash` CliOperationSpec (+30), the register lines (+10), and `OperationEntry::internal` (+8); register `squash_layerstack` as an ordinary `OperationEntry::cli` (small layerstack entry group in `operation/src/operation.rs` `CLI_FAMILIES`, which today lists only command/workspace_session/file).
  replacement: existing generic forwarding at `crates/sandbox-manager/src/router/dispatch.rs` + `router/forward.rs`; existing `OperationEntry::cli` constructor (`operation/src/operation.rs:21` — the `cli: Option<…>` field already exists, no second constructor needed). Net: −1 new file, ~−90 LoC, −1 new mechanism; test 17's "runtime catalog does not expose squash_layerstack" clause inverts and shrinks.
  why correctness still holds: nothing breaks — the same one wire round trip (cli → manager → daemon) occurs either way; singleflight and the storage commit are daemon-local and unaffected by which catalog names the op.

- [block] §A `stack/ops/publish.rs (+40)` "extract shared commit tail (promote → sidecars → recheck → manifest write) used by publish and squash": the claimed shared tail does not match publish's proven inline order. Verified at `crates/sandbox-runtime/layerstack/src/stack/ops/publish.rs:71-122`: publish does promote → digest → **equality** recheck (`latest != *active`) → manifest → **best-effort bytes after manifest**; squash phase 3 (§Storage table row 3) needs **run-presence** recheck first, three sidecars durable **before** the manifest rename, a multi-run splice instead of a prepend, and a GC step publish lacks. A "shared tail" spanning four differing axes is a parameterized abstraction with 1.5 users that forces reordering or wrapping a proven durability sequence — concrete regression risk to the publish commit path for zero correctness gain.
  delete/merge: delete the +40 extraction; leave `stack/ops/publish.rs` at 0 LoC changed. Squash's commit composes the existing primitives directly inside `stack/squash.rs`.
  replacement: `storage/fs.rs` — `allocate_layer_dirs`, `write_atomic` (fs.rs:113), `write_manifest`, `write_layer_digest`, `write_layer_bytes`, `remove_path`, `fsync_dir` all verified present; the planned fs.rs +30 (sources sidecar helper) completes the shared layer.
  why correctness still holds: nothing breaks — fs primitives are already the shared vocabulary; two ~50-line commit sequences with different rechecks are simpler and safer than one generic tail.

- [revise] §A `workspace/src/session/state.rs (+6)` "MountedWorkspace gains the active workdir path" + `lifecycle/persistence.rs (+4)` "persist active workdir with the handle": both duplicate state that already exists. Verified: `MountedWorkspace.dirs` is `OverlayDirs { run_dir, upperdir, workdir }` (`session/state.rs:11`, `overlay/dirs.rs:5-9`), and `persist_handles` already writes `"workdir": handle.dirs.workdir` (`lifecycle/persistence.rs:30`).
  delete/merge: delete both line items (−10 LoC, −1 copied concept). The verified switch updates `dirs.workdir` in place to `work-remount-<n>` when it swaps `snapshot`; the existing persist call then records it with no schema change.
  replacement: existing `OverlayDirs.workdir` field + existing `persist_handles`.
  why correctness still holds: nothing breaks — the old workdir path is transaction-local (held by `lifecycle/remount.rs` for old-lease cleanup); invariant 6's "persist active handle" is satisfied by the existing rewrite.

- [revise] §A `stack/sweep.rs (new ~80)` boot storage sweep: this is a third home for "delete layer dirs + sidecars not referenced". Verified `stack/lease/cleanup.rs:25-49` already owns exactly that logic (`unreferenced_layers` + `remove_layers`), and the spec already touches cleanup.rs (+12) to make it delete the full sidecar set (digest/bytes/sources). The boot sweep is `remove_layers` with candidates = disk listing, an empty lease registry, plus a `staging/*` wipe.
  delete/merge: delete `stack/sweep.rs`; put the sweep function in `stack/lease/cleanup.rs` (or `storage/`), sharing `remove_layers` and the one sidecar-set definition.
  replacement: `crates/sandbox-runtime/layerstack/src/stack/lease/cleanup.rs` (`unreferenced_layers`, `remove_layers`).
  why correctness still holds: nothing breaks — fail-closed manifest check and `B*` protection move with the function; having lease-release GC and boot sweep share one deletion routine removes the risk of the two ever disagreeing on the sidecar set.

- [revise] §A `stack/squash/rewrite.rs (new ~70)`: a standalone "pure" file whose only consumer, `acquire_rewritten_lease` (§Vocabulary), has **no file in the plan** — a one-call expand-then-contract + validate + acquire under a shared writer-lock guard cannot fit in the "+25 wiring + exports" line. The plan under-budgets the lease-side half while over-filing the pure half.
  delete/merge: merge rewrite.rs's expand-then-contract together with `acquire_rewritten_lease` into one module under `stack/lease/` (beside `registry.rs:87` `lease_newest_layers`, whose lock it shares), leaving `squash/` with only `flatten.rs`.
  replacement: `stack/lease/` module + `LeaseRegistry` (`crates/sandbox-runtime/layerstack/src/stack/lease/registry.rs`).
  why correctness still holds: nothing breaks — rewrite is a lease operation (ledger → replacement lease manifest), not a commit-path operation; one module means one hop from ledger to lease and no orphan pure file.

- [revise] §A `workspace/src/lifecycle/recover.rs (new ~70)`: recover's whole job is read-then-delete of `manager.json` plus run-dir removal, and `lifecycle/persistence.rs` (85 LoC) is the sole owner of that file's path and schema (`persisted_handles_path`, `PERSISTED_HANDLES_SCHEMA_VERSION`).
  delete/merge: merge recover into `lifecycle/persistence.rs` (~+50 there instead of a new file); keep the single boot-hook call site in `operation/src/services.rs` (+10) as planned.
  replacement: `crates/sandbox-runtime/workspace/src/lifecycle/persistence.rs`.
  why correctness still holds: nothing breaks — reap semantics ("every persisted handle is a dead session") are unchanged; the manager.json format stays fenced in one file instead of exporting parse helpers across two.

- [accept] §A workspace/namespace remount chain (`remount_session.rs` ~60 → `remount_workspace.rs` ~40 → `lifecycle/remount.rs` ~150; `engine.remount_overlay` +30 → `NamespaceRuntime::remount_overlay` +35 → `runner/setns/remount_overlay.rs` ~260): I verified this mirrors the existing chains hop-for-hop — destroy is `operation/workspace_session/service/impls/destroy_session.rs` (38) → `workspace/service/impls/destroy_workspace.rs` (61) → `lifecycle/destroy.rs` (149), and mount is `workspace/namespace/setns_runner.rs:25` `mount_overlay` → `engine.rs:134` `mount_overlay` → `runner/setns/mount_overlay.rs` (38); workspace impls really are 26–64 LoC as the spec claims. No hop can be deleted: the runner launcher is private to `NamespaceExecutionEngine` (`launcher: Box<dyn NsRunnerLauncher>`, engine.rs:25), so workspace cannot spawn the setns runner directly without breaking the README boundary (workspace must not own command/runner process state), and `remount_session` must be a `WorkspaceSessionService` method because the per-session gate and sessions map are private to that service. Deleting a hop breaks the pin-overlap/ordering transaction ownership (invariant 6) or the crate boundary — keep.

- [accept] §A `quiesce.rs (~240)` in namespace-execution: the crate already owns live command-process state (`ExecutionRegistry`, `NamespaceExecution::pgid()` at `execution.rs`), which is quiesce's discovery seed; README assigns it "namespace execution engine" and forbids only "own workspace lifecycle", which quiesce does not touch. Moving it to namespace-process would put daemon-side `/proc` scanning in the crate that owns in-namespace runner bodies — worse. Keep.

Round trips / fallbacks to delete:
- item: manager-owned `checkpoint_squash` dispatch indirection (manager catalog entry + impl + register) in front of the daemon-local `squash_layerstack`.
  reason: `router/dispatch.rs` already generically forwards sandbox-scoped non-manager ops; the manager layer adds a name alias, not a round trip saved — same one cli→manager→daemon hop either way, minus ~90 LoC and one new mechanism (`OperationEntry::internal`).
- item: none other found in my lane — the sweep loop's per-session sequence (gate → rewritten lease → freeze → one runner → persist → resume → release) already has no redundant daemon/runner round trips; the runner is one spawn per migrated session, matching `mount_overlay`.

Required tests:
- test name: sandbox_scoped_squash_forwards_generically
  scenario: build `Request { op: "squash_layerstack", scope: Sandbox { sandbox_id } }`; assert `manager_owns_operation` is false and `dispatch_request` routes through `forward_sandbox_request`; assert the op appears exactly once in the runtime CLI catalog and nowhere in the manager catalog (replaces spec test 17).
  expected result: forwarding works with zero manager-side squash code.
- test name: remount_updates_existing_workdir_field
  scenario: verified staged switch on a live session; read the persisted `manager.json` handle.
  expected result: `"workdir"` equals the fresh `work-remount-<n>` path via the existing `handle.dirs.workdir` field and existing `persist_handles`; no new field, no schema-version change.
- test name: boot_sweep_and_lease_release_share_deletion_set
  scenario: create a layer with digest/bytes/sources sidecars; delete it once via `release_lease` GC and once via the boot sweep (fresh process, orphan on disk).
  expected result: both paths remove the identical file set through the single `cleanup.rs` routine; no path leaks a `.sources.json` or `.bytes`.
- test name: publish_path_untouched_by_squash
  scenario: land the squash feature; run the full existing publish test suite and assert `stack/ops/publish.rs` has zero diff (no shared-tail extraction).
  expected result: publish behavior byte-identical; squash commit tests pass using only `storage/fs.rs` primitives.

Net effect if all findings land: 12 new files → 8 (`checkpoint_squash.rs`, `sweep.rs`, `recover.rs`, standalone `rewrite.rs` gone), ~−250 source LoC, zero changes to the proven publish commit path, one deletion routine, one persistence schema, no new operation-entry mechanism — with no change to the storage-commit correctness boundary, crash recovery, or the remount protocol.

---

## Agent D — Storage, Metadata, Ledger, GC

Agent D verdict: simplify

Findings:
- [revise] Vocabulary `LayerSubstitution` / storage layout `.layer-metadata/<S-id>.sources.json` / phase-3 "sidecars": the persisted ledger is durable state whose only consumer is the remount rewrite ("substitution ledger (remount input)", spec line 332), which the spec itself declares outside the correctness boundary and covered by environment fact 3 "No remount state is ever persisted." Every reader of the ledger runs in the same daemon process that wrote it, and no lease survives a restart (fact 2), so nothing ever needs the ledger after a restart — the boot sweep reclaims by manifest keep-set, not by ledger.
  delete/merge: delete the `<S-id>.sources.json` sidecar, its `schema_version` field, the missing/unknown-version/degenerate fail-closed triad (spec line 128), the build-time inner-S L-expansion ("inner Sa expanded at build time", line 483), the sources helpers in the `src/storage/fs.rs (+30)` item, the `.sources.json` part of `lease/cleanup.rs (+12)`, the sources case in boot-sweep metadata cleanup (line 366), and the ledger-corruption half of required test 8.
  replacement: an in-process map `{S-id → replaced run}` living beside the existing per-root in-memory lease state (`shared_registry_for_root`, crates/sandbox-runtime/layerstack/src/stack/lease/registry.rs:21 — exactly the precedent: per-root, process-lifetime, dies with the daemon like the sessions it serves). Rewrite becomes oldest-generation-first contraction of raw runs; it produces the same chains as expand-then-contract (B4 ws-3: apply Sa/Sb, then Sc=[L8,Sa]) because every intermediate entry stays in memory — the "sidecars die with the layer dir" liveness reasoning (line 136) disappears entirely.
  why correctness still holds: nothing breaks. Wrong view is impossible — `acquire_rewritten_lease` still validates rewritten layers alive before acquiring; a missing map entry degrades to identity, which the spec already accepts as the universal fallback. Crash recovery is unchanged: after restart there are no leases to rewrite and boot sweep never reads ledgers even in the current spec. Worst case is reclaim latency for a session whose substitution predates the map — unreachable, since the map and the session share the same process lifetime. If the ledger is nevertheless kept durable, at minimum delete `schema_version` and collapse the three failure cases to one rule ("any ledger failing to parse/validate as `{sources: [≥2 ids]}` ⇒ identity"); a future format change renames the sidecar and old runtimes see "missing ⇒ identity" for free.

- [revise] Storage layout / phase 3 "write digest/bytes/sources via write_atomic": squash does not need to write `.digest` or `.bytes` for S layers at all — they are publish-path baggage. Verified consumers: `.digest` is read only by publish dedup `head_layer_digest` (crates/sandbox-runtime/layerstack/src/stack/ops/publish.rs:129–139, tolerates NotFound → publish proceeds correctly) and by base-only binding validation (workspace_base/binding.rs:74–89 — checks `WORKSPACE_BASE_LAYER_ID` only, and B is never squashed). `.bytes` is read only by sandbox-observability/src/collect/layerstack.rs:70–76, which on a missing sidecar walks the layer dir and regenerates the sidecar itself.
  delete/merge: delete the digest and bytes writes from phase 3 (spec line 339) and the flatten rule "`.bytes` counts logical bytes" (line 126); shrink required test 6's "all three sidecars" accordingly. Combined with the previous finding, S layers get zero sidecars.
  replacement: existing observability lazy sizing (collect/layerstack.rs `layer_bytes` walk-and-cache); existing publish dedup simply misses on an S head, which is correct — a flatten digest would never match a publish changeset digest anyway.
  why correctness still holds: nothing breaks — a missing head digest makes publish create a layer it might otherwise dedup (behavior-preserving, and unreachable in practice), and byte accounting self-heals on first observability view.

- [revise] Phase-3 "commit-time GC" / vocabulary "plan lease": the spec must pin the commit-time GC to be literally the existing release path, not a parallel deletion pass inside `src/stack/squash.rs (~280)`. The mechanism already exists: releasing the plan lease through `release_lease_locked` (crates/sandbox-runtime/layerstack/src/stack/lease/cleanup.rs:11–23) re-reads the just-committed manifest and deletes exactly the replaced sources no other lease pins. The only delta needed is returning the removed set (today it returns `bool`) so `replaced_layers: reclaimed|leased` can be derived.
  delete/merge: merge "release plan lease (refcount GC)" and "GC deletes sources no lease references" (line 339) into one named call: `release_lease` on the plan lease; forbid any second deletion routine in squash.rs.
  replacement: crates/sandbox-runtime/layerstack/src/stack/lease/cleanup.rs `release_lease_locked` + `unreferenced_layers`.
  why correctness still holds: identical semantics by construction; reclaimed-vs-leased is decided by the registry at that instant, which is exactly what the spec's policy block demands.

- [accept] Three reclaim paths (commit GC, per-session release GC, boot sweep): none is deletable, because they are really only two mechanisms. Commit GC and old-lease release are the same `release_lease` refcount path; deleting it defers all reclaim to reboot — and worse, `unreferenced_layers` (cleanup.rs:25–36) only ever evaluates the released lease's own layers, so ex-manifest sources pinned by nothing would never become candidates again: an unbounded runtime leak that defeats the feature. Boot sweep is independently required: crash orphans (staging trees, promoted-but-uncommitted S dirs) and old sources pinned only by leases that died with the daemon are invisible to the release path forever — unbounded across crashes.
  delete/merge: nothing.
  replacement: n/a.
  why correctness still holds: deleting either violates the core rule (unbounded leak existing GC cannot reclaim).

- [revise] Boot cleanup rule 1, "deletion never crosses a mount boundary": decoration. The sweep's scope is exactly `staging/*`, `layers/*`, `.layer-metadata/*`; the only real mount inside a stack root is the shared base volume (sandbox-provider-docker/src/runtime.rs — mounted at `/eos/layer-stack/base`, outside all three swept dirs, forced `:ro` at runtime.rs:262/314), which is additionally protected by the fail-closed parse guard, by keep-set (binding validation requires the base in the manifest — binding.rs:92–97), and by the B*-never-deleted rule. A per-entry device-boundary detector guards a case already triple-covered and kernel-enforced read-only.
  delete/merge: delete "deletion never crosses a mount boundary" from boot cleanup rule 1 and its slice of `src/stack/sweep.rs`.
  replacement: keep-set = parsed active manifest + `B*` prefix guard (one predicate) + the `:ro` mount enforcement already in sandbox-provider-docker.
  why correctness still holds: nothing breaks — to delete a needed layer the sweep would have to pass the parse guard, miss the keep-set, miss the B* check, and defeat a read-only mount simultaneously.

- [accept] Boot cleanup rule 1 fail-closed (parsed manifest, `version ≥ 1`, non-empty layers): load-bearing, keep. `read_manifest` silently returns an empty v0 manifest when `manifest.json` is missing (crates/sandbox-runtime/layerstack/src/storage/fs.rs:170–172); without this guard a transient EIO or missing file yields keep-set = ∅ and the sweep deletes every layer — permanent data loss from a recoverable state.
  delete/merge: nothing.
  replacement: n/a.
  why correctness still holds: deleting it can strand/corrupt state (core rule: crash-recovery ambiguity → data loss).

- [accept] `lease/cleanup.rs (+12)` sidecar removal: the claimed leak is real and this is the right fix in the right place. Verified: `remove_layers` (cleanup.rs:38–48) deletes the layer dir and `.digest` only, while `.bytes` is written by publish (ops/publish.rs:118) and regenerated by observability — every lease-GC'd layer orphans a `.bytes` file today, and observability's self-healing cache keeps recreating them for live layers. Same-id sidecar removal on layer delete is the flat, existing-cleanup-path answer.
  delete/merge: nothing (with the ledger finding, the `+12` shrinks to `.digest` + `.bytes`).
  replacement: n/a.
  why correctness still holds: keeping it fixes an existing unbounded metadata leak; deleting it re-opens one.

- [revise] Section A `src/stack/ops/publish.rs (+40)` "extract shared commit tail (promote → sidecars → recheck → manifest write)": the shared abstraction doesn't fit its two callers and the spec contradicts itself about it — phase 3 (line 339) orders squash as "recheck → promote → sidecars → manifest rename", while the extraction item orders the shared tail "promote → sidecars → recheck → manifest write" (today's publish order, ops/publish.rs:82–117). Forcing one helper over two different orderings adds parametrized indirection to a working, subtle commit path to save ~20 straight-line lines, and touches publish under the parallel-workers rule for no correctness gain.
  delete/merge: delete the shared-tail extraction; let squash.rs own its own ~25-line commit sequence (recheck-first, which is the right order for squash since promotes are the expensive step).
  replacement: existing publish commit stays untouched; squash duplicates the tiny `rename → fsync_dir → write_manifest` sequence using the same fs.rs primitives (`write_atomic`, `fsync_dir`) both paths already share.
  why correctness still holds: nothing breaks — the shared primitives, not a shared orchestration function, are what carry the durability guarantees.

Round trips / fallbacks to delete:
- item: durable write + read-back round trip of `<S-id>.sources.json` (phase-3 sidecar write, rewrite-time load, lease-cleanup delete, boot-sweep membership)
  reason: sole consumer is best-effort remount in the same process lifetime; an in-memory per-root map (LeaseRegistry precedent) carries the same fact with zero persistence, zero corruption cases, zero cleanup.
- item: `.digest` and `.bytes` writes for S layers in phase 3
  reason: no reader needs them — publish dedup tolerates absence, binding validation is base-only, observability regenerates `.bytes` by walking.
- item: mount-boundary detection in the boot sweep
  reason: swept dirs contain no mounts; the shared base volume is outside sweep scope, read-only, in the keep-set, and B*-guarded.
- item: missing / unknown-version / degenerate ledger fallback triad
  reason: with an in-memory map none is reachable; if the sidecar survives review, collapse to a single "fails parse/validation ⇒ identity" rule and drop `schema_version`.

Required tests:
- test name: squash_commits_with_no_s_layer_sidecars
  scenario: squash to `[S1, B]`; then publish a new layer on top of S1; then request the observability layerstack view.
  expected result: commit succeeds with no `.digest`/`.bytes` for S1; publish proceeds (dedup miss is silent); observability sizes S1 by walking and self-heals its own cache — proving the deleted sidecar writes were unnecessary.
- test name: in_memory_substitutions_match_expand_then_contract
  scenario: replay B4 (gen-1 Sa/Sb, gen-2 Sc re-squash, ws-2/ws-3 rewrites) using the in-memory map with oldest-generation-first contraction; then restart the daemon and run squash again.
  expected result: rewritten chains identical to the spec's expand-then-contract results; post-restart no rewrite is ever attempted (no sessions exist) and boot sweep reclaims by keep-set alone — proving the durable ledger is unnecessary.
- test name: commit_gc_is_plan_lease_release
  scenario: idle-stack squash (B1) instrumented so the only deletion path invoked is `release_lease_locked` on the plan lease; repeat with a session lease acquired between plan and commit.
  expected result: idle case deletes sources at commit; raced case reports `leased` with sources intact — no second deletion routine exists in squash.rs.
- test name: boot_sweep_safe_without_mount_boundary_check
  scenario: stack root with the shared base volume mounted read-only, valid manifest, plus orphan staging and orphan S dirs; run reap-then-sweep with the mount-boundary rule removed; repeat with an unreadable manifest.
  expected result: orphans deleted, base and manifest layers untouched; unreadable manifest deletes nothing — proving fail-closed + keep-set + B* suffice.
- test name: bytes_sidecar_removed_on_lease_gc
  scenario: publish a layer, lease it, drop it from the active manifest, release the lease.
  expected result: layer dir, `.digest`, and `.bytes` all gone (regression test for today's verified `.bytes` leak at cleanup.rs:38–48).

---

## Agent E — Remount Workflow Minimalism

Agent E verdict: simplify

(Verified prior deletions are real: no quarantine (§Goal fact 3, C5), no durable remount state ("NO remount state enum", state.rs +6 note), no plan-time pinning-set deletes (Phase 4 table). Attacks below are only on what remains.)

Findings:

- [revise] C3 preconditions + A (`overlay/kernel_mount.rs +70`) + D "Chain-length limits": real-path lowerdir mode and the exact-lowerdir mountinfo probe are built on a false premise and protect against nothing.
  delete/merge: delete "real-path lowerdir mode", the "exact lowerdir= mountinfo probe" (C3 step 3 second half, C4 "post-switch: exact lowerdir= list proof"), the entire D-section "Staged remount path … ≈ 97 lowerdirs … collapses near k ≈ 50" paragraph, `stage_failed:lowerdir_limit` as a probed condition, and the staged half of e2e test 9. Keep only `move_mountpoint` and `strict_unmount` in kernel_mount.rs (+70 → ~+35).
  replacement: the existing production builder `mount_overlay` in `crates/sandbox-runtime/overlay/src/kernel_mount.rs` (fsopen/fsconfig `lowerdir+` per layer, fd-backed via `fd_path`, `userxattr` set at line 132). The one-page option-string limit is a legacy-`mount(2)` artifact; fsconfig takes one string per `lowerdir+` call, so there is no silent truncation for a mountinfo probe to catch — the only cap is `OVL_MAX_STACK` (500), and an over-limit chain fails the staging mount itself as a clean pre-PONR `stage_failed`. Option parity "by construction" holds identically for the same builder in fd mode; the removed experiment's defect was a *separate* helper, not fd paths.
  why correctness still holds: the staged NEW mount is built in-process by the validated builder from the rewritten lease's paths; layer dirs are immutable and pinned by the replacement lease, so fsconfig either mounts exactly those lowerdirs or errors. The kept read probe still gates PONR. Nothing breaks — and the fake ~97-lowerdir remount ceiling disappears.

- [revise] C3 step 7 / vocabulary "restore ladder": the 4-step ladder (move new→staging, rollback→root, re-probe OLD, unmount staging) only saves a session from destruction; per the core rule that is polish, and the ladder adds its own faulty-generating path (B4 ws-4 is destroyed *because* the ladder's re-probe failed).
  delete/merge: delete the restore ladder and the `verified restore ⇒ leased(pinned:rollback_unmount_busy)` outcome. Post-PONR rule collapses to two lines: (a) rollback-unmount EBUSY after a *verified* switch ⇒ resume on NEW, keep BOTH leases in memory, release at session destroy, report `leased(pinned:rollback_unmount_busy)` — this is byte-for-byte the shape of C5's existing "active-handle persist failure" rule; (b) any other post-PONR failure ⇒ faulty report + ordinary destroy, tasks still frozen so nothing observes the partial state.
  replacement: C5 row "Active-handle persist failure … keep BOTH leases in memory, released at session destroy" — the pattern already exists in this spec; invariant 4 explicitly accepts namespace death as the detach proof; `release_lease` at destroy is existing (`layerstack/src/stack/mod.rs:95`).
  why correctness still holds: at EBUSY time steps 2–6 succeeded — the NEW mount is verified visible; no partial switch is observable. Old lease held ⇒ no layer released while the old superblock lives; namespace death at destroy releases both. The rollback mountpoint sits under masked scratch, unreachable after remask. Bonus: the spec's own e2e test 7 scenario (SCM_RIGHTS-parked fd) currently repeats freeze→switch→EBUSY→ladder on *every* squash run forever; under (a) it converges once and the next run is Identity. If long-lived same-upperdir coexistence is judged outside the kernel gate's proof, fall back to (b) for EBUSY too — still strictly less machinery than the ladder, and destroying an ephemeral session is none of the five core-rule failures.

- [revise] C4 table: per-task `mountinfo` reads are redundant with the per-task `ns/mnt` check.
  delete/merge: drop `mountinfo` from the per-`tid` inspection loop; read the holder's mountinfo once per session for the child-mount-under-workspace check.
  replacement: the C4 `ns/mnt` row already blocks any discovered task whose mount table could differ from the holder's (`pinned:mount_namespace_escaped`); all remaining tasks share one mount table, so N reads of identical content prove nothing the holder's one read doesn't.
  why correctness still holds: nothing breaks — child mounts and the workspace overlay are properties of the (single) holder mount namespace, not of tasks.

- [revise] A (workspace crate): `session/state.rs (+6) MountedWorkspace gains the active workdir path` and `lifecycle/persistence.rs (+4) persist active workdir with the handle` duplicate fields that already exist.
  delete/merge: delete both line items.
  replacement: `MountedWorkspace.dirs: OverlayDirs` already carries `workdir` (`crates/sandbox-runtime/workspace/src/session/state.rs:11`), and `persist_handles` already serializes `handle.dirs.workdir` (`crates/sandbox-runtime/workspace/src/lifecycle/persistence.rs:30`). Remount mutates the existing `dirs.workdir` to the fresh sibling path; the existing persist call writes it.
  why correctness still holds: nothing breaks — boot reap destroys run dirs wholesale (workdirs live under `run_dir`), so no boot consumer reads the persisted workdir anyway.

- [revise] C3 steps 1/6 (`RemountMaskGuard`): the unmask is required only for *building* the staged mount (upperdir/fresh-workdir must be kernel-resolvable real paths under masked roots — `hidden_paths: [/eos, /tmp/eos]` in `sandbox-config`, and `kernel_mount.rs` documents that fd-backed upper/work paths are rejected). The MS_MOVE pair does not need masks off.
  delete/merge: narrow the guard to step 2 only — unmask → build staged NEW → remask → probe → moves; do the moves via O_PATH dirfds + `move_mount(…, MOVE_MOUNT_F_EMPTY_PATH)` opened before remask (the pattern kernel_mount.rs:144–150 already uses). Delete "restore masks" from step 6.
  replacement: existing fd-based `move_mount` usage in `overlay/src/kernel_mount.rs`.
  why correctness still holds: nothing breaks — masks matter only for path resolution, and tasks are frozen throughout either way. Gain: remask failure moves from the post-PONR faulty surface to a clean pre-PONR skip, shrinking the only genuinely dangerous window.

- [accept] Vocabulary "quiesce" (cgroup ∪ /proc ns-scan ∪ allowlist): keep both discovery legs — each prevents a distinct core-rule failure.
  delete/merge: nothing; but the spec should note *why* neither leg alone suffices.
  replacement: n/a.
  why correctness still holds (why deletion breaks): the ns-scan is the freeze-set proof — cgroup placement is best-effort and silently ignored on failure (`namespace-execution/src/launcher.rs:344–348`, and `cgroup_procs_path`/`cgroup_root` are `Option`), so cgroup-only discovery can miss an entire command tree ⇒ unfrozen task ⇒ visible partial switch. The cgroup union is the only detector of mnt-ns-escaped tasks — an escaped namespace's copied vfsmount does NOT make the holder's rollback unmount EBUSY, so step 7 does not subsume escape detection ⇒ deleting it risks releasing layers an escaped namespace still reads (needed layer released). One optional trim: "still pin-inspected" for allowlisted infra can go — infra is daemon-owned code and any infra pin surfaces as rollback-unmount EBUSY, which the simplified post-PONR rule absorbs without destroying anything.

- [accept] C1/B5: Identity short-circuit vs "deliberately NO registry short-circuit" is not an inconsistency; keep the uniform freeze→inspect pipeline.
  delete/merge: nothing.
  replacement: n/a.
  why correctness still holds: Identity is computed from durable data already in hand (lease manifest + ledgers via `acquire_rewritten_lease`) — skipping is the absence of work, not a classification. A "predictably pinned" skip would predict volatile process state from a second source (even though command state exists in the registry, it can go stale between check and freeze), i.e., a new classification path to save a ~50 ms SIGSTOP paid only on explicit `checkpoint_squash`. Deleting the uniform path adds complexity; keeping it adds none.

- [accept] C3 step 8 / vocabulary "point of no return": the phase-tagged report with `first_move_succeeded` on all paths and missing-report⇒faulty is already minimal.
  delete/merge: nothing (though under the restore-ladder deletion the report's vocabulary shrinks: no "restore verified" state remains — only pre-PONR abort class, verified switch + rollback-unmounted bool, faulty).
  replacement: n/a.
  why correctness still holds: one bit + phase is the minimum that disambiguates clean-skip from faulty when the runner dies mid-switch; deleting it creates crash-recovery ambiguity (can't tell whether the first move happened ⇒ could resume tasks onto a half-switched view).

Round trips / fallbacks to delete:
- item: restore ladder (2 extra MS_MOVEs + OLD re-probe + staging unmount) in C3 step 7 / C5 row 3
  reason: session-saving only; replaced by the existing both-leases pattern (EBUSY) or straight faulty-destroy; also deletes the per-run futile re-switch loop for parked-fd sessions.
- item: exact-lowerdir mountinfo probe + real-path builder mode
  reason: no silent truncation exists under fsconfig `lowerdir+`; the same-builder read probe and mount success already gate PONR.
- item: per-task mountinfo read in the C4 loop
  reason: `ns/mnt` equality makes all task mount tables identical; one holder read suffices.
- item: post-PONR "restore masks" step
  reason: remask before the moves (dirfd-based `move_mount`); mask failure becomes a clean pre-PONR skip.
- item: `stage_failed:lowerdir_limit` as a probed/detected condition + D's staged ≈97-lowerdir analysis
  reason: the staging mount syscall itself fails cleanly at `OVL_MAX_STACK`; derive the reason string from the mount errno.
- item: pin-inspection of allowlisted infrastructure (holder, pid-ns init, runner)
  reason: daemon-owned processes; any missed infra pin surfaces as rollback-unmount EBUSY, absorbed by the simplified post-PONR rule.

Required tests:
- test name: both_leases_on_rollback_unmount_busy_converges_next_run
  scenario: SCM_RIGHTS-parked workspace fd; verified switch; strict `umount2(rollback, 0)` returns EBUSY; session resumes; next `checkpoint_squash` runs.
  expected result: session on the NEW compact chain, both leases held and released at destroy, old layers deleted only after namespace death; second run reports Identity — no repeated freeze/switch; proves the 4-step ladder is unnecessary.
- test name: staged_mount_over_ovl_max_stack_is_clean_skip
  scenario: rewritten chain exceeds the kernel lowerdir cap; staged mount attempted via the production fsconfig builder.
  expected result: mount fails pre-PONR, `leased(stage_failed:…)` derived from the mount error, old lease intact — proves no separate lowerdir-limit probe/detector is needed.
- test name: single_holder_mountinfo_read_blocks_child_mounts
  scenario: N frozen tasks all with `ns/mnt == holder`; one `unshare -m` escapee; one child mount under the workspace root.
  expected result: escapee blocked by the ns/mnt check before any mountinfo parse; child mount blocked from ONE holder mountinfo read; zero per-task mountinfo reads issued.
- test name: remask_before_moves_removes_postponr_mask_failure
  scenario: remask forced to fail after the staged build; moves performed via pre-opened dirfds.
  expected result: clean pre-PONR skip (`leased`), no faulty outcome possible from mask restore; masked paths never observable by any resumable task.
- test name: remount_persists_workdir_via_existing_handle_schema
  scenario: successful live migration with fresh sibling workdir.
  expected result: `manager.json` shows the new workdir through the existing `persist_handles` field (`dirs.workdir`), with no schema addition; boot reap after a crash deletes it with the run dir.

---

## Agent F — CLI And Product Surface

Agent F verdict: simplify

Findings:
- [revise] §A `crates/sandbox-manager` block + "CLI surface": the entire manager wrapper — `impls/checkpoint_squash.rs` (~40), `management_operations.rs` `checkpoint_squash CliOperationSpec` + family `"checkpoint"` (+30), `operation/{mod,dispatch,specs}.rs` register (+10), `manager_core.rs` tests (+50) — plus `OperationEntry::internal` (+8 in `operation.rs`) exists only to rename one daemon op and hide it from the runtime catalog.
  delete/merge: delete the manager `checkpoint_squash` operation, the `"checkpoint"` family, and `OperationEntry::internal`; register `squash_layerstack` as an ordinary runtime CLI catalog entry via the existing `OperationEntry::cli` constructor (`crates/sandbox-runtime/operation/src/operation.rs:21`) in a small layerstack cli_definition module.
  replacement: the manager router already forwards **any** sandbox-scoped, non-manager-owned request generically — `crates/sandbox-manager/src/router/dispatch.rs:19-21` → `router/forward.rs` (endpoint resolution, Ready-state check, timeout all exist). `sandbox-cli runtime squash_layerstack --sandbox-id ID` works end to end today via `resolve_runtime_sandbox_id` (`sandbox-gateway/src/cli/request_builder.rs:138`, including config-default sandbox id). Note the spec's model is wrong: `destroy_sandbox.rs` is a manager-**local** lifecycle op (store transition + `stop_daemon` + `runtime.destroy_sandbox`), it forwards nothing — there is no per-op forwarding impl to copy because forwarding is already generic. `"checkpoint"` appears nowhere in the codebase; it is a one-member family invented for future extension. Hiding the op from the runtime catalog is not a privilege boundary — the daemon dispatches by name regardless (`operation.rs:59-70`).
  why correctness still holds: nothing breaks — no invariant in the spec depends on which catalog carries the name; singleflight, commit, and sweep are daemon-side. ~140 LoC and two concepts (name pair, family) deleted.
- [block] §"With `--progress`" + §C5 "Faulty reporting is not optional": the mandated faulty report has no field to live in, and the progress surface it leans on does not exist. Verified: zero `progress` support in `sandbox-protocol` and `sandbox-daemon`; `ProgressSink` is manager-local and wired only for `create_sandbox` (`sandbox-manager/src/operation/dispatch.rs:35-41`); §A budgets `sandbox-protocol / sandbox-daemon / sandbox-gateway (+0)`, so daemon-side remount telemetry streaming is unimplementable inside the spec's own file plan. Worse: a faulty session's destroy releases its leases, so its block can legitimately report `"reclaimed"` — the destroyed session vanishes from the JSON entirely.
  delete/merge: delete the `--progress` example and the "after manager-to-daemon progress forwarding is wired" dependency; add one top-level `faulty_sessions: [{session_id, phase, upperdir_bytes, lease_errors}]` array (omitted when empty) to the stdout result.
  replacement: the existing one-line stdout result contract; observability records for the detail trail.
  why correctness still holds: destroying a live session and discarding upperdir bytes is user-visible data loss; the result line is the only surface guaranteed to exist, so this is the minimal fix that makes the spec's own "must not be observability-only" rule satisfiable. Everything else about progress is reporting-only.
- [revise] §"Output contract" `layers` field: the full stack dump duplicates the existing daemon view `sandbox-cli observability layerstack --sandbox-id ID` — "Show the active manifest as a per-layer inventory: disk bytes, how many workspaces lease each layer" (`crates/sandbox-observability-operations/src/cli_definition/layerstack.rs:5-12`).
  delete/merge: delete `layers`; keep `manifest_version` + `squashed_blocks`.
  replacement: `observability layerstack` view.
  why correctness still holds: nothing breaks — reporting convenience only.
- [revise] §"Output contract" `leases` count: duplicates both `replaced_layers:"leased"` (the binary fact) and the per-layer lease counts already served by the same observability view; the count is stale the instant it is printed (sessions exit at any time post-sweep).
  delete/merge: delete the `leases` field from `squashed_blocks`.
  replacement: `replaced_layers:"leased"` + `observability layerstack` lease counts.
  why correctness still holds: nothing breaks — reporting only.
- [revise] §"Output contract" / §C4 `blocked_reasons` with five contract-stable class prefixes (`unsupported | quiesce_failed | pinned | mount_uncertain | stage_failed`): a committed, test-pinned (test 16, e2e 6) error taxonomy for a day-one feature whose control flow is binary (migrated/leased/faulty). The classes exist internally for C5 decisions; the *stability promise* is the surface.
  delete/merge: keep `blocked_reasons` strings but declare them free-form diagnostics; contract only "non-empty when `leased`".
  replacement: the same C4/C5-produced strings, minus the stability commitment.
  why correctness still holds: nothing breaks — no caller behavior in the spec branches on class prefixes; they exist for humans.
- [revise] §"Storage commit faults" `manifest_conflict` error kind with `planned_version/found_version` details: the spec itself calls it defensive, and I verified the only other manifest writer, `amend`, also only prepends (publishes a new layer — `operation/src/layerstack/service/impls/amend.rs` doc: "runs the caller's transform against the active head … and publishes the resulting layer"), so under singleflight no existing writer can break run contiguity. An unreachable error kind should not be contract vocabulary — the protocol's kind set is six entries today (`sandbox-protocol/src/error_kind.rs`).
  delete/merge: delete `manifest_conflict` and its details schema from the contract; keep the internal commit recheck, surfacing any (future) violation as the existing `operation_failed`.
  replacement: `error_kind::OPERATION_FAILED` (`crates/sandbox-protocol/src/error_kind.rs:4`).
  why correctness still holds: the recheck guard remains and still aborts; only the wire vocabulary shrinks. Nothing breaks.
- [accept] §A `sandbox-observability/src/record.rs` (+8), three record constants: matches the existing house grammar exactly — per-subsystem spans already exist (`NAMESPACE_EXEC_MOUNT_OVERLAY`, `WORKSPACE_SESSION_CREATE`, `LAYERSTACK_PUBLISH`, `record.rs:55-71`), one const each, spans nest. Collapsing to one record would break the established subsystem-span convention to save two consts. Keep.
- [accept] Zero options on the command: verified nothing quietly requires one — `--progress` is a global CLI flag (`sandbox-gateway/src/cli/output.rs:38`), sandbox-id resolution (incl. config default) is existing machinery, and there is no trigger policy. Keep zero options.

Round trips / fallbacks to delete:
- item: manager-side `checkpoint_squash` dispatch layer (CLI → manager op impl → daemon) → replace with the existing generic router forward (CLI → `forward_sandbox_request` → daemon).
  reason: same single network round trip, minus one bespoke dispatch layer, one name translation, one new family, and ~140 LoC; forwarding, endpoint, Ready-check, and timeout already exist in `router/forward.rs`.
- item: manager-to-daemon `--progress` streaming for squash/remount telemetry.
  reason: requires new protocol/daemon/gateway infrastructure the spec budgets at +0; pure reporting; result JSON + observability records already cover both machine and human consumers.

Required tests:
- test name: squash_layerstack_runtime_cli_forwards_via_generic_router (replaces test 17)
  scenario: runtime catalog exposes `squash_layerstack`; invoke via `sandbox-cli runtime squash_layerstack --sandbox-id ID`; assert no manager operation named `checkpoint_squash` exists and the request reaches the daemon through the existing sandbox-scope forward path.
  expected result: one JSON result line; proves the manager wrapper and `OperationEntry::internal` were unnecessary.
- test name: faulty_session_appears_in_result_json
  scenario: post-PONR failure with unverified restore and non-empty upperdir.
  expected result: stdout JSON contains `faulty_sessions` with session id, phase, upperdir bytes, lease errors; exit code 0 (commit succeeded); no dependency on progress or observability to learn of the destroy.
- test name: squash_result_minimal_contract (replaces test 16)
  scenario: B3-shaped run.
  expected result: result carries exactly `manifest_version` + `squashed_blocks` (+ `faulty_sessions` when non-empty); no `layers`, no `leases`; `observability layerstack` still serves the stack dump and per-layer lease counts — proving the deleted fields were duplicates.
- test name: commit_recheck_conflict_surfaces_operation_failed
  scenario: artificially corrupt the planned run between plan and commit (test-only injection).
  expected result: commit aborts with existing `operation_failed` kind; no `manifest_conflict` kind anywhere in the wire contract.
