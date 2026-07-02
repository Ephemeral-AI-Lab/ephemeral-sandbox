---
title: LayerStack Squash + Live Remount — Acceptance Criteria
tags:
  - ephemeral-os
  - layerstack
  - acceptance
status: acceptance_criteria
updated: 2026-07-02
---

# Acceptance Criteria

Feature is accepted when **every criterion below is checked**, with the
listed verification. Criteria are derived from `spec.md` (post
simplicity-review revision); test numbers refer to the spec's "Required
tests" section (unit 1–22, gates G1–G3, features E1–E10).

## 1. Scope and non-goals (must hold, verified by inspection)

- [ ] There is **no auto-squash trigger policy**: squash runs only on
      explicit `checkpoint_squash` invocation; the command takes zero
      options beyond `--sandbox-id`. *Verify: CLI catalog + grep for any
      trigger/policy config.*
- [ ] **No durable remount state exists**: no substitution ledger on disk,
      no remount state enum, no quarantine mechanism, no persisted sweep
      state. *Verify: grep for `.sources.json`/remount-state writes; test 8;
      test 22.*
- [ ] **No progress streaming**: `sandbox-protocol`, `sandbox-daemon`,
      `sandbox-gateway` have a zero-line diff for this feature. *Verify:
      `git diff --stat` on those crates.*
- [ ] Live remount is **never part of storage-commit correctness**: any
      remount failure before the point of no return leaves a committed
      squash intact. *Verify: test 11.*

## 2. Storage commit (squash)

- [ ] **Merged-view equivalence**: for every path, the post-squash merged
      view (including rewritten lease chains) equals the pre-squash view —
      whiteout encodings, opaque markers, dir-created-then-emptied, file
      modes, hardlinked whole-file winners. *Verify: test 2; G2; E5 witness
      reads.*
- [ ] Squash squashes **every squashable block** (maximal runs ≥ 2 between
      boundaries); boundaries come from `lease_newest_layers()` under the
      plan lock; singletons and `B*` are never touched. *Verify: test 1.*
- [ ] **Commit is atomic at the manifest rename**; the commit sequence is
      recheck → promote → one `syncfs` → manifest rename → plan-lease
      release. No layer any live lease references is ever mutated or
      deleted. *Verify: tests 3, 6, 7, 13, 20.*
- [ ] **Commit-time GC is literally `release_lease` on the plan lease**
      (returning the removed set); no second deletion routine exists in
      squash. Reclaimed-vs-leased is decided by the registry at commit
      instant, never a plan-time snapshot. *Verify: test 20; test 3.*
- [ ] **Singleflight per root**: concurrent invocations wait or fail
      cleanly; racing publishes never starve or conflict the commit
      (run-presence recheck); a broken run aborts as `operation_failed` —
      no `manifest_conflict` kind exists in the wire contract. *Verify:
      tests 4, 5.*
- [ ] **S layers carry zero sidecars** (no `.digest`, no `.bytes`, no
      ledger); publish dedup and observability tolerate that (dedup-miss
      silent, bytes self-heal by walking). *Verify: test 21.*
- [ ] **Crash safety**: a crash at any commit point leaves either the old
      or the new manifest fully valid; boot sweep reclaims orphan staging
      and orphan S dirs; a non-crash post-promote failure cleans up
      in-process. *Verify: tests 6, 14; E10.*
- [ ] Durability holds with exactly **one `syncfs`** before the manifest
      rename (no per-entry fsync walk); simulated power-fail after commit
      leaves all S content, whiteouts, and symlinks intact. *Verify:
      test 7 (syscall-recording shim).*

## 3. Substitution and rewrite

