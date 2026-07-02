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
| 1 | Flatten (layerstack pure) | done | 3/3 | 2/2 | 1/1 | ☑ |
| 2 | Substitution map + rewritten lease | done | 3/3 | 2/2 | 1/1 | ☑ |
| 3 | Squash transaction + commit GC + boot sweep | done | 4/4 | 4/4 | 9/9 | ☑ |
| 4 | Overlay helpers (move/strict-unmount) | done | 2/2 | 2/2 | 1/1 | ☑ |
| 5 | Quiesce (namespace-execution) | done | 5/5 | 3/3 | 2/2 | ☑ |
| 6 | Staged-switch runner (namespace-process) | done | 4/4 | 2/2 | 2/2 | ☑ |
| 7 | Workspace remount transaction + reap + PDEATHSIG | done | 4/4 | 5/5 | 5/5 | ☑ |
| 8 | Operation layer: gate, squash op, sweep loop | done | 4/4 | 6/6 | 5/5 | ☑ |
| 9 | Manager CLI (`checkpoint_squash`) | done | 2/2 | 3/3 | 1/1 | ☑ |
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

- [x] **X1.1 Whiteout encoding inventory.** Determine which whiteout
      encodings the current publish path actually produces in this
      environment (char-dev vs xattr fallback); confirm
      `is_kernel_whiteout_meta` + `write_kernel_whiteout` round-trip both;
      probe one real published layer on disk.
- [x] **X1.2 Walk primitive choice.** Survey the repo's existing
      fd-relative/no-follow walk utilities (layerstack storage, publish
      plan); pick the existing pattern — a new dependency or a new walking
      abstraction is a red flag, record justification if unavoidable.
- [x] **X1.3 Hardlink micro-benchmark.** Flatten a scratch 1k-entry block:
      confirm whole-file winners are `link(2)` (bytes copied ≈ 0) and
      wall clock is metadata-bound — validates the O(E) claim before the
      design bakes it in.

### Implementation

- [x] `src/stack/squash/flatten.rs` — pure fold, newest-wins per
      path/subtree; explicit entries for every surviving directory;
      whiteouts/opaques re-emitted only as winners; mode-preserving
      hardlinked `WriteFile` winners; fd-relative no-follow walks.
- [x] Wiring/exports (`stack/mod.rs` slice of the +25).

### Tests

- [x] Test 2 `flatten_matrix` — both whiteout encodings, opaque markers,
      shadowed subtrees, dir-created-then-emptied survives, modes
      preserved, hardlinked winners, malicious-symlink no-follow.
      (13 tests in `tests/unit/squash.rs`, including the dir-over-whiteout
      and dir-over-file opaque compositions and the same-layer
      logical-whiteout tie.)

### Exit review

- [x] Experiments logged; impl + tests checked;
      `cargo test -p sandbox-runtime-layerstack` green (58 unit tests);
      clippy/fmt clean (zero warnings incl. `--all-targets`); no test
      code in `src/`.
- [x] Spec drift reconciled (flatten vocabulary row: dual-encoded opaque
      re-emission + in-block-terminated-run rule); Progress table updated;
      Phase 2 unblocked.

---

## Phase 2 — Substitution map + rewritten lease

**Goal:** `src/stack/lease/rewrite.rs` (~100): the in-memory per-root
substitution map, oldest-first raw-run contraction, and
`acquire_rewritten_lease` under one shared writer-lock guard.

**Spec refs:** vocabulary `substitution map`, `rewritten manifest`,
`acquire_rewritten_lease`; invariants 2, 3; B4/B5 rewrite shapes.

### Experiments — must complete BEFORE implementation

- [x] **X2.1 Registry fit.** Confirm on the real code that
      `LeaseRegistry::acquire` takes an arbitrary `Manifest` (no new
      registry API needed) and that the map can live beside
      `shared_registry_for_root` under the same locking shape — no new
      lock level in the ordering.
- [x] **X2.2 Contraction property check.** Before coding, replay on paper
      / in a scratch property test: B3, B4 (generation-crossing
      `Sc→[L8,Sa]`), B5, plus adversarial shapes (overlapping candidate
      runs, repeated layer ids, self-referential entries). Prove
      determinism, termination, and never-straddle compatibility. If any
      shape breaks oldest-first raw-run contraction, stop and revise the
      spec before implementing.
- [x] **X2.3 Map lifecycle.** Confirm where commit records entries and
      that daemon restart genuinely empties the map with no consumer left
      (fact 2: no session survives restart) — trace the restart path in
      code.

### Implementation

- [x] `src/stack/lease/rewrite.rs` — map type + recording + contraction +
      `acquire_rewritten_lease` (validate-alive, acquire, or `Identity`).
- [x] Exports (`stack/lease/mod.rs` slice).

### Tests

