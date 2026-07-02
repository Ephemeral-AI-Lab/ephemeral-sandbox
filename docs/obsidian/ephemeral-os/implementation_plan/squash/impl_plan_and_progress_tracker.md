---
title: LayerStack Squash + Live Remount — Implementation Plan & Progress Tracker
tags:
  - ephemeral-os
  - layerstack
  - implementation
  - tracker
status: in_progress
updated: 2026-07-02
---

# Implementation Plan & Progress Tracker

Companion to `spec.md` (design) and `acceptance_criteria.md` (definition of
done). This file is the working document: it is updated **during** the work,
not after it.

## Rules (non-negotiable)

1. **Experiment-first.** Every phase lists experiments under *"Experiments —
   must complete BEFORE implementation"*. No production code for a phase is
   written until every experiment box is checked and its outcome is recorded
   in the Experiment log. Experiments exist because the spec makes claims
   about kernel behavior, lock shapes, and performance that must be verified
   **on the target machine** first — we do not go straight into
   implementation on faith.
2. **Phase gate.** A phase may start only when the previous phase's *Exit
   review* checklist is fully checked and the Progress table row is updated.
   Review = re-read the phase's spec sections, reconcile any drift, update
   every checklist, and record decisions.
3. **Spec is source of truth, and stays true.** Any deviation discovered
   mid-phase (an experiment disproves a claim, an API doesn't fit) goes in
   the Decision log **and** into `spec.md` in the same change. The spec must
   never lag the code.
4. **Descope switch.** If Phase 0's kernel-gate experiments (X0.2/X0.3)
   fail irrecoverably in the supported environment, Phases 5–7 and the
   remount halves of Phases 8/10 are descoped: squash ships commit-only,
   every session reports `leased(unsupported:kernel_gate_not_proven)`, and
   the descope is recorded in the Decision log. Phases 1–4, the storage
   half of 8, 9, and the storage e2e still ship.
5. **Repo law.** No test code in `src/`; no inline comments in production
   code; fault injection is `tests/`-only shims or external process
   control. `cargo clippy --all-targets` and `cargo fmt` clean at every
   phase exit. Work directly on `main`; touch only what the phase requires.

## Progress table

Statuses: `todo` → `experiments` → `implementing` → `review` → `done`
(or `blocked` / `descoped`).

| Phase | Title | Status | Experiments | Impl | Tests | Exit review |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | Environment & kernel ground truth | done | 10/10 | — | — | ☑ |
| 1 | Flatten (layerstack pure) | experiments | 0/3 | 0/2 | 0/1 | ☐ |
| 2 | Substitution map + rewritten lease | todo | 0/3 | 0/2 | 0/1 | ☐ |
| 3 | Squash transaction + commit GC + boot sweep | todo | 0/4 | 0/4 | 0/9 | ☐ |
| 4 | Overlay helpers (move/strict-unmount) | todo | 0/2 | 0/2 | 0/1 | ☐ |
| 5 | Quiesce (namespace-execution) | todo | 0/5 | 0/3 | 0/2 | ☐ |
| 6 | Staged-switch runner (namespace-process) | todo | 0/4 | 0/2 | 0/2 | ☐ |
| 7 | Workspace remount transaction + reap + PDEATHSIG | todo | 0/4 | 0/5 | 0/5 | ☐ |
| 8 | Operation layer: gate, squash op, sweep loop | todo | 0/4 | 0/6 | 0/5 | ☐ |
| 9 | Manager CLI (`checkpoint_squash`) | todo | 0/2 | 0/3 | 0/1 | ☐ |
| 10 | Live Docker e2e + enablement + sign-off | todo | — | 0/3 | 0/13 | ☐ |

---

## Phase 0 — Environment & kernel ground truth (experiments only, no production code)

**Goal:** verify every kernel/filesystem assumption the design stands on,
in the supported Docker sandbox environment, before a single line of
production code. Outputs: pass/fail per gate, measured numbers for the
performance claims, recorded errnos for classification.

**Spec refs:** environment facts; gates 1–3; §Storage (syncfs); §C3; §D
(chain-length, sweep budget); e2e harness ground rules.

**How:** `bin/start-sandbox-docker-gateway`, then shell probes and
disposable scratch programs (under `/tmp` or `tests/` scratch — never
`src/`). Record every result in the Experiment log with the exact command
and output.

### Experiments — must complete BEFORE any implementation phase

- [x] **X0.1 Environment preconditions.** `uname -r` (expect ≥ 6.0; hard
      floor 5.8 for `syncfs` error reporting); `findmnt -no FSTYPE` on the
      layer-stack root (must NOT be overlay); unprivileged `userxattr`
      overlay mount succeeds in the sandbox userns.
- [x] **X0.2 Same-upperdir coexistence (G1 prototype).** Script the full
      G1 sequence with witness files: mount OLD, force copy-up, mount NEW
      at staging with the same upperdir + fresh sibling workdir, probe,
      MS_MOVE pair, probe, strict unmount rollback; then the abort leg
      (stage + unmount without moves, OLD must still copy-up). **This is
      the go/no-go gate for the entire remount half.**
- [x] **X0.3 userxattr resurrection control (G2 prototype).** Delete a
      lowerdir file through OLD, remount NEW over flattened sources with
      the same upperdir: file must stay deleted; repeat once without
      `userxattr` to confirm it resurfaces (the assertion has teeth).
- [x] **X0.4 Mount-move semantics.** In the holder-style namespace:
      propagation type of the workspace root (MS_MOVE requires a private
      parent — verify); `move_mount` with pre-opened `O_PATH` dirfds +
      `MOVE_MOUNT_F_EMPTY_PATH` works in the userns; moving out of a
      shared-propagation parent fails `EINVAL` (E8's natural induction).
- [x] **X0.5 Strict-unmount EBUSY.** Reproduce with an SCM_RIGHTS-parked
      fd (fd sent over a socketpair to self, local copy closed):
      `umount2(path, 0)` returns EBUSY, the mount stays fully usable, and
      the parked mount unmounts cleanly at namespace death.
- [x] **X0.6 syncfs behavior + benchmark.** `syncfs` available and reports
      errors; measure on a 5k-small-file staging tree: single `syncfs` vs
      per-entry fsync walk (expect orders of magnitude — this validates
      deleting the walk); measure worst-case `syncfs` cost when session
      upperdirs share the filesystem and are dirty (it flushes the whole
      fs — quantify the collateral cost and record whether it is
      acceptable at commit frequency).
- [x] **X0.7 OVL_MAX_STACK.** Mount at 500 lowerdirs succeeds, 501 fails;
      record the exact errno for the `stage_failed:<errno>` mapping and
      the creation-path error shape.
- [x] **X0.8 Hardlink flatten feasibility.** Cross-directory `link(2)`
      within the layer-stack filesystem works in the userns (same-fs
      requirement); no practical nlink ceiling at our scale.
- [x] **X0.9 Freeze mechanics + cost.** SIGSTOP → `/proc/*/stat` = `T`
      poll latency for 1/10/100 tasks (validates the ~50 ms claim and
      sets the default freeze budget); SIGKILL works on stopped tasks; a
      `setsid()` escapee is found by the full `/proc` ns-scan; measure the
      ns-scan cost on a loaded machine (validates per-invocation stall).
- [x] **X0.10 Outside observation channel.** From the daemon side,
      `/proc/<holder>/mountinfo` is readable; a staging mount appearing
      and the workspace root's mount ID changing are both detectable —
      this is the kill-point mechanism for E7/E8/E10 and needs no src
      hooks.

### Exit review

- [x] All 10 experiment boxes checked; results (numbers, errnos,
      pass/fail) in the Experiment log with commands.
- [x] Go/no-go recorded: remount half **GO** (X0.2 and X0.3 both pass in
      the supported environment; no descope).
- [x] Freeze-budget default and `syncfs` cost recorded as inputs to
      Phases 3 and 5: freeze of 100 tasks reaches all-`T` in ≤ 2.6 ms, so
      the 500 ms default budget has ≥ 100× headroom; commit `syncfs` is
      ~33 ms clean / ~197 ms with 256 MiB foreign dirty data on the shared
      ext4 — acceptable at explicit-invocation frequency.
- [x] Progress table updated; Phase 1 unblocked.

---

## Phase 1 — Flatten (layerstack pure)

**Goal:** `src/stack/squash/flatten.rs` (~180): pure fold of a block's
layer dirs into one winning changeset — dir entries, whiteout re-emission,
hardlinked whole-file winners, fd-relative no-follow walks.

**Spec refs:** vocabulary `flatten`; invariant 1; §D re-squash cost.

### Experiments — must complete BEFORE implementation

- [ ] **X1.1 Whiteout encoding inventory.** Determine which whiteout
      encodings the current publish path actually produces in this
      environment (char-dev vs xattr fallback); confirm
      `is_kernel_whiteout_meta` + `write_kernel_whiteout` round-trip both;
      probe one real published layer on disk.
- [ ] **X1.2 Walk primitive choice.** Survey the repo's existing
      fd-relative/no-follow walk utilities (layerstack storage, publish
      plan); pick the existing pattern — a new dependency or a new walking
      abstraction is a red flag, record justification if unavoidable.
- [ ] **X1.3 Hardlink micro-benchmark.** Flatten a scratch 1k-entry block:
      confirm whole-file winners are `link(2)` (bytes copied ≈ 0) and
      wall clock is metadata-bound — validates the O(E) claim before the
      design bakes it in.

### Implementation

- [ ] `src/stack/squash/flatten.rs` — pure fold, newest-wins per
      path/subtree; explicit entries for every surviving directory;
      whiteouts/opaques re-emitted only as winners; mode-preserving
      hardlinked `WriteFile` winners; fd-relative no-follow walks.
- [ ] Wiring/exports (`stack/mod.rs` slice of the +25).

### Tests

- [ ] Test 2 `flatten_matrix` — both whiteout encodings, opaque markers,
      shadowed subtrees, dir-created-then-emptied survives, modes
      preserved, hardlinked winners, malicious-symlink no-follow.

### Exit review

- [ ] Experiments logged; impl + tests checked;
      `cargo test -p sandbox-runtime` (layerstack) green; clippy/fmt
      clean; no test code in `src/`.
- [ ] Spec drift reconciled; Progress table updated; Phase 2 unblocked.

---

## Phase 2 — Substitution map + rewritten lease

**Goal:** `src/stack/lease/rewrite.rs` (~100): the in-memory per-root
substitution map, oldest-first raw-run contraction, and
`acquire_rewritten_lease` under one shared writer-lock guard.

**Spec refs:** vocabulary `substitution map`, `rewritten manifest`,
`acquire_rewritten_lease`; invariants 2, 3; B4/B5 rewrite shapes.

### Experiments — must complete BEFORE implementation

- [ ] **X2.1 Registry fit.** Confirm on the real code that
      `LeaseRegistry::acquire` takes an arbitrary `Manifest` (no new
      registry API needed) and that the map can live beside
      `shared_registry_for_root` under the same locking shape — no new
      lock level in the ordering.
- [ ] **X2.2 Contraction property check.** Before coding, replay on paper
      / in a scratch property test: B3, B4 (generation-crossing
      `Sc→[L8,Sa]`), B5, plus adversarial shapes (overlapping candidate
      runs, repeated layer ids, self-referential entries). Prove
      determinism, termination, and never-straddle compatibility. If any
      shape breaks oldest-first raw-run contraction, stop and revise the
      spec before implementing.
- [ ] **X2.3 Map lifecycle.** Confirm where commit records entries and
      that daemon restart genuinely empties the map with no consumer left
      (fact 2: no session survives restart) — trace the restart path in
      code.

### Implementation

- [ ] `src/stack/lease/rewrite.rs` — map type + recording + contraction +
      `acquire_rewritten_lease` (validate-alive, acquire, or `Identity`).
- [ ] Exports (`stack/lease/mod.rs` slice).

### Tests

- [ ] Test 8 `in_memory_substitutions_match_expand_then_contract` —
      B4 shapes, missing-entry ⇒ identity, bounded single pass,
      post-restart no rewrite + keep-set-only sweep.

### Exit review

- [ ] Experiments logged (X2.2 outcome is a mandatory Decision-log
      entry); impl + tests checked; crate tests green; clippy/fmt clean.
- [ ] Spec drift reconciled; Progress table updated; Phase 3 unblocked.

---

## Phase 3 — Squash transaction + commit GC + boot sweep + syncfs

**Goal:** `src/stack/squash.rs` (~250) plan → build → commit;
`lease/cleanup.rs` (+70) sidecar fix + boot sweep; `storage/fs.rs` (+12)
syncfs helper + removed-set return.

**Spec refs:** §Storage layout and transaction (phase table, boot
cleanup); vocabulary `plan lease`, `commit recheck`, `SquashBlock`,
`boundary`; tests 1, 3–7, 13, 14, 20, 21.

### Experiments — must complete BEFORE implementation

- [ ] **X3.1 Release-path API delta.** Verify `release_lease_locked` can
      return the removed set without breaking existing callers (survey
      all call sites); confirm the reentrant writer lock
      (`storage/lock.rs` `write_depth`) legally supports the nested
      release inside the exclusive commit section.
- [ ] **X3.2 Commit-sequence dry run.** With fs.rs primitives only
      (`allocate_layer_dirs`, `write_atomic`, `fsync_dir`, rename),
      rehearse recheck → promote → syncfs → manifest rename in a scratch
      harness; verify promote is a same-fs `rename(2)` (not a copy) and
      the in-process error path can remove a promoted S dir cleanly.
- [ ] **X3.3 Crash-window rehearsal.** `kill -9` the scratch harness
      between promote and rename; verify on-disk state is exactly
      "old manifest + orphan S dir" and that keep-set sweeping reclaims
      it — before writing the real sweep.
- [ ] **X3.4 Sweep candidate enumeration.** Confirm the disk listing →
      keep-set → shared `remove_layers` shape covers staging, layers, and
      sidecars with ONE routine; verify `read_manifest`'s
      empty-v0-on-missing behavior (fs.rs:170) so the fail-closed guard
      is placed correctly.

### Implementation

- [ ] `src/stack/squash.rs` — plan (boundaries via
      `lease_newest_layers()`, blocks ≥ 2), build (flatten into staging),
      commit (own ~25-line tail, recheck-first; plan-lease release as the
      only GC; `operation_failed` on abort).
- [ ] `src/stack/lease/cleanup.rs` — `.digest` + `.bytes` removal with the
      layer dir; set-based membership; boot storage sweep (fail-closed,
      `B*` guard, keep-set = active manifest) sharing `remove_layers`.
- [ ] `src/storage/fs.rs` — syncfs helper on the storage-root fd;
      removed-set plumbing.
- [ ] Wiring/exports.

### Tests

- [ ] Test 1 `partition_blocks_between_boundaries_and_base`
- [ ] Test 3 `commit_gc_never_deletes_layers_leased_after_plan`
- [ ] Test 4 `commit_recheck_compacts_through_racing_publish_or_aborts_cleanly`
- [ ] Test 5 `squash_singleflight_per_root`
- [ ] Test 6 `crash_and_error_paths_around_commit`
- [ ] Test 7 `syncfs_commit_durability` (syscall-recording shim in `tests/`)
- [ ] Test 13 `old_layers_not_deleted_until_refcount_zero`
- [ ] Test 14 `boot_cleanup_matrix` (incl. shared-routine + `.bytes`
      regression)
- [ ] Tests 20 `commit_gc_is_plan_lease_release` + 21
      `squash_commits_with_no_s_layer_sidecars`

### Exit review

- [ ] Experiments logged; impl + tests checked; **`git diff` on
      `stack/ops/publish.rs` is empty**; crate tests green; clippy/fmt
      clean.
- [ ] Milestone note: storage-only squash is now functionally complete
      behind the (not yet wired) operation — record it.
- [ ] Spec drift reconciled; Progress table updated; Phase 4 unblocked.

---

## Phase 4 — Overlay helpers

**Goal:** `overlay/src/kernel_mount.rs` (+35): `move_mountpoint`,
`strict_unmount`. The production fsconfig builder is reused unchanged.

**Spec refs:** vocabulary `staged switch`, `strict unmount`; C3
preconditions.

### Experiments — must complete BEFORE implementation

- [ ] **X4.1 Syscall surface.** Confirm the workspace-pinned
      nix/rustix version already exposes `move_mount`/`umount2` as needed
      (no version bumps, per workspace-deps convention); match the
      fd-based patterns already in `kernel_mount.rs`.
- [ ] **X4.2 Re-verify X0.4/X0.5 through the crate's own abstractions**
      (a 20-line scratch test using the new helpers' intended signatures)
      so the API is proven before it lands.

### Implementation

- [ ] `move_mountpoint` (dirfd-based) + `strict_unmount`
      (`umount2(path, 0)`, no lazy fallback) + exports.
- [ ] No real-path mode, no lowerdir introspection — verify nothing of
      the sort creeps in.

### Tests

- [ ] `tests/unit/kernel_mount.rs` (+40) — move + strict-unmount
      behavior, EBUSY surfaced verbatim.

### Exit review

- [ ] Standard review (experiments logged, tests green, clippy/fmt, spec
      drift, table updated); Phase 5 unblocked.

---

## Phase 5 — Quiesce (namespace-execution)

**Goal:** `namespace-execution/src/quiesce.rs` (~200): discovery
(cgroup ∪ ns-scan ∪ allowlist), SIGSTOP freeze, poll-to-`T` within budget,
`/proc` pin inspection with ONE holder mountinfo read, resume-on-drop
guard; `engine.remount_overlay` (+30).

**Spec refs:** vocabulary `quiesce`, `pin`; §C4; §C6; §D sweep budget.

### Experiments — must complete BEFORE implementation

- [ ] **X5.1 Discovery legs on the machine.** Confirm cgroup placement is
      genuinely best-effort/`Option` in `launcher.rs` (ns-scan is the
      freeze-set proof); verify a `setsid()` escapee and an
      `unshare -m` escapee are each caught by the correct leg.
- [ ] **X5.2 Allowlist identification.** Enumerate how holder, pid-ns
      init, and the runner are identified from existing state (pids
      already tracked? stable comm/ppid?) — the allowlist must be
      constructed from daemon-owned facts, not guesses.
- [ ] **X5.3 Inspection parsing corpus.** Build a scratch corpus on the
      real kernel: `maps` lines with spaces and `(deleted)`; fd link
      strings for PTY/socket/pipe/eventfd/timerfd/io_uring; `t`-state
      with an outside tracer; mountinfo octal escaping. Freeze the parser
      rules against reality before coding them.
- [ ] **X5.4 Budget calibration.** Re-run X0.9 through the intended
      quiesce shape (stop → poll → membership-stable) and set the default
      freeze budget from measured data; document the number in the spec
      if it differs from the ~500 ms example.
- [ ] **X5.5 One-read mountinfo.** Verify child-mount detection from a
      single holder mountinfo read catches a bind mount whose creating
      task has exited (the E4 sub-case) — proving per-task reads are
      unnecessary on this kernel.

### Implementation

- [ ] `src/quiesce.rs` — discovery union, freeze, poll, membership-stable,
      pin inspection (any read error = pinned), resume-on-drop guard.
- [ ] `src/engine.rs` — `remount_overlay` beside `mount_overlay`.
- [ ] Exports.

### Tests

- [ ] `tests/quiesce.rs` (~80) — discovery/freeze/inspect matrix with
      fixture processes.
- [ ] `tests/engine.rs` (+25).

### Exit review

- [ ] Standard review; measured budget + parser corpus results in the
      Experiment log; Phase 6 unblocked.

---

## Phase 6 — Staged-switch runner (namespace-process)

**Goal:** `runner/setns/remount_overlay.rs` (~200): narrowed
`RemountMaskGuard` (build window only), pre-opened dirfds, MS_MOVE pair,
strict rollback-unmount (EBUSY = park), two-boolean report on all paths;
protocol fields (+25).

**Spec refs:** §C3 (steps 1–9); vocabulary `staged switch`,
`point of no return`, `parked old mount`.

### Experiments — must complete BEFORE implementation

- [ ] **X6.1 Masked-build necessity.** Try building the staged NEW mount
      with masks ON in the holder namespace. If it works, the unmask step
      (C3 step 1) is deletable — record and update the spec either way.
      If it fails (expected: upperdir/workdir under masked roots are not
      kernel-resolvable), the narrowed guard stands.
- [ ] **X6.2 Dirfds across remask.** Verify `O_PATH` dirfds opened on
      staging/rollback/workspace-root before remask remain usable for
      `move_mount` and for re-opened probe reads after remask.
- [ ] **X6.3 Probe-through-dirfd.** Verify witness reads via
      `openat(dirfd, …)` on the staged mount; define the probe set (from
      the rewritten chain's witness content).
- [ ] **X6.4 Kill-matrix rehearsal.** Using X0.10's observation channel,
      rehearse killing a scratch runner at the three E8 points and
      confirm the daemon side can classify each from report
      presence/booleans alone.

### Implementation

- [ ] `src/runner/setns/remount_overlay.rs` — steps 1–9 exactly as C3;
      report always emitted with `first_move_succeeded`,
      `mount_verified`, free-form detail.
- [ ] `src/runner/{setns,protocol,mod}.rs` — op entry, request fields
      (incl. fresh workdir path), dispatch.

### Tests

- [ ] `tests/unit/runner/setns.rs` (+60) — step ordering, report shape.
- [ ] Test 19 `runner_report_two_booleans_drive_policy` (daemon-side
      classification half lands with Phase 7 wiring; runner half here).

### Exit review

- [ ] Standard review; X6.1 outcome recorded as a Decision-log entry
      (unmask kept or deleted); Phase 7 unblocked.

---

## Phase 7 — Workspace remount transaction + boot reap + PDEATHSIG

**Goal:** `workspace/src/lifecycle/remount.rs` (~120) — the whole
transaction with C5 failure rules; `service/impls/remount_workspace.rs`
(~40); boot reap in `lifecycle/persistence.rs` (+50); `setns_runner.rs`
(+35); `holder.rs` PDEATHSIG (+5).

**Spec refs:** §C2, §C5; invariants 2, 4, 5, 6; environment facts 1–3;
boot cleanup step 2.

### Experiments — must complete BEFORE implementation

- [ ] **X7.1 PDEATHSIG probe.** Prototype `pre_exec`
      `PR_SET_PDEATHSIG(SIGKILL)` on the holder spawn path; SIGKILL the
      parent; verify holder death and namespace teardown (this is
      environment fact 1 — it must be proven, not assumed).
- [ ] **X7.2 Park-state carrier.** Design-spike where the parked old
      lease lives until destroy **without a new state enum or struct
      field beyond a lease handle** — confirm the existing session/lease
      guard types can carry it and that destroy releases both. If a new
      field is unavoidable, it must be exactly one `Option<lease>` and
      recorded in the Decision log.
- [ ] **X7.3 dirs.workdir mutation.** Confirm swapping
      `MountedWorkspace.snapshot` + mutating `dirs.workdir` in place
      composes with every existing reader of those fields (grep all
      uses); confirm `persist_handles` picks the new value up with zero
      schema change.
- [ ] **X7.4 Reap-in-persistence fit.** Verify `persistence.rs` owns all
      `manager.json` path/schema knowledge needed for reap (no parse
      helpers exported to a second file) and that reap-before-sweep
      ordering has a single natural call site for Phase 8's boot hook.

### Implementation

- [ ] `src/lifecycle/remount.rs` — rewritten lease → freeze → runner →
      verify → best-effort persist → resume → release old lease; EBUSY
      park; faulty → ordinary destroy; all C5 rows.
- [ ] `src/service/impls/remount_workspace.rs` — thin delegate.
- [ ] `src/lifecycle/persistence.rs` — boot reap (destroy run dirs, drop
      handles, observability record).
- [ ] `src/namespace/setns_runner.rs` — `NamespaceRuntime::remount_overlay`.
- [ ] `src/namespace/holder.rs` — PDEATHSIG `pre_exec`.

### Tests

- [ ] Test 10 `retarget_never_runs_before_mount_verification`
- [ ] Test 11 `post_commit_remount_failure_does_not_fail_squash_commit`
- [ ] Test 12 `persist_failure_still_migrates`
- [ ] Test 15 `ebusy_park_keeps_both_leases_and_converges`
- [ ] `tests/unit/{remount.rs, recover.rs}` — transaction + reap units.

### Exit review

- [ ] Standard review; X7.2 outcome in the Decision log; Phase 8
      unblocked.

---

## Phase 8 — Operation layer: admission gate, squash op, sweep loop

**Goal:** per-session admission gate subsuming `session_lifecycle_lock`;
`layerstack/service/impls/squash.rs` (~110) with the per-session sweep
loop and result assembly; `remount_session.rs` (~60); boot hook in
`services.rs`; observability records (+8); `squash_layerstack` registered
with `cli: None`.

**Spec refs:** §C1; lock discipline; §Output contract; boot cleanup;
§A operation block.

### Experiments — must complete BEFORE implementation

- [ ] **X8.1 Entrypoint audit.** Enumerate by grep EVERY workspace-session
      entrypoint that must route through the gate: exec launch, one-shot
      create/finalize incl. engine completion/timeout/cancel hooks
      (`finalize_one_shot`), file read/write/edit, capture, destroy,
      runner entrypoints, remount. Produce the definitive list in the
      Experiment log — a missed entrypoint is a correctness hole.
- [ ] **X8.2 Lifecycle-lock subsumption proof.** Enumerate every
      `session_lifecycle_lock` use; confirm nothing it serializes is
      cross-session; sketch the lock-order argument (sessions-map <
      gate < writer lock) and check no path waits on the gate while
      holding the sessions-map mutex.
- [ ] **X8.3 `cli: None` registration.** Compile-probe that an
      `OperationEntry` without a CLI spec dispatches by name and appears
      in no catalog — no new constructor/mechanism.
- [ ] **X8.4 Result assembly inputs.** Confirm the removed-set (Phase 3)
      + post-sweep registry reads suffice for
      `replaced_layers`/`blocked_reasons`/`faulty_sessions` with no extra
      round trips.

### Implementation

- [ ] Per-session admission gate in the workspace-session service; route
      all X8.1 entrypoints; delete `session_lifecycle_lock`
      (`command/service/core.rs` +15/−10).
- [ ] `src/layerstack/service/impls/squash.rs` — storage squash +
      post-commit snapshot + per-session sweep loop + result assembly.
- [ ] `src/workspace_session/service/impls/remount_session.rs` — gate
      hold, snapshot, delegate, registry refresh (existing
      `refresh_after_capture` idiom).
- [ ] `src/services.rs` — boot hook: reap + storage sweep, once, before
      serving (asserts the kernel floor once).
- [ ] DTOs/exports; `squash_layerstack` with `cli: None`.
- [ ] `sandbox-observability/src/record.rs` — the three records.

### Tests

- [ ] Test 9 `admission_blocks_all_workspace_session_entrypoints`
- [ ] Test 16 `faulty_outcome_is_reported_then_destroyed`
- [ ] Test 17 `squash_output_contract`
- [ ] Test 22 `ultra_nonfaulty_sweep_converges`
- [ ] `tests/layerstack_squash.rs` integration (~300).

### Exit review

- [ ] Standard review; grep shows zero `session_lifecycle_lock`
      references; Phase 9 unblocked.

---

## Phase 9 — Manager CLI (`checkpoint_squash`)

**Goal:** `manager/.../impls/checkpoint_squash.rs` (~30) delegating to the
generic forward; `CliOperationSpec` under the existing `"management"`
family (+25); registration (+10).

**Spec refs:** §CLI surface; §Output contract examples.

### Experiments — must complete BEFORE implementation

- [ ] **X9.1 Forward-path trace.** Scratch-test one request through
      `router/forward.rs` with the renamed op (`checkpoint_squash` in,
      `squash_layerstack` to the daemon): endpoint lookup, Ready check,
      timeout all reused; confirm the impl needs no bespoke client
      sequence.
- [ ] **X9.2 Catalog shape.** Confirm the `"management"` family carries
      the new spec cleanly and that no `"checkpoint"` family or name
      translation layer creeps in beyond the one op-name mapping.

### Implementation

- [ ] `impls/checkpoint_squash.rs` — parse sandbox id, delegate.
- [ ] `cli_definition/management_operations.rs` — spec under
      `"management"`.
- [ ] Register in `operation/{mod,dispatch,specs}.rs`.

### Tests

- [ ] Test 18 `checkpoint_squash_manager_cli_forwards_to_runtime` +
      `tests/manager_core.rs` catalog/forwarding (+50).

### Exit review

- [ ] Standard review; end-to-end manual smoke:
      `sandbox-cli manager checkpoint_squash --sandbox-id <id>` against a
      live gateway returns the contract JSON; Phase 10 unblocked.

---

## Phase 10 — Live Docker e2e, enablement, sign-off

**Goal:** formalize Phase 0's prototypes as the G1–G3 gate tests, land
E1–E10, flip live remount on only when all gates pass, and sign off
against `acceptance_criteria.md`.

**Spec refs:** §Live Docker e2e (harness ground rules, G1–G3, E1–E10,
explicitly-not-covered).

### Implementation

- [ ] e2e harness: environment preconditions (hard-fail), outside
      mountinfo observation helpers, witness-file fixtures,
      strict-unmount-only teardown assertions — all in `tests/`.
- [ ] Gate tests G1–G3 (from the X0.2/X0.3 prototypes + reap ordering).
- [ ] Enablement wiring: live remount active only with gates proven;
      otherwise every session reports
      `leased(unsupported:kernel_gate_not_proven)`.

### Tests (all in the supported Docker environment)

- [ ] G1 `same_upperdir_fresh_workdir_kernel_gate` (incl. failure leg)
- [ ] G2 `production_builder_parity_no_resurrection` (incl. negative
      control)
- [ ] G3 `startup_cleanup_reap_then_sweep` (incl. unreadable-manifest leg)
- [ ] E1 `all_task_quiesce_blocks_escaped_pgid_child`
- [ ] E2 `nested_mount_namespace_blocks_remount`
- [ ] E3 `masks_never_observable_and_mask_failure_is_clean_skip`
- [ ] E4 `proc_pin_matrix_blocks_uncertainty`
- [ ] E5 `live_migration_under_running_batch_command`
- [ ] E6 `strict_unmount_ebusy_keeps_both_leases_and_converges`
- [ ] E7 `post_ponr_unverified_failure_is_faulty_destroy`
- [ ] E8 `ponr_boundary_two_boolean_report`
- [ ] E9 `staged_mount_over_ovl_max_stack_is_clean_skip`
- [ ] E10 `crash_matrix_recovery`

### Exit review (= feature sign-off)

- [ ] Every checklist in `acceptance_criteria.md` checked with its
      verification.
- [ ] All experiment and decision logs complete; spec.md matches shipped
      behavior.
- [ ] Progress table shows every phase `done` (or `descoped` with the
      rule-4 record).

---

## Experiment log

Record every experiment here when its box is checked. Evidence = exact
command(s) + key output, or a path to a scratch script.

| Date | Phase | ID | Result (pass/fail + numbers/errnos) | Evidence |
| --- | --- | --- | --- | --- |
| 2026-07-02 | 0 | X0.1 | PASS. Kernel `6.12.76-linuxkit` (≥ 6.0). `/eos/layer-stack` = ext4 named volume (`f_type 0xef53`, mountinfo `254:1 ext4 /dev/vda1`); `/eos/workspace` = ext4; base = separate `ro` ext4 mount at `/eos/layer-stack/base`. userns `userxattr` overlay mount + copy-up write OK. NOTE: pre-existing containers created before the layer-stack-volume fix had `/eos/layer-stack` on the container overlayfs rootfs; a fresh sandbox from current `main` has the ext4 volume — precondition holds for current code. | `bin/start-sandbox-docker-gateway`; `sandbox-cli manager create_sandbox --image ubuntu:24.04 --workspace-bind-root /tmp/eos-squash-testbed` → `eos-3312cf45…`; `docker exec … /probe x01` (scratch probe `/tmp/eos-squash-probes/src/main.rs`, aarch64-musl) |
| 2026-07-02 | 0 | X0.2 | PASS — **GO for the remount half**. OLD `[l2,l1]+U+Wold` and staged NEW `[S]+U+W-remount-1` coexist; all witness reads exact on staged NEW, post-switch, incl. copy-up content, whiteout-masked absence, dir-created-then-emptied, mode 0640; `move_mount` pair OK; strict `umount2(rollback,0)`=0; post-switch copy-up OK. Abort leg: stage+strict-unmount without moves, then OLD copy-up durable in U2. **Finding: a held pre-opened `O_PATH` fd on the OLD mount root pins the moved OLD mount → strict unmount self-EBUSYs; the runner must drop the OLD-root dirfd after the second move, before step 8** (spec C3 updated). | `docker exec … /probe x02 /eos/workspace/probe-scratch` — all 25 checks `[ok]`; first run failed only on the held-fd EBUSY, rerun after dropping fds fully green |
| 2026-07-02 | 0 | X0.3 | PASS. Kernel encodings on this environment: deleted-file whiteout = **char 0:0 device, zero xattrs** (xattr-independent, does NOT resurface without `userxattr`); recreated-dir opaque marker = **`user.overlay.opaque=y`**. Negative control (same mount, no `userxattr`): mount SUCCEEDS in the userns; opaque marker unread → **2 lower entries resurface** in the recreated dir. G2's teeth are the opaque-dir case, not the plain whiteout (spec G2 updated). | `docker exec … /probe x03 /eos/workspace/probe-scratch` |
| 2026-07-02 | 0 | X0.4 | PASS. In the holder-style ns (rec-private `/`): the workspace parent volume mount carries no `shared:` tags; `move_mount(O_PATH fd, "", CWD, path, MOVE_MOUNT_F_EMPTY_PATH)` moves a live overlay; classic `mount(MS_MOVE)` parity OK; moving a mount out of a `MS_SHARED` parent fails **EINVAL** (E8 induction confirmed). | `docker exec … /probe x04 /eos/workspace/probe-scratch` |
| 2026-07-02 | 0 | X0.5 | PASS. fd sent to self via `SCM_RIGHTS`, local copy closed → `umount2(ws,0)` = **EBUSY (16)**; parked mount fully usable (read + copy-up write); overlay visible from outside via `/proc/<pid>/mountinfo` until ns death; after namespace death scratch removable, no residue. | `docker exec … /probe x05 /eos/workspace/probe-scratch` |
| 2026-07-02 | 0 | X0.6 | PASS. 5000 files + 50 dirs: per-entry fsync walk (5050 fsyncs) = **3.27 s**; single `syncfs` = **33 ms** (99×); `syncfs` with 256 MiB foreign dirty data on the same fs = **197 ms**. All sandbox volumes share one ext4 superblock (`/dev/vda1`), so `syncfs` flushes the whole VM data disk — collateral measured and acceptable at explicit-invocation frequency. `syncfs` returns 0; kernel 6.12 ≥ 5.8 error-reporting floor. | `docker exec … /probe x06 /eos/workspace/probe-scratch` |
| 2026-07-02 | 0 | X0.7 | PASS. 500 lowerdirs mount + bottom-marker read OK. 501st layer fails at the **`fsconfig lowerdir+` call itself with EINVAL (22)** (not at create/fsmount) — the production builder surfaces it as `MountSyscall{context:"fsconfig lowerdir+"}`, a clean pre-PONR `stage_failed:<errno>` with zero side effects. | `docker exec … /probe x07 /eos/workspace/probe-scratch` |
| 2026-07-02 | 0 | X0.8 | PASS. Cross-directory `link(2)` within the layer-stack fs works in the userns (inode shared, mode 0640 preserved); 1000 links to one inode in **6.4 ms** (nlink 1002; ext4 ceiling 65 000 — no practical limit at our scale). | `docker exec … /probe x08 /eos/workspace/probe-scratch` |
| 2026-07-02 | 0 | X0.9 | PASS. SIGSTOP → all-`T` poll: 1 task ~76–152 µs, 10 tasks ~250–600 µs, 100 tasks **1.3–2.6 ms** — the ~50 ms spec claim holds with ≥ 20× margin; default freeze budget 500 ms confirmed generous. SIGKILL on stopped tasks works (reaped as SIGKILL). `setsid()` escapee found by full `/proc` ns/mnt scan; scan of container `/proc` = **74 µs** (container pid-ns scope ≈ sandbox scope). | `docker exec … /probe x09` |
| 2026-07-02 | 0 | X0.10 | PASS. `/proc/<holder>/mountinfo` readable from outside the userns; staging mount appearance detectable (new mount id); after the `MS_MOVE` pair the workspace root's mount id changes (135 → 138) and the old id is visible at the rollback point — deterministic kill-point mechanism for E7/E8/E10 with zero src hooks. | `docker exec … /probe x10 /eos/workspace/probe-scratch` |

## Decision log

Every spec deviation, descope, or design call made mid-implementation.
Each entry must reference the spec section it changes and confirm the spec
was updated in the same change.

| Date | Phase | Decision | Spec section updated |
| --- | --- | --- | --- |
| 2026-07-02 | 0 | **Go/no-go: GO.** X0.2 (same-upperdir coexistence + staged switch) and X0.3 (userxattr parity) both pass in the supported Docker environment; phases 5–7 proceed, no descope. | none needed (spec already gated on this proof) |
| 2026-07-02 | 0 | **OLD-root dirfd must be dropped before the strict rollback unmount.** X0.2 proved a held pre-opened `O_PATH` fd on the OLD mount root pins the moved OLD mount at rollback, self-inflicting EBUSY at C3 step 8. The runner closes the OLD-root dirfd after the second move (it is only needed as the move-1 source). | §C3 step 8 |
| 2026-07-02 | 0 | **G2's negative control targets the opaque-dir marker, not the plain-file whiteout.** X0.3 measured: deleted-file whiteouts are char 0:0 devices (xattr-independent — they hold without `userxattr`); the xattr-encoded metadata with resurrection teeth is `user.overlay.opaque` (lower entries resurface without `userxattr`). | §Required tests → Live Docker e2e G2 |
| 2026-07-02 | 0 | **OVL_MAX_STACK failure point recorded**: the over-limit chain fails the `fsconfig lowerdir+` call (EINVAL), not `fsmount` — still a clean pre-PONR `stage_failed:<errno>` derived from the mount-build error; wording folded into §D. | §D chain-length paragraph |