- [ ] The substitution map is **in-memory, per-root**, recorded at commit,
      dies with the daemon; rewrite is oldest-generation-first contraction
      of raw runs; generation-crossing shapes (B4's `Sc→[L8,Sa]`) produce
      the spec's chains; a missing entry degrades to identity — never a
      wrong chain, never a hang. *Verify: test 8.*
- [ ] `acquire_rewritten_lease` is one call under one shared writer-lock
      guard, validates every rewritten layer alive before acquiring, and
      never releases the old lease. Identity short-circuits before any
      freeze. *Verify: tests 8, 10; test 22.*
- [ ] **Pin-overlap** holds: no instant exists where either chain is
      unpinned; clean aborts release only the replacement lease (none
      leaked, registry count returns to baseline). *Verify: tests 9, 10;
      E4 teardown assertions.*

## 4. Live remount

- [ ] **Gated, not assumed**: if any gate (G1 kernel proof, G2 parity, G3
      startup cleanup) is not proven in the supported environment, squash
      still commits and every session reports `leased(unsupported:…)`.
      *Verify: G1 failure leg.*
- [ ] A session running a clean batch command (no workspace pins)
      **migrates live**: the command never errors, pre-freeze upperdir
      writes remain visible, absolute lookups land on the new mount, the
      chain shortens, old layers reclaim within the invocation. *Verify:
      E5; test 22.*
- [ ] **Every pin class blocks cleanly**: cwd, root, fd, mmap (paths with
      spaces, `(deleted)`), child mount (detected from ONE holder
      mountinfo read — zero per-task mountinfo reads), mount-namespace
      escape, io_uring/unknown anon fd, outside tracer, any `/proc` read
      error, membership churn, freeze timeout. Each yields
      `leased(class:detail)`, resumes the session, and leaves the old
      lease intact. *Verify: E1, E2, E4.*
- [ ] **Masks are never observable** by any resumable task; mask-restore
      failure is a clean **pre-PONR** skip (remask happens before the
      first move; moves go through pre-opened dirfds). *Verify: E3.*
- [ ] **EBUSY park**: strict-unmount EBUSY after a verified switch resumes
      the session on NEW holding both leases (released at destroy),
      reports `leased(pinned:rollback_unmount_busy)`, and the next squash
      sees Identity — no repeated freeze/switch, no restore ladder.
      *Verify: test 15; E6.*
- [ ] **Faulty is narrow and total**: any other post-PONR failure —
      including a missing/ambiguous runner report at/past
      first-move-success — reports the session in `faulty_sessions`
      (session id, `class:detail`, lease errors; no byte totals) and
      destroys it through the ordinary destroy path; leases release only
      after namespace death; tasks stayed frozen so no partial state was
      ever observable. *Verify: tests 16, 19; E7, E8.*
- [ ] The runner report is **two booleans + free-form detail**
      (`first_move_succeeded`, `mount_verified`), present on all paths;
      every C5 outcome is a pure function of them plus report presence.
      *Verify: test 19; E8.*
- [ ] The **admission gate is the single serializer**: exec launch,
      one-shot create/finalize (including timeout/cancel completion hooks
      firing mid-switch), file ops, capture, destroy, runner entrypoints,
      and remount all route through the per-session gate; the global
      `session_lifecycle_lock` no longer exists; no deadlock under
      concurrent load. *Verify: test 9; grep for `session_lifecycle_lock`.*
- [ ] Remount writes **nothing required** to disk: the only write is a
      best-effort `persist_handles()` whose failure still reports
      `migrated` (the fresh workdir rides the existing `dirs.workdir`
      field — no schema change, no new session-state fields). *Verify:
      test 12.*

## 5. Boot cleanup and recovery

- [ ] Boot runs **reap-then-sweep once before serving**: every persisted
      handle is treated as a dead session (PDEATHSIG makes holders
      provably dead); reap records precede any sweep deletion; sweep keeps
      exactly the active manifest's layers/sidecars. *Verify: test 14; G3;
      E10.*
- [ ] Sweep is **fail-closed**: missing/unparsable manifest ⇒ nothing
      deleted, `B*` never deleted; no mount-boundary detector exists.
      *Verify: test 14; G3 unreadable-manifest leg.*
- [ ] Boot sweep and lease-release GC share **one deletion routine**; lease
      GC removes `.digest` + `.bytes` with the layer dir (the pre-existing
      `.bytes` leak is fixed and regression-tested). *Verify: test 14.*
- [ ] Daemon killed at **any** point (mid-freeze, mid-switch, pre-release,
      pre-rename) recovers with no remount-specific branch: holder and
      pid-ns init die with the daemon, restart reaps and sweeps, no
      session state resurrects, a fresh session + squash succeed.
      *Verify: E10.*

## 6. CLI and product surface

- [ ] Surface is exactly
      `sandbox-cli manager checkpoint_squash --sandbox-id SANDBOX_ID`
      under the existing `"management"` family; the impl delegates to the
      generic `router/forward.rs` path; `squash_layerstack` registers with
      `cli: None` and appears in **no** CLI catalog; no
      `OperationEntry::internal` mechanism exists. *Verify: test 18.*
- [ ] Result JSON is exactly `manifest_version` +
      `squashed_blocks{squashed_layer_id, replaced_layer_ids,
      replaced_layers, blocked_reasons}` + `faulty_sessions` (omitted when
      empty). No `layers`, no `leases`, no `no_op`, no byte totals.
      `blocked_reasons` is non-empty whenever `leased`; its strings are
      free-form diagnostics. *Verify: test 17.*
- [ ] All faults are one stderr `{"error":…}` line, exit 1, kind
      `operation_failed`; success is one stdout JSON line, exit 0 —
      including runs with faulty sessions (the squash committed).
      *Verify: tests 4, 17; E7.*
- [ ] Exactly three observability records exist (`LAYERSTACK_SQUASH`,
      `WORKSPACE_SESSION_REMOUNT`, `NAMESPACE_EXEC_REMOUNT_OVERLAY`),
      following the existing per-subsystem span grammar. *Verify: record.rs
      diff.*

## 7. Performance and space

- [ ] Commit issues **≈3 durability syscalls** total (one `syncfs` + the
      manifest `write_atomic` pair), independent of entry count — not
      O(entries). *Verify: test 7 shim counts.*
- [ ] Flatten hardlinks whole-file winners: per-generation cost is O(E)
      metadata ops; bytes are copied only for re-encoded content
      (whiteouts, opaques, partial trees). *Verify: Phase 1 experiment
      benchmark + test 2 hardlink assertions.*
- [ ] The freeze stall on live sessions is bounded by the freeze budget
      and paid **only** on explicit invocations; a D-state straggler
      times out as `quiesce_failed:freeze_timeout` and the sweep proceeds.
      *Verify: Phase 5 experiment measurements; E4.*
- [ ] Peak temporary storage is one builder's staging (singleflight);
      after the sweep, disk for a fully-migrated stack drops to
      Θ(B + F + U + pins/singletons + publish tail). *Verify: test 22
      disk assertions; E5.*
- [ ] Lease-GC membership checks are set-based (no `Vec::contains` scans
      inside the writer lock). *Verify: code review of cleanup.rs.*
- [ ] Chain-length behavior: creation and staged remount share the single
      `OVL_MAX_STACK` = 500 cap; an over-limit staged mount is a clean
      pre-PONR skip derived from the mount errno. *Verify: E9.*

## 8. Code quality and boundaries

- [ ] `cargo test` (workspace), `cargo clippy --all-targets`, `cargo fmt`
      all clean; no new `unwrap_used`/`dbg_macro` warnings; any `unsafe`
      carries `// SAFETY:`.
- [ ] No test code in `src/`; no inline comments in production code; all
      fault injection lives in `tests/` shims or external process control
      — grep confirms zero in-src test flags.
- [ ] Crate boundaries per `README.md` hold: protocol vocabulary in
      `sandbox-protocol`, dispatch in `sandbox-daemon`, operation specs in
      `sandbox-runtime/operation`; the YAML fence and workspace-dependency
      conventions are untouched.
- [ ] `stack/ops/publish.rs` has a **zero diff** (test:
      `publish_path_untouched_by_squash` equivalent — verify via
      `git diff`).
- [ ] Final file/LoC shape matches spec §A within reason: 10 new source
      files, no `stack/sweep.rs`, no `lifecycle/recover.rs`, no standalone
      `squash/rewrite.rs`, no new `MountedWorkspace` fields.

## 9. Sign-off

- [ ] All 22 unit/integration tests green in CI.
- [ ] Gates G1–G3 green in the supported Docker environment; live remount
      enabled only after all three pass (otherwise squash ships
      commit-only and this is recorded).
- [ ] Feature tests E1–E10 green in the supported Docker environment,
      including every teardown assertion (empty lease registry, no
      staging/rollback mounts, empty `staging/`).
- [ ] `impl_plan_and_progress_tracker.md` shows every phase's exit review
      complete; experiment and decision logs are filled in.
- [ ] `spec.md` matches the shipped behavior (any deviation was folded
      back into the spec before sign-off).