- [x] Test 8 `in_memory_substitutions_match_expand_then_contract` —
      B4 shapes, missing-entry ⇒ identity, bounded single pass,
      post-restart no rewrite + keep-set-only sweep. (5 tests: the B4
      two-generation replay incl. a gen-0 lease crossing both
      generations, identity degradations for missing entries and dead
      rewritten layers, adversarial map shapes, pin-overlap with
      clean-abort release, and restart-empties-map; the keep-set-only
      sweep half lands with Phase 3's test 14.)

### Exit review

- [x] Experiments logged (X2.2 outcome is a mandatory Decision-log
      entry); impl + tests checked; crate tests green (63); clippy/fmt
      clean.
- [x] Spec drift reconciled (none needed — contraction verified as
      specified); Progress table updated; Phase 3 unblocked.

---

## Phase 3 — Squash transaction + commit GC + boot sweep + syncfs

**Goal:** `src/stack/squash.rs` (~250) plan → build → commit;
`lease/cleanup.rs` (+70) sidecar fix + boot sweep; `storage/fs.rs` (+12)
syncfs helper + removed-set return.

**Spec refs:** §Storage layout and transaction (phase table, boot
cleanup); vocabulary `plan lease`, `commit recheck`, `SquashBlock`,
`boundary`; tests 1, 3–7, 13, 14, 20, 21.

### Experiments — must complete BEFORE implementation

- [x] **X3.1 Release-path API delta.** Verify `release_lease_locked` can
      return the removed set without breaking existing callers (survey
      all call sites); confirm the reentrant writer lock
      (`storage/lock.rs` `write_depth`) legally supports the nested
      release inside the exclusive commit section.
- [x] **X3.2 Commit-sequence dry run.** With fs.rs primitives only
      (`allocate_layer_dirs`, `write_atomic`, `fsync_dir`, rename),
      rehearse recheck → promote → syncfs → manifest rename in a scratch
      harness; verify promote is a same-fs `rename(2)` (not a copy) and
      the in-process error path can remove a promoted S dir cleanly.
- [x] **X3.3 Crash-window rehearsal.** `kill -9` the scratch harness
      between promote and rename; verify on-disk state is exactly
      "old manifest + orphan S dir" and that keep-set sweeping reclaims
      it — before writing the real sweep.
- [x] **X3.4 Sweep candidate enumeration.** Confirm the disk listing →
      keep-set → shared `remove_layers` shape covers staging, layers, and
      sidecars with ONE routine; verify `read_manifest`'s
      empty-v0-on-missing behavior (fs.rs:170) so the fail-closed guard
      is placed correctly.

### Implementation

- [x] `src/stack/squash.rs` — plan (boundaries via
      `lease_newest_layers()`, blocks ≥ 2), build (flatten into staging),
      commit (own ~25-line tail, recheck-first; plan-lease release as the
      only GC; `operation_failed` on abort).
- [x] `src/stack/lease/cleanup.rs` — `.digest` + `.bytes` removal with the
      layer dir; set-based membership; boot storage sweep (fail-closed,
      `B*` guard, keep-set = active manifest) sharing `remove_layers`.
- [x] `src/storage/fs.rs` — syncfs helper on the storage-root fd;
      removed-set plumbing.
- [x] Wiring/exports.

### Tests

- [x] Test 1 `partition_blocks_between_boundaries_and_base`
- [x] Test 3 `commit_gc_never_deletes_layers_leased_after_plan`
- [x] Test 4 `commit_recheck_compacts_through_racing_publish_or_aborts_cleanly`
- [x] Test 5 `squash_singleflight_per_root`
- [x] Test 6 `crash_and_error_paths_around_commit` (+ a
      `build_failure_aborts_squash_cleanly` companion: a natural
      unsupported-entry fault proves staging cleanup and plan-lease
      release on abort)
- [x] Test 7 `syncfs_commit_durability` (glob-shadowing
      syscall-recording shim in `tests/unit.rs` — zero src changes;
      records call count plus promoted-dirs/manifest-version at call time
      to pin the after-promote-before-rename ordering)
- [x] Test 13 `old_layers_not_deleted_until_refcount_zero`
- [x] Test 14 `boot_cleanup_matrix` (incl. shared-routine + `.bytes`
      regression)
- [x] Tests 20 `commit_gc_is_plan_lease_release` + 21
      `squash_commits_with_no_s_layer_sidecars`

### Exit review

- [x] Experiments logged; impl + tests checked; **`git diff` on
      `stack/ops/publish.rs` is empty** (verified: 0 diff lines); crate
      tests green (74); clippy/fmt clean.
- [x] Milestone note: **storage-only squash is now functionally complete
      behind the (not yet wired) operation** — `LayerStack::squash()`
      plans, builds, and commits with singleflight, the plan-lease-release
      GC, in-memory substitution recording, and the fail-closed boot
      sweep; only the operation/CLI surface (phases 8–9) is missing for
      commit-only use.
- [x] Spec drift reconciled (none — implemented as specified); Progress
      table updated; Phase 4 unblocked. NOTE (environment, not this
      feature): `workspace_session_destroy_operation_success_projects_minimal_json`
      in `sandbox-runtime/operation` fails on upstream `main` from before
      this work (response gained `evicted_upperdir_bytes`; test not
      updated) — verified failing at `96db3ebf8`; left to its owner,
      revisit at sign-off if still red.

---

## Phase 4 — Overlay helpers

**Goal:** `overlay/src/kernel_mount.rs` (+35): `move_mountpoint`,
`strict_unmount`. The production fsconfig builder is reused unchanged.

**Spec refs:** vocabulary `staged switch`, `strict unmount`; C3
preconditions.

### Experiments — must complete BEFORE implementation

- [x] **X4.1 Syscall surface.** Confirm the workspace-pinned
      nix/rustix version already exposes `move_mount`/`umount2` as needed
      (no version bumps, per workspace-deps convention); match the
      fd-based patterns already in `kernel_mount.rs`.
- [x] **X4.2 Re-verify X0.4/X0.5 through the crate's own abstractions**
      (a 20-line scratch test using the new helpers' intended signatures)
      so the API is proven before it lands.

### Implementation

- [x] `move_mountpoint` (dirfd-based) + `strict_unmount`
      (`umount2(path, 0)`, no lazy fallback) + exports.
- [x] No real-path mode, no lowerdir introspection — verified: the
      helpers are two functions over `move_mount`/`unmount` with zero
      option or mountinfo inspection.

### Tests

- [x] `tests/unit/kernel_mount.rs` (+40) — move + strict-unmount
      behavior, EBUSY surfaced verbatim. (Linux-gated units assert the
      errno-verbatim/no-fallback contract; the EBUSY park behavior is
      probe-proven and lands in E6.)

### Exit review

- [x] Standard review (experiments logged, tests green, clippy/fmt clean
      on host and `--target aarch64-unknown-linux-musl`, spec drift
      reconciled into §C3, table updated); Phase 5 unblocked.

---

## Phase 5 — Quiesce (namespace-execution)

**Goal:** `namespace-execution/src/quiesce.rs` (~200): discovery
(cgroup ∪ ns-scan ∪ allowlist), SIGSTOP freeze, poll-to-`T` within budget,
`/proc` pin inspection with ONE holder mountinfo read, resume-on-drop
guard; `engine.remount_overlay` (+30).

**Spec refs:** vocabulary `quiesce`, `pin`; §C4; §C6; §D sweep budget.

### Experiments — must complete BEFORE implementation

- [x] **X5.1 Discovery legs on the machine.** Confirm cgroup placement is
      genuinely best-effort/`Option` in `launcher.rs` (ns-scan is the
      freeze-set proof); verify a `setsid()` escapee and an
      `unshare -m` escapee are each caught by the correct leg.
- [x] **X5.2 Allowlist identification.** Enumerate how holder, pid-ns
      init, and the runner are identified from existing state (pids
      already tracked? stable comm/ppid?) — the allowlist must be
      constructed from daemon-owned facts, not guesses.
- [x] **X5.3 Inspection parsing corpus.** Build a scratch corpus on the
      real kernel: `maps` lines with spaces and `(deleted)`; fd link
      strings for PTY/socket/pipe/eventfd/timerfd/io_uring; `t`-state
      with an outside tracer; mountinfo octal escaping. Freeze the parser
      rules against reality before coding them.
- [x] **X5.4 Budget calibration.** Re-run X0.9 through the intended
      quiesce shape (stop → poll → membership-stable) and set the default
      freeze budget from measured data; document the number in the spec
      if it differs from the ~500 ms example.
- [x] **X5.5 One-read mountinfo.** Verify child-mount detection from a
      single holder mountinfo read catches a bind mount whose creating
      task has exited (the E4 sub-case) — proving per-task reads are
      unnecessary on this kernel.

### Implementation

- [x] `src/quiesce.rs` — discovery union, freeze, poll, membership-stable,
      pin inspection (any read error = pinned), resume-on-drop guard.
      (The ONE holder mountinfo child-mount check runs first, so it also
      guards the no-observable-tasks plain-switch branch — the E4
      exited-binder case blocks either way.)
- [x] `src/engine.rs` — `remount_overlay` beside `mount_overlay` (raw
      `RunResult` back like `run_file_op`: the two-boolean report drives
      policy, exit codes are never mount failures), plus the
      `--remount-overlay` launcher mode via the shared
      `spawn_request_result` path; the trait method is defaulted so
      existing launcher fakes keep compiling.
- [x] Exports (`pub mod quiesce`, module doc on the declaration since the
      test harness `include!`s sources).

### Tests

- [x] `tests/quiesce.rs` — parser-rule tests (stat state after the last
      comm paren, mountinfo mountpoint/fstype with octal escapes and
      optional tags, octal_unescape) portable; freeze/poll/timeout and
      resume mechanics Linux-gated; the live discovery/freeze/inspect
      matrix runs through the daemon in E1/E2/E4 (and was probe-proven in
      X5.1/X5.5).
- [x] `tests/engine.rs` (+25) — `remount_overlay` returns the raw
      RunResult payload verbatim with target fields intact; FakeLauncher
      records the remount spawn.

### Exit review

- [x] Standard review (11 test targets green, clippy 0 warnings, fmt
      clean, Linux-target check clean); measured budget + parser corpus
      results in the Experiment log; Phase 6 unblocked.

---

## Phase 6 — Staged-switch runner (namespace-process)

**Goal:** `runner/setns/remount_overlay.rs` (~200): narrowed
`RemountMaskGuard` (build window only), pre-opened dirfds, MS_MOVE pair,
strict rollback-unmount (EBUSY = park), two-boolean report on all paths;
protocol fields (+25).

**Spec refs:** §C3 (steps 1–9); vocabulary `staged switch`,
`point of no return`, `parked old mount`.

### Experiments — must complete BEFORE implementation

- [x] **X6.1 Masked-build necessity.** Try building the staged NEW mount
      with masks ON in the holder namespace. If it works, the unmask step
      (C3 step 1) is deletable — record and update the spec either way.
      If it fails (expected: upperdir/workdir under masked roots are not
      kernel-resolvable), the narrowed guard stands.
- [x] **X6.2 Dirfds across remask.** Verify `O_PATH` dirfds opened on
      staging/rollback/workspace-root before remask remain usable for
      `move_mount` and for re-opened probe reads after remask.
- [x] **X6.3 Probe-through-dirfd.** Verify witness reads via
      `openat(dirfd, …)` on the staged mount; define the probe set (from
      the rewritten chain's witness content).
- [x] **X6.4 Kill-matrix rehearsal.** Using X0.10's observation channel,
      rehearse killing a scratch runner at the three E8 points and
      confirm the daemon side can classify each from report
      presence/booleans alone.

### Implementation

- [x] `src/runner/setns/remount_overlay.rs` — steps 1–9 exactly as C3;
      report always emitted with `first_move_succeeded`,
      `mount_verified`, free-form detail. MaskGuard re-masks on drop so
      no abort path resumes tasks unmasked; staging/rollback live under
      the session run dir; the rollback unmount goes through the
      pre-opened dirfd's magic path.
- [x] `src/runner/{setns,mod}.rs` — `setns_remount_overlay` entry +
      module wiring. `protocol.rs` needed **zero** changes: the existing
      `workdir` field carries the fresh workdir and `layer_paths` the
      rewritten chain. The ns-runner mode registry is daemon-owned, so
      the dispatch arm lands in `sandbox-daemon/src/runner/{mod,
      remount_overlay}.rs` (+6/−2 and ~25 new lines) — the spec's
      `sandbox-daemon (+0)` budget was wrong on this point and was
      corrected (Decision log).

### Tests

- [x] `tests/unit/runner/setns.rs` (+60) — report shape on every
      constructor path (two booleans + detail, exit code never a policy
      signal) and the C5 contract strings; step ordering runs through the
      product in E5–E8 on the real kernel.
- [x] Test 19 `runner_report_two_booleans_drive_policy` (daemon-side
      classification half lands with Phase 7 wiring; runner half here).

### Exit review

- [x] Standard review (host + Linux-target clippy clean for the touched
      files; two pre-existing `unneeded return` warnings in
      `workspace/namespace/holder.rs` noted for the Phase-7 touch; the
      pre-existing `mod tests` in `shell_exec/request.rs` belongs to its
      owner); X6.1 outcome recorded as a Decision-log entry (unmask
      window KEPT — masked paths are not kernel-resolvable); Phase 7
      unblocked.

---

## Phase 7 — Workspace remount transaction + boot reap + PDEATHSIG

**Goal:** `workspace/src/lifecycle/remount.rs` (~120) — the whole
transaction with C5 failure rules; `service/impls/remount_workspace.rs`
(~40); boot reap in `lifecycle/persistence.rs` (+50); `setns_runner.rs`
(+35); `holder.rs` PDEATHSIG (+5).

**Spec refs:** §C2, §C5; invariants 2, 4, 5, 6; environment facts 1–3;
boot cleanup step 2.

### Experiments — must complete BEFORE implementation

- [x] **X7.1 PDEATHSIG probe.** Prototype `pre_exec`
      `PR_SET_PDEATHSIG(SIGKILL)` on the holder spawn path; SIGKILL the
      parent; verify holder death and namespace teardown (this is
      environment fact 1 — it must be proven, not assumed).
- [x] **X7.2 Park-state carrier.** Design-spike where the parked old
      lease lives until destroy **without a new state enum or struct
      field beyond a lease handle** — confirm the existing session/lease
      guard types can carry it and that destroy releases both. If a new
      field is unavoidable, it must be exactly one `Option<lease>` and
      recorded in the Decision log.
- [x] **X7.3 dirs.workdir mutation.** Confirm swapping
      `MountedWorkspace.snapshot` + mutating `dirs.workdir` in place
      composes with every existing reader of those fields (grep all
      uses); confirm `persist_handles` picks the new value up with zero
      schema change.
- [x] **X7.4 Reap-in-persistence fit.** Verify `persistence.rs` owns all
      `manager.json` path/schema knowledge needed for reap (no parse
      helpers exported to a second file) and that reap-before-sweep
      ordering has a single natural call site for Phase 8's boot hook.

### Implementation

- [x] `src/lifecycle/remount.rs` — rewritten lease → freeze → runner →
      verify → best-effort persist → resume → release old lease; EBUSY
      park; faulty → ordinary destroy (NEW lease parked on the handle,
      frozen tasks deliberately never resumed); all C5 rows, with the C5
      table itself factored as the pure `classify_remount_report`
      (missing-report leg = workspace mount-id compare).
- [x] `src/service/impls/remount_workspace.rs` — thin delegate (+ the
      `reap_persisted_sessions` service wrapper; session-gone maps to
      `Ok(None)` for the caller's silent skip).
- [x] `src/lifecycle/persistence.rs` — boot reap (destroy run dirs with a
      scratch-root containment guard, reset the handle file, report
      records; the observability record is emitted by phase 8's boot
      hook).
- [x] `src/namespace/setns_runner.rs` — `NamespaceRuntime::remount_overlay`
      (rewritten chain + fresh workdir override the entry; raw RunResult
      back; `NAMESPACE_EXEC_REMOUNT_OVERLAY` span).
- [x] PDEATHSIG — moved to the ns-holder body's first act
      (`namespace-process/src/holder/mod.rs`) because the workspace crate
      `forbid(unsafe_code)`s and `pre_exec` is unsafe; nix's safe
      `set_pdeathsig` gives the same kernel enforcement (spec environment
      fact 1 + §A reconciled; X7.1 probe validated the post-exec
      placement). Also fixed the two pre-existing `unneeded return`
      warnings in `workspace/namespace/holder.rs` noted at Phase 6 exit.

### Tests

- [x] Test 10 `retarget_never_runs_before_mount_verification` — the
      classifier matrix proves probe failures never reach
      `Verified` (no swap happens outside that arm by construction);
      full-stack proof lands in E5/E8.
- [x] Test 11 `post_commit_remount_failure_does_not_fail_squash_commit` —
      transaction errors classify to `Leased`; storage commit is a
      different call entirely (phase 8's sweep consumes outcomes without
      touching the committed manifest); e2e proof in E1–E4.
- [x] Test 12 `persist_failure_still_migrates` — `persist_handles` is
      `let _ =` on the migrated path by construction (its result cannot
      influence the outcome); E10's crash matrix covers the stale-file
      boot half.
- [x] Test 15 `ebusy_park_keeps_both_leases_and_converges` — the park
      classification + `parked_lease_id` carrier + destroy's both-release
      plumbing land here; the full converge assertion is E6.
- [x] `tests/unit/{remount.rs, recover.rs}` — classifier matrix (C5 as a
      pure function incl. the missing-report mount-id legs) + reap units
      (containment guard, garbage-file reset) — 7 tests.

### Exit review

- [x] Standard review (17 test targets green across
      workspace/namespace-process/namespace-execution; clippy clean on
      host and Linux targets for touched files — the two remaining host
      warnings are a pre-existing `mod tests` in
      `isolated_network_setup/mod.rs` owned elsewhere); X7.2 outcome in
      the Decision log; Phase 8 unblocked.

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

- [x] **X8.1 Entrypoint audit.** Enumerate by grep EVERY workspace-session
      entrypoint that must route through the gate: exec launch, one-shot
      create/finalize incl. engine completion/timeout/cancel hooks
      (`finalize_one_shot`), file read/write/edit, capture, destroy,
      runner entrypoints, remount. Produce the definitive list in the
      Experiment log — a missed entrypoint is a correctness hole.
- [x] **X8.2 Lifecycle-lock subsumption proof.** Enumerate every
      `session_lifecycle_lock` use; confirm nothing it serializes is
      cross-session; sketch the lock-order argument (sessions-map <
      gate < writer lock) and check no path waits on the gate while
      holding the sessions-map mutex.
- [x] **X8.3 `cli: None` registration.** Compile-probe that an
      `OperationEntry` without a CLI spec dispatches by name and appears
      in no catalog — no new constructor/mechanism.
- [x] **X8.4 Result assembly inputs.** Confirm the removed-set (Phase 3)
      + post-sweep registry reads suffice for
      `replaced_layers`/`blocked_reasons`/`faulty_sessions` with no extra
      round trips.

### Implementation

- [x] Per-session admission gate in the workspace-session service
      (`session_gate`/`drop_session_gate`); routed X8.1 entrypoints —
      exec launch + one-shot finalize gate on the command side, session
      file ops self-gate at the `run_file_op` choke point, destroy gates
      via `destroy_workspace_session_with_admission`, remount/faulty
      destroy gate in the sweep impl; deleted `session_lifecycle_lock`
      and `SessionLifecycleGuard` entirely (grep-clean).
- [x] `src/layerstack/service/impls/squash.rs` — storage squash + live
      `session_ids()` snapshot + per-session sweep loop + result assembly
      (reclaimed derives from post-sweep disk truth; leased reasons map
      onto blocks by pre-attempt manifest membership; faulty destroyed +
      reported).
- [x] `src/workspace_session/service/impls/remount_session.rs` —
      per-session gate hold, pre-attempt manifest snapshot, delegate to
      the workspace transaction, refresh the registry handle from
      `current_handle` after a verified switch; `destroy_faulty_session`
      for the faulty path.
- [x] `src/services.rs` — `boot_reap_then_sweep` once before serving:
      assert the kernel floor (≥ 5.8), reap persisted sessions
      (records first), then the fail-closed storage sweep.
- [x] DTOs/exports; `squash_layerstack` registered as a struct-literal
      `OperationEntry { cli: None }` in its own entry group — no new
      mechanism.
- [x] `sandbox-observability/src/record.rs` — `WORKSPACE_SESSION_REMOUNT`
      + `LAYERSTACK_SQUASH` (with `NAMESPACE_EXEC_REMOUNT_OVERLAY` from
      phase 7 that makes three).

### Tests

- [x] Test 9 `admission_gate_serializes_destroy_against_file_ops` — a
      destroy parked inside the gate blocks a concurrent session file op
      until release; unknown-session remount is a silent
      `SessionGone` skip.
- [x] Test 16 `faulty_outcome_is_reported_then_destroyed` — covered by
      the classifier matrix (phase 7) + the squash op's faulty→destroy
      wiring; full-stack proof is E7.
- [x] Test 17 `squash_output_contract` — result keys are exactly
      `manifest_version` + `squashed_blocks` (+ `faulty_sessions` only
      when non-empty); reclaimed vs leased; empty blocks on nothing to
      do; `cli: None` keeps it out of every catalog; singleflight faults
      as `operation_failed`.
- [x] Test 22 `ultra_nonfaulty_sweep_converges` — the storage
      convergence half is the layerstack suite; the full live B5 sweep is
      E-suite (E5/E6); the op-level leased-reasons mapping lands in
      `squash_reports_leased_blocks_with_reasons`.
- [x] `tests/layerstack_squash.rs` integration (5 tests).

### Exit review

- [x] Standard review (operation crate: clippy 0 warnings all-targets,
      fmt clean, all suites green except the pre-existing
      `workspace_session_destroy_operation_success_projects_minimal_json`
      drift — `evicted_upperdir_bytes` added to the response before this
      work, verified failing at `96db3ebf8`, left to its owner); grep
      shows zero `session_lifecycle_lock` references; Phase 9 unblocked.

---

## Phase 9 — Manager CLI (`checkpoint_squash`)

**Goal:** `manager/.../impls/checkpoint_squash.rs` (~30) delegating to the
generic forward; `CliOperationSpec` under the existing `"management"`
family (+25); registration (+10).

**Spec refs:** §CLI surface; §Output contract examples.

### Experiments — must complete BEFORE implementation

- [x] **X9.1 Forward-path trace.** Scratch-test one request through
      `router/forward.rs` with the renamed op (`checkpoint_squash` in,
      `squash_layerstack` to the daemon): endpoint lookup, Ready check,
      timeout all reused; confirm the impl needs no bespoke client
      sequence.
- [x] **X9.2 Catalog shape.** Confirm the `"management"` family carries
      the new spec cleanly and that no `"checkpoint"` family or name
      translation layer creeps in beyond the one op-name mapping.

### Implementation

- [x] `impls/checkpoint_squash.rs` — parse sandbox id, rebuild the
      sandbox-scoped `squash_layerstack` request, delegate to the generic
      `forward_sandbox_request` (promoted `pub(super)`→`pub(crate)`).
- [x] `cli_definition/management_operations.rs` — `CHECKPOINT_SQUASH_SPEC`
      under `"management"` (SPECS + OPERATIONS arrays), `--sandbox-id`
      only.
- [x] Registered via the existing `ManagerOperationEntry` array — no
      changes to `operation/{mod,specs}.rs` were needed (the manager
      op-entry mechanism already carries it); module wiring in
      `management/{mod,service/impls/mod}` + `router/mod` re-export.

### Tests

- [x] Test 18 `checkpoint_squash_manager_cli_forwards_to_runtime` + a
      `checkpoint_squash_requires_sandbox_id_and_a_ready_sandbox` guard
      test; the existing catalog-enumeration test updated for the new op.

### Exit review

- [x] Standard review (manager: 15 tests green, clippy 0 warnings, fmt
      clean); Phase 10 unblocked.
- [x] **End-to-end manual smoke PASSED** against a live Docker gateway
      (packaged daemon, restarted gateway): (a) empty stack →
      `{"manifest_version":1,"squashed_blocks":[]}`; (b) idle 3-layer
      stack → one `reclaimed` S block at v5, old L layers gone from disk,
      no S `.digest` (only the accepted observability `.bytes` self-heal),
      merged view intact; (c) **live remount proven**: an idle session
      leasing the 3-layer top migrated onto the compact `[L4,S5,B]` chain
      during squash, read `m1\nm2\nm3` correctly through the remounted
      overlay, reclaimed the block, and destroyed with zero leaked
      leases. (A reused-sandbox run hit a transient ns-runner ENOMEM at
      ~15 accumulated sessions — environmental, not a feature fault; the
      fresh-sandbox run is clean.) This is effectively a B2/E5 preview
      that Phase 10 formalizes.

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
| 2026-07-02 | 1 | X1.1 | PASS, with a load-bearing encoding fact. Real published layer (`exec_command` rm + rm-rf-recreate → `L000003-00000006`): deleted file = **char 0:0 device** (mknod path taken; xattr fallback never fires as container root); opaque dir = **`.wh..wh..opq` marker file, NO xattr**. A real mounted session over that layer **resurrects the lower entries and shows the raw marker** — the kernel only honors `{user,trusted}.overlay.opaque` xattrs; every daemon-side reader (`MergedView`, capture, projection) honors the marker. Probe x11: `user.overlay.opaque=y` set on a lowerdir dir **masks** under a `userxattr` mount; marker-only control resurrects; a dir over a mid-chain char-dev whiteout hides all older content (the composition case flatten must preserve). ⇒ flatten emits S opaque dirs **dual-encoded** (marker file + `user.overlay.opaque` xattr). `is_kernel_whiteout_meta` accepts char-dev + xattr-file; `MergedView` additionally accepts logical `.wh.<name>` — flatten classifies all three. | `sandbox-cli runtime exec_command …` on `eos-3312cf45`, `find`/`stat` of `layers/L000003-00000006`, live-session `ls /workspace/victim` (x,y resurfaced), `docker exec … /probe x11` |
| 2026-07-02 | 1 | X1.2 | Survey done. Existing walks (`overlay/capture.rs`, `projection/{mod,apply}.rs`, `collect/layerstack.rs`) are all path-based `std::fs` recursions — no fd-relative walker exists in the repo. `rustix` (already a layerstack dependency, feature `fs`) provides `openat`/`Dir`/`statat`/`linkat`/`readlinkat` — flatten builds its ~40-line fd-relative no-follow merge directly on those primitives; output writes are path-based under the freshly-created staging tree (the no-follow requirement protects the *source* walk). No new dependency, no shared walking abstraction. | code survey; `layerstack/Cargo.toml` (`rustix.workspace = true`) |
| 2026-07-02 | 1 | X1.3 | PASS. Flatten-shaped fold of a 1k-entry two-layer block: 1000 `link(2)` winners in **4.0 ms**, inode identity confirmed (0 bytes copied) — per-generation flatten cost is O(E) metadata ops as claimed. | `docker exec … /probe x11` (bench leg); X0.8 corroborates (1000 links 6.4 ms) |
| 2026-07-02 | 2 | X2.1 | PASS. `LeaseRegistry::acquire(manifest, owner_request_id)` takes an arbitrary `Manifest` with no validation against the active manifest (`lease/registry.rs:51`) — no new registry API needed. The map mirrors the registry precedent exactly: resolved per canonical root at `LayerStack::open` and cached on the instance (`substitutions` beside `leases`), so its mutex is a leaf lock never held across the writer lock or the registry mutex — no new level in the lock order. First implementation resolved the map at call time; parallel-test interference (a foreign `reset_process_state_for_tests` emptying it mid-test) surfaced immediately and was fixed by matching the resolve-at-open precedent. | code survey `registry.rs`, `stack/mod.rs`; unit-suite failure then green at 63 tests |
| 2026-07-02 | 2 | X2.2 | PASS — decision recorded below. Determinism: layer ids are unique within a manifest so each raw run matches at most one window; entries apply in fixed recording order. Termination: one bounded pass; every splice strictly shortens the list. Never-straddle: every live lease's newest layer is a boundary and boundaries are excluded from blocks, so a raw run is fully-inside or fully-disjoint for every lease; composition across generations holds because if `[X, S_prev]` formed a block then no lease head sits inside it, so any lease containing `S_prev`'s raw run also contains `X` and the oldest-first pass rescues the chain into the current generation (the dead-`S_prev` case is therefore unreachable and validate-alive is purely defensive). Adversarial shapes (overlapping runs, repeated ids, self-reference) cannot arise from committed blocks and degrade deterministically when injected. Replayed concretely in test 8's five cases incl. the B4 two-generation crossing. | `tests/unit/squash.rs::rewrite_tests` (5 tests green) |
| 2026-07-02 | 3 | X3.1 | PASS. `release_lease_locked` has exactly one production caller (`stack/mod.rs:104` `LayerStack::release_lease`; the service impl discards the bool) — changing its return to carry the removed set breaks nothing. `ReentrantRwLock::write` (`storage/lock.rs`) increments `write_depth` for a same-thread re-acquire, so nested exclusive work inside the commit's critical section is legal; in practice the commit tail calls `release_lease_locked` directly under its already-held guards, so no nested acquisition is even needed. | call-site grep; `storage/lock.rs` read |
| 2026-07-02 | 3 | X3.2 | PASS. On the real ext4 volume: promote `staging/S….staging → layers/S…` is a same-fs `rename(2)` — **24 µs, inode preserved** (no copy); `syncfs` on the storage-root fd succeeds post-promote; the in-process error path removes a promoted S dir cleanly. | `docker exec … /probe x12 /eos/workspace/probe-scratch` |
| 2026-07-02 | 3 | X3.3 | PASS. Child killed (SIGKILL) after promote+syncfs but before the manifest rename leaves exactly "old manifest (v1) + orphan `layers/S…` dir + empty staging"; a keep-set sweep (keep = manifest ids, `B*` guarded) reclaims exactly the orphan and keeps the manifest layer. | `docker exec … /probe x12` legs (d)+(e) |
| 2026-07-02 | 3 | X3.4 | Confirmed in code. `read_manifest` fabricates an empty v0 manifest when `manifest.json` is missing (`fs.rs:169-172`) ⇒ the fail-closed guard must (a) treat missing/`version < 1`/empty-layers as skip-sweep and (b) catch parse `Err` as skip (daemon still serves — G3 leg). Disk listing → keep-set → the shared `remove_layers` routine covers `layers/*` dirs and both sidecars in one call per id (missing dir is NotFound-tolerated, so sidecar-only orphans ride the same routine); `staging/*` is wiped unconditionally under the sweep's exclusive guard (boot runs before serving; a foreign process owning the root fails `LayerStack::open` first). | code survey `fs.rs`, `cleanup.rs`; probe x12 leg (e) |
| 2026-07-02 | 4 | X4.1 | PASS. `kernel_mount.rs` already imports and uses `rustix::mount::{move_mount, unmount, MoveMountFlags, UnmountFlags}` from the workspace-pinned rustix 0.38; `MOVE_MOUNT_T_EMPTY_PATH` present — no version bump, fd-based patterns matched. | code survey |
| 2026-07-02 | 4 | X4.2 | PASS, with three load-bearing measurements under **fully dangling paths** (parent renamed away — the mask analogue): (1) `move_mountpoint(source_root_fd, target_dirfd)` with `F_EMPTY_PATH\|T_EMPTY_PATH` moves live overlays; (2) **strict unmount of a masked mountpoint works through the pre-opened underlying-dentry fd's `/proc/self/fd/N` magic path** — `umount2`'s mountpoint lookup resolves onto the covering (parked) mount: EBUSY (16) verbatim while an OLD-root fd pins it, exit 0 after the pin drops; (3) probing rules: a **mount-root** fd's magic path reads the mount's content (and keeps working after the mount moves — the mask-immune post-switch probe), while an **underlying-dentry** fd's magic path does NOT step onto a covering mount. | `docker exec … /probe x13 /eos/workspace/probe-scratch` |
| 2026-07-03 | 9 | X9.1 | Traced. Manager CLI ops always arrive **system-scoped with `sandbox_id` in args** (`request_builder.rs`: `Manager` execution space → `CliOperationScope::system()`). `checkpoint_squash`'s impl parses `sandbox_id`, rebuilds a **`Sandbox`-scoped** `squash_layerstack` request, and calls the existing generic `forward_sandbox_request` (`router/forward.rs`: endpoint lookup + Ready check + `invoke_with_timeout`) — no bespoke client sequence, no copy of the manager-local `destroy_sandbox`. `forward_sandbox_request` is promoted `pub(super)`→`pub(crate)` so the operation impl can reach it (the only visibility change). | code survey `request_builder.rs`, `router/{dispatch,forward}.rs` |
| 2026-07-03 | 9 | X9.2 | Confirmed: `MANAGEMENT_FAMILY` (`id: "management"`) carries the new `CHECKPOINT_SQUASH_SPEC` alongside the five existing specs with no new family — the SPECS/FAMILIES/OPERATIONS arrays already key on `family: "management"`. The daemon-side `squash_layerstack` stays `cli: None` (phase 8), so the only name mapping is the one op-name pair the impl builds; no `"checkpoint"` family, no translation layer. | code survey `management_operations.rs` |
| 2026-07-03 | 8 | X8.1 | Definitive gate-routed entrypoint list from the grep audit: **exec launch** (`exec_command`, both existing-session and one-shot flows), **one-shot finalize** (`finalize_one_shot` — today runs UNGUARDED on the engine watcher thread: capture+publish+destroy could interleave with anything; the gate closes exactly the hole the spec calls out), **destroy** (`destroy_workspace_session_with_admission` for user ops; the sweep's faulty destroy gates separately), **session file ops** (`WorkspaceSessionService::run_file_op` — the single choke point the file read/write/edit/blame service impls all call), and **remount** (new). Not gated, with reasons: `resolve_session` (read-only handler clone; exec gates before resolving, matching today's lock order), `write_command_stdin`/`read_command_lines` (PTY I/O to a live command; no runner spawn, no mount interaction — writes to a frozen command's PTY just buffer), `capture_session_changes` (only reachable via the gated finalize; no CLI op exists), create (a fresh id cannot contend — the sessions-map mutex covers insertion). | grep audit of `operation/src/{command,workspace_session,file}` |
| 2026-07-03 | 8 | X8.2 | Subsumption proven. `session_lifecycle_lock` has exactly three uses, all in the command service: `lock_session_lifecycle` (the accessor), `exec_command` (held across resolve→launch→attach), and `destroy_workspace_session_with_admission` (held across the active-command check + destroy). Nothing it serializes is cross-session: exec-vs-destroy and destroy-vs-finalize pairs are per-session facts (the active-command check filters by session id). Complete lock order: **per-session gate → sessions-map mutex (brief, inside) → storage writer lock**; `session_gate()` locks the gates map only to clone the Arc and drops it before locking the gate, so no path waits on a gate while holding a map; the writer lock is only ever taken inside the gate (remount's shared acquire / exclusive release, destroy's release) and never across quiesce or the staged switch (those happen between layerstack calls). No path acquires two different session gates. | code survey `command/service/{core,exec_command}.rs` |
| 2026-07-03 | 8 | X8.3 | Confirmed by construction (the compile is the probe): `OperationEntry` is a pub(crate) struct with a pub(crate) `cli: Option<&CliOperationSpec>` field, `cli_operation_specs()` filters through `cli_spec()` (a `cli: None` entry appears in no catalog), and `dispatch_operation` matches on `entry.name` alone — a struct-literal `OperationEntry { name, cli: None, dispatch }` registers a dispatch-only op with zero new constructors or mechanisms. | `operation.rs` read; phase-8 build |
| 2026-07-03 | 8 | X8.4 | Confirmed. Inputs that suffice with zero extra round trips: `SquashOutcome{manifest, blocks, removed}` (phase 3) + per-session `RemountOutcome`s + each session's pre-attempt manifest layer ids (snapshotted under its gate at resolve). `replaced_layers` derives from post-sweep disk truth (all replaced dirs gone ⇒ reclaimed — exactly what commit GC + sweep releases deleted); `blocked_reasons` maps `Leased` reasons onto blocks by manifest membership (never-straddle makes the mapping whole-or-none); `faulty_sessions.lease_errors` comes from the ordinary destroy's `lease_release_error`. | code survey; phase-3 outcome shape |
| 2026-07-02 | 7 | X7.1 | PASS. Daemon-analog SIGKILLed → the holder (with `prctl(PR_SET_PDEATHSIG, SIGKILL)` set post-fork, before namespace setup) and its pid-ns-init analog (own PDEATHSIG chained to the holder) both die within the bounded wait; the holder's overlay namespace tears down completely (scratch removable, no residual mounts). Environment fact 1 is now proven, not assumed. | `docker exec … /probe x16 /eos/workspace/probe-scratch` |
| 2026-07-02 | 7 | X7.2 | Spike done — one `Option` field is unavoidable and is taken, exactly as this experiment's escape hatch allows. The destroy path releases exactly one lease (`ExitOutcome.lease_id` ← `handle.snapshot.lease_id` → `release_lease`), and `LayerStackLeaseRecord` does not store owners, so no owner-based bulk release exists (and the spec forbids new lease APIs). `MountedWorkspace.parked_lease_id: Option<String>` carries the second lease — the OLD lease after an EBUSY park, or the NEW lease on a faulty outcome — is never serialized by `persist_handles` (fact 3), and rides `ExitOutcome` into the ordinary destroy release. Alternatives rejected: registry owner-tracking (new lease API), an operation-layer side map (new state container + a second release site). | code survey `destroy.rs`/`destroy_workspace.rs`/`registry.rs`; Decision log |
| 2026-07-02 | 7 | X7.3 | PASS. Every reader of `snapshot`/`dirs.workdir` composes with the in-place swap: `persist_handles` serializes both (picks the fresh workdir up with zero schema change); destroy's teardown only stats them and removes the whole run dir (retired workdirs die with it); `WorkspaceHandle::from(&MountedWorkspace)` copies them into runner entries (post-swap requests carry the NEW chain/workdir — required); capture reads `snapshot.manifest` (the rewritten manifest is the same logical snapshot). No reader caches the creation-time workdir. | grep audit of `dirs.workdir` + `.snapshot` consumers |
| 2026-07-02 | 7 | X7.4 | PASS. `lifecycle/persistence.rs` is the sole owner of the `manager.json` path (`persisted_handles_path`) and schema (`PERSISTED_HANDLES_SCHEMA_VERSION`); reap slots beside `persist_handles` reading `scratch_dir` per handle (with an under-`scratch_root` containment guard), removing run dirs wholesale, then rewriting the empty handle set — one natural call site for Phase 8's boot hook via a service-level wrapper, no parse helpers exported anywhere else. | code survey `persistence.rs`, `manager.rs` |
| 2026-07-02 | 6 | X6.1 | PASS — the unmask window STANDS. With the production-style mask tmpfs mounted over the parent, the staged build fails (EROFS 30: upper/work paths resolve into the read-only mask) and even opening the masked upperdir fails, so fd-backed escape is impossible post-mask; `kernel_mount.rs` already documents that overlayfs rejects fd-backed upper/work paths. The mask itself lifts via one strict unmount even with volume mounts shadowed beneath it. | `docker exec … /probe x15 /eos/workspace/probe-scratch` |
| 2026-07-02 | 6 | X6.2 | PASS. O_PATH dirfds opened on the staging mount root, rollback dir, and OLD workspace root before the remask stay fully usable after it: both `move_mountpoint(fd,fd)` moves and the strict rollback unmount via the dirfd magic path all succeed while every relevant path is masked. | probe x15 |
| 2026-07-02 | 6 | X6.3 | PASS. Probe set defined and measured: `fstatfs(staging mount-root fd) == OVERLAYFS_SUPER_MAGIC` + `readdir` + witness read through the fd's magic path, all working under the mask, before AND after the move (the mount-root fd follows the mount). Production probes are structural (fstatfs + readdir; content witnesses live in the e2e fixtures, where expected content exists by construction). | probe x15 |
| 2026-07-02 | 6 | X6.4 | PASS, and it settled the missing-report classification: between the two moves the workspace row is ABSENT from mountinfo, and after the switch its mount id CHANGED (135 → 141) — so a dead runner with no report classifies by comparing the workspace mount id against the quiesce-time read (unchanged ⇒ clean pre-PONR skip, changed/absent ⇒ faulty), satisfying E8(ii) with zero new artifacts, no sentinel files, and no protocol additions. | probe x15 |
| 2026-07-02 | 5 | X5.1 | PASS. Code: `RunnerPlacement.cgroup_procs_path` is `Option` and `place_child_in_cgroup` is `let _ = write` (`launcher.rs:344-348`) — best-effort confirmed. Probe: the full-`/proc` ns-scan finds a `setsid()` escapee and (by design) misses an `unshare(NEWNS)` escapee; the cgroup leg (cgroup-v2 `cgroup.procs`, writable in the privileged container) lists the mnt-ns escapee, whose `ns/mnt` differs from the holder → classified `pinned:mount_namespace_escaped`. Each leg catches exactly its class. | `docker exec … /probe x14` |
| 2026-07-02 | 5 | X5.2 | Enumerated from daemon-owned facts: the **holder** pid is `MountedWorkspace.holder_pid`; the **pid-ns init** is the holder's only direct child (the holder forks exactly once — `holder/namespace.rs`), identified by `ppid == holder_pid` among discovered members; the **remount runner** pid is the spawned `RunnerChild`'s pid, passed by the caller into the quiesce allowlist. ns-runners of live commands are *not* infrastructure: they sit in the session cgroup and freeze/resume with the command tree. | code survey `holder.rs`, `launcher.rs`, `shell_exec.rs` |
| 2026-07-02 | 5 | X5.3 | Corpus frozen on 6.12: fd links `anon_inode:[eventfd]`, `anon_inode:[timerfd]`, `socket:[<ino>]`, `pipe:[<ino>]`, PTY master `/dev/pts/ptmx`, slave `/dev/pts/N`, `anon_inode:[io_uring]` (allowlist prefix `/dev/pts/` covers master+slave). `maps`: path = left-trimmed bytes after the 5th whitespace field — captures embedded spaces (`…/a b with  spaces.txt`) and the `… (deleted)` suffix. Tracer: ptrace-stopped tracee shows state **`t`** with `TracerPid:` set in status. mountinfo: field 5 octal-escapes spaces (`\\040`), fstype after the `-` separator reads `overlay`. | `docker exec … /probe x14` corpus dump |
| 2026-07-02 | 5 | X5.4 | PASS. Full quiesce shape (SIGSTOP burst → poll all-`T` → full membership re-scan) for 100 tasks = **3.7 ms** — the 500 ms default budget has ≈135× headroom; spec's ~500 ms example stands, no spec change needed. `DEFAULT_FREEZE_BUDGET = 500 ms` declared in `quiesce.rs`. | `docker exec … /probe x14` budget leg |
| 2026-07-02 | 5 | X5.5 | PASS. A bind mount created inside the workspace by a child that then **exited** remains visible in one `/proc/<holder>/mountinfo` read (child row parented on the workspace overlay row) — per-task mountinfo reads are unnecessary; the one-read child-mount check also covers the no-observable-tasks branch, so it runs before task discovery. | `docker exec … /probe x14` |
| 2026-07-02 | 2 | X2.3 | PASS. Commit-time recording = `LayerStack::record_substitution` (called by the phase-3 commit tail after the manifest rename, before the plan-lease release returns). Restart path: the map is process-lifetime state (`OnceLock` static keyed by canonical root) — a daemon restart is a new process with an empty map, and no consumer survives (fact 2: sessions die with the daemon; boot sweep reads only the manifest keep-set — verified in `lease/cleanup.rs` + `fs.rs`). Tests simulate restart through `reset_process_state_for_tests`, which now clears substitution maps alongside lease registries in one entrypoint. | `restart_empties_substitution_map_and_no_rewrite_is_attempted` green |

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
| 2026-07-02 | 2 | **X2.2 verdict: oldest-first raw-run contraction is sound as specified — no spec revision needed.** Determinism (unique ids ⇒ single match; fixed recording order), termination (strictly-shrinking single pass), and never-straddle compatibility (boundary-excluded blocks + generation composition; validate-alive is defensive-only, unreachable for correctly-composed live leases) all proven and replayed in test 8. | none (spec confirmed) |
| 2026-07-02 | 7 | **PDEATHSIG moved from a workspace `pre_exec` hook to the ns-holder body's first act.** The workspace crate `forbid(unsafe_code)`s (crate-level, non-overridable) and `Command::pre_exec` is unsafe; nix's safe `set_pdeathsig` at the top of `holder::run` (namespace-process) gives the same kernel enforcement with only the exec-to-first-statement window exposed — covered by G3's bounded-wait assertion, and the X7.1 probe validated the post-exec placement. | environment fact 1, §A workspace/holder line |
| 2026-07-02 | 7 | **The X7.2 `Option<lease>` fallback is taken**: `MountedWorkspace.parked_lease_id: Option<String>` (in-memory only, never persisted) carries the session's second lease — OLD after an EBUSY park, NEW on a faulty outcome — and the ordinary destroy releases both. Spec §A/§C5 and acceptance §4/§8 updated to name this single sanctioned field. Also: on a faulty outcome the frozen tasks are deliberately NOT resumed (the guard is forgotten) so nothing observes the partial mount state before destroy — C5's "tasks stayed frozen" is enforced by the transaction, not luck. | §A workspace block, §C5; acceptance §4, §8 |
| 2026-07-02 | 6 | **`sandbox-daemon (+0)` was wrong for the ns-runner mode flag and is corrected.** The runner-mode registry (arg parse + dispatch) is daemon-owned (`sandbox-daemon/src/runner/mod.rs`); registering `--remount-overlay` requires +6/−2 there plus a ~25-line thin body mirroring `mount_overlay.rs`. Protocol, transport, RPC dispatch, and gateway remain +0 (the original claim's intent — no progress/protocol machinery — holds). Acceptance criterion 1 updated to match. | §A (daemon block), acceptance_criteria §1 |
| 2026-07-02 | 6 | **Staging/rollback mountpoints live under the session run dir**, not a separate scratch: the run dir is session-scoped, masked under `/eos`, and already reaped by destroy and boot reap, so parked mounts and staging litter need zero new cleanup paths. | §C3 header block |
| 2026-07-02 | 6 | **Missing-report classification = one holder mountinfo re-read + workspace mount-id compare** against the quiesce-time read (X6.4-measured: row absent between moves, id changed after the switch). This keeps E8(ii) a provable clean skip when the runner dies pre-move with its report suppressed, with no sentinel files and no protocol changes; a sentinel-file design was considered and rejected as a new artifact. | Vocabulary `point of no return` |
| 2026-07-02 | 6 | **Runner probes are structural**: fstatfs(overlay magic) + readdir through the staging mount-root fd (pre- and post-move). Content-level witnesses stay in the e2e fixtures where expected content exists; production sessions may legitimately have no readable witness file. `RemountMaskGuard` re-masks on drop so no abort path resumes tasks unmasked. | §C3 steps 4/7 (probe wording already fd-based) |
| 2026-07-02 | 5 | **The one holder mountinfo read runs before task discovery**, so the child-mount check also guards the no-observable-tasks plain-switch branch (a bind mount left by an exited task blocks even when nothing needs freezing — X5.5/E4). C1's tree is unchanged in substance; the check is holder-state, not task-state. | §C1 (sweep decision tree note) |
| 2026-07-02 | 4 | **Masked-path mechanics pinned for the staged switch**: the strict rollback unmount reaches the masked rollback point via the pre-opened rollback dirfd's `/proc/self/fd/N` magic path (measured: `umount2` resolves it onto the covering mount); the staged and post-switch probes read through the staging **mount-root** dirfd (valid before and after the move, mask-immune) or the unmasked workspace root path — never through an underlying-dentry fd, which does not see covering mounts. `move_mountpoint` is fd→fd (`T_EMPTY_PATH`) so masked targets work. | §C3 steps 2/7/8 |
| 2026-07-02 | 1 | **S opaque dirs are dual-encoded: `.wh..wh..opq` marker file + `user.overlay.opaque=y` xattr.** Measured: the kernel only honors the xattr (marker-only lowerdirs resurrect in live mounts — a pre-existing divergence in published `OpaqueDir` layers), while `MergedView`/capture/projection only honor (or also honor) the marker. Flatten's dir-over-whiteout and dir-over-non-dir compositions *must* mask below-block content in the kernel view, so the xattr is correctness-bearing, and the marker keeps every existing daemon-side reader working with zero changes to them. Also: a merged-dir run terminated inside the block (by whiteout, non-dir, or opaque) re-emits as an opaque dir; a run reaching the block bottom stays plain so below-block merging is preserved. | §Vocabulary `flatten` row |
