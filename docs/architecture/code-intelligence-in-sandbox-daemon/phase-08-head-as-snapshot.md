# Phase 8 — Snapshot redesign: lowerdir is the live codebase; upperdir is the only carrier of changes

**Type:** **Architectural correctness fix.** The original `git add -A` snapshot construction was the wrong abstraction. The lowerdir of the overlay is already a complete, content-stable representation of the workspace at command-start; constructing a separate git tree as the "snapshot" both filtered out content that should have been in scope (gitignored deps) and duplicated state that the overlay primitive was already maintaining for free.
**Estimated effort:** Completed in the lowerdir-read path; no CoW snapshot mechanism was needed.
**Risk profile:** MEDIUM — replaces the snapshot abstraction; OCC's per-path base reads are reshaped from `git show <snap>:path` to filesystem reads against the lowerdir.
**Status:** Implemented. See `phase-08-implementation-report.md`.
**Blocks on:** Satisfied by Phase 6 daemon-local fold and the Phase 8 probe conclusions recorded in `phase-08-implementation-report.md`.

## Why the original `git add -A` snapshot was wrong

Two reasons, neither of them about performance.

**1. It filtered the snapshot to the gitincluded subset of the workspace.**
The lowerdir of the per-command overlay is the live main layer of the entire codebase: tracked, untracked, gitincluded, gitignored — `.venv`, `node_modules`, build artifacts, everything. The `git add -A` snapshot dropped every gitignored path before producing the tree. That tree is therefore not a snapshot of the workspace; it's a snapshot of the workspace's gitincluded subset.

This is incorrect by abstraction. OCC consumers presume the snapshot represents pre-command state, but it only represents a filtered slice. Concurrent gitignored writes are invisible to the snapshot and therefore to OCC. Cross-cutting consistency between gitincluded and gitignored content (e.g., a `pyproject.toml` change paired with a `.venv` rebuild) cannot be reasoned about from the snapshot. The system has been carrying a partial-truth snapshot and calling it complete.

**2. It re-encoded state the overlay primitive already provided.**
The lowerdir is already content-stable for the lifetime of the command — overlayfs guarantees lower is read-only from the merged view; user writes land in upper. The lowerdir at command-start *is* the snapshot. Walking it with `git add -A` and re-encoding it as a tree object pays ~0.25s/call to produce an artifact whose only purpose is to give OCC a "pre-command base" — but the lowerdir already serves that purpose, with `read(<lowerdir>/path)` returning the pre-command content of any path in O(1).

The git encoding only earned its keep if you accepted (1)'s filtering as correct. Without that acceptance, it's redundant work over an already-stable representation.

## The corrected model

The contract Phase 8 establishes:

- **lowerdir = the live main layer of the entire codebase.**
  At the start of every svc.cmd, the lowerdir contains the workspace's full state — every tracked file, every untracked file, every gitignored dependency tree, every cache. The lowerdir is not filtered. It is the workspace.
- **upperdir = the only carrier of per-command updates, writes, deletions, and changes.**
  Anything the user command produces — file creates, modifications, whiteouts (deletions) — lands exclusively in the upperdir. The lowerdir is untouched by the command. Walking the upperdir at command-end yields the complete delta, with no other place to look.
- **Snapshot at command-start = the lowerdir at command-start.**
  No tree construction, no `git add -A`, no separate artifact. OCC's per-path base read becomes `read(<lowerdir>/path)`. The snapshot is already there because the overlay put it there.
- **Gitignore is a routing rule for upperdir merges, not a snapshot filter.**
  Gitincluded upperdir entries → OCC merge against lowerdir base content. Gitignored upperdir entries → direct merge to live workspace. The snapshot itself never filters.

This is the design the overlay primitive was always trying to give us. The original implementation built a parallel snapshot via git, then filtered it, then consumed it through git plumbing — three indirections to do what `read()` already does correctly.

## Goal

Replace the git-tree snapshot mechanism with the lowerdir as the snapshot. After Phase 8:

- `git_snapshot.py`, `build_live_snapshot_in_namespace`, and the `--snap` argument plumbing through the runner are removed from the hot path.
- OCC's per-path base reads consume the lowerdir directly via filesystem `read()`.
- The snapshot represents the full pre-command workspace state, including gitignored content.
- The 10× warm-path svc.cmd p50 drops by the snapshot's current cost (~0.245s) — but that is a side effect, not the goal. The goal is correct snapshot semantics.

## What is and isn't in scope

**In scope.**
- Replacing the snapshot construction step with a "snapshot is the lowerdir" contract.
- Reshaping every snapshot consumer (OCC base read, classifier base lookup, `git_show_base_factory`, anything that calls `git show <snap>:path`) to read from the lowerdir filesystem path instead.
- Probes (Task 8.0 series) to determine the mechanism for keeping the lowerdir authoritative across the per-command unshare lifetime, given that reflink-based snapshots have already been ruled out on the target filesystem.
- A freshness/external-mutation guard that fails safe if the lowerdir is observed to have shifted between commands without going through OCC.

**Out of scope.**
- HEAD-as-snapshot. Superseded — it preserved the same incorrect filtering as today, just faster.
- Narrowed snapshot. Same reason.
- libgit2 / pygit2 substitution. Not relevant — we're not constructing trees anymore.
- Changing OCC's conflict-detection algorithm.
- Changing the gitignore routing of upperdir merges (gitincluded → OCC, gitignored → direct-merge stays as today).
- Replay/audit of full historical lowerdir states across many commands. This is a separate concern — Phase 8 makes correct *per-command* base reads possible; multi-command historical replay needs its own snapshot mechanism (e.g., periodic CoW captures) and is deferred.
- Streaming `on_progress_line`. Unchanged.

## Probes (Task 8.0 series)

The contract is fixed; the implementation depends on what the underlying filesystem and Daytona's mount model permit.

### Task 8.0.A — Filesystem capability

**Question.** What is the actual filesystem under `/workspace`, and what CoW / snapshot mechanisms does it support?

**Procedure.**
- `findmnt -no FSTYPE,SOURCE /workspace` (or wherever the live workspace root is mounted).
- Run `cp --reflink=always /workspace/<small_file> /tmp/test` and capture the exact failure (already known to fail; confirm the failure mode for the report).
- Test `cp -al` (hardlink tree) over a small subtree; record latency and any errors.
- If FS reports `btrfs`: test `btrfs subvolume snapshot`. If `xfs` with reflink: re-test reflink with explicit `xfs_io`. If `overlay` (likely): note that no kernel-level CoW primitive is available; the snapshot mechanism must be overlay-based.

### Task 8.0.B — Lowerdir lifetime probe

**Question.** Does the existing `_NS_LOWER` bind-mount (set up by `setup_mounts` inside the unshare namespace) survive long enough for the OCC commit step to read from it, given that OCC commit runs in the daemon process **after** the unshare process exits?

**Hypotheses.**
1. The bind-mount is namespace-scoped; it disappears when the unshare process exits, so OCC running in the daemon cannot read from `_NS_LOWER`.
2. The lowerdir's underlying directory (the live workspace path before bind-mount) is persistently visible to the daemon, so OCC can read from there directly.
3. The live workspace is bind-mounted to the merged overlay during the command and reverts after the unshare exits, so the lowerdir is reachable both inside the namespace (via `_NS_LOWER`) and from the daemon (via the underlying path) once the namespace tears down.

**Procedure.**
- Read `setup_mounts` in `namespace.py` to confirm which paths exist where, and which are namespace-scoped.
- Add transient instrumentation: from the daemon, after the unshare process exits, attempt to `stat()` and `read()` from the underlying workspace path and record results.
- Determine whether the daemon needs its own persistent reference (e.g., a daemon-side bind-mount of the workspace at a known stable path) to guarantee reads succeed regardless of namespace teardown ordering.

### Task 8.0.C — Snapshot persistence requirement

**Question.** Does OCC require the snapshot to be readable *after* the user command has begun mutating the upperdir? Or only before the upperdir walker runs?

This determines whether we need any persistence at all beyond "the live workspace at command-start." If OCC's base reads happen entirely inside the unshare process before the user command writes anything, the lowerdir bind-mount is enough. If OCC base reads happen in the daemon after the user command, we need the lowerdir to remain readable from the daemon's perspective.

**Procedure.**
- Trace OCC's read points by grepping for `git_show_base_factory`, `git show`, and any `<snap>` consumer in `auditor.py`, `classifier.py`, `direct_routes.py`, and the write-coordinator.
- Document the call ordering: who reads the snapshot, when, from which process.

**Decision rules.** The combined output of 8.0.A/B/C selects the implementation approach:

| Combined finding | Likely mechanism |
|---|---|
| FS supports reflinks (we know it doesn't, but recording for completeness) | snapshot = `cp --reflink=always /workspace /snapshot/<id>`; OCC reads from snapshot dir |
| FS is btrfs | snapshot = `btrfs subvolume snapshot /workspace /snapshot/<id>` |
| FS is overlay/ext4 (no kernel CoW) AND OCC reads happen inside the unshare process before any upperdir writes | snapshot = `_NS_LOWER` bind-mount; OCC reads via filesystem path inside the namespace; no persistence needed |
| Same FS but OCC reads happen in the daemon post-namespace | snapshot = a daemon-side persistent bind-mount of the workspace's underlying path; OCC reads via that |
| Same FS and we need historical replay | overlay-chain-as-snapshot (out of scope per §"Out of scope"; deferred) |

## Implementation tasks

Tasks 8.0.B/C selected the in-namespace lowerdir read mechanism: base reads happen inside the runner before `diff.ndjson` is handed back to the daemon, so no persistent daemon-side lowerdir mount is required for the current per-command OCC contract.

### Task 8.1 — Replace snapshot construction with lowerdir reference

Remove `build_live_snapshot_in_namespace` and the `--snap` argument from the runner's hot path. Pass instead a path that the daemon and the unshare process both agree refers to the lowerdir at command-start.

### Task 8.2 — Reshape OCC base reads

Every consumer of `git show <snap>:path` migrates to `read(<lowerdir_path>/path)`. Ordered list of touch points to be enumerated by Task 8.0.C.

### Task 8.3 — Remove gitignore filtering at snapshot time

The classifier already does gitignore routing on the upperdir side; nothing changes there. The change is that the *base* read no longer filters — it returns whatever the lowerdir has at that path, gitincluded or gitignored.

### Task 8.4 — Freshness / external-mutation guard

If the lowerdir's content has shifted between commands without going through OCC, the contract is violated and OCC's base reads are stale. Guard via a fingerprint (e.g., `stat()` mtime on workspace root + `.git/index`) sampled at the end of each command and checked at the start of the next; fail closed on mismatch.

### Task 8.5 — Parity corpus

Compare the new lowerdir-as-snapshot OCC outcomes against today's `git add -A` snapshot outcomes across the Phase 6 fixture corpus, plus new fixtures that exercise gitignored content (where today's snapshot was incomplete and the new snapshot is correct — these will diverge by design, and the parity test asserts the divergence is exactly the gitignore filter being removed).

## Risks

1. **OCC base read pathing.** If any consumer of `git show <snap>:path` runs in a context that can't see the lowerdir as a filesystem path (e.g., runs in a subprocess that doesn't inherit the relevant mount namespace), the migration in Task 8.2 has to reshape the consumer's process model too. Probes 8.0.B/C surface this.
2. **Behavioral divergence on gitignored paths.** Today's OCC ignores gitignored writes for base-read purposes; under Phase 8, OCC sees them. This could expose latent races that were previously masked by the incomplete snapshot — i.e., concurrent svc.cmds modifying the same `.venv` path will now be visible to OCC's verification, which may surface failures that today silently last-write-wins. This is a correctness improvement, but a behavioral change. The parity corpus must distinguish "intended divergence from removing the filter" from "regression."
3. **Lowerdir lifetime mismatch.** If OCC reads happen after the unshare exits, the namespace-scoped `_NS_LOWER` bind-mount is gone. The daemon needs its own stable reference to the lowerdir's underlying path. Task 8.0.B probes this.
4. **External workspace mutations.** The contract assumes only the daemon writes to the workspace. If an external mutator changes the lowerdir between commands, the freshness guard (Task 8.4) trips and we fall back to a full re-read. Logging makes this visible.
5. **Replay/audit deferred.** Phase 8 makes the *current* command's base reads correct; it does not give us per-command historical lowerdir snapshots for replay across many commands. If that need surfaces, a separate snapshot mechanism (overlay-chain or btrfs subvolume snapshots, depending on FS) becomes a follow-up phase.

## What ships

| Artifact | File | Status |
|---|---|---|
| Probe conclusions | `phase-08-implementation-report.md` | shipped |
| Phase 8 implementation report | `phase-08-implementation-report.md` | shipped |
| Snapshot construction removal | `runner.py`, `git_snapshot.py`, `auditor.py` (modified or deleted) | shipped |
| OCC base read reshape | `classifier.py`, `lowerdir.py`, `runner.py` | shipped |
| Gitignore filter removal at snapshot layer | `lowerdir_base_factory` reads filesystem lowerdir, including gitignored paths | shipped |
| Freshness guard | `auditor.py` | shipped |
| Parity corpus | `test_overlay_run.py`, `test_overlay_auditor.py`, `test_overlay_daemon_local_parity.py` | shipped |

## Landing summary

1. Tasks 8.0.B/C selected the in-namespace lowerdir read mechanism.
2. Tasks 8.1-8.3 removed git-tree construction, removed `--snap`, and moved base reads to `lowerdir_base_factory` in `lowerdir.py`.
3. Task 8.4 added the daemon-local freshness guard.
4. Task 8.5 landed through the focused overlay classifier/auditor parity tests, including gitignored lowerdir content coverage.
5. The live E2E gate passed: per-command OCC base reads come from lowerdir, `svc.cmd` 10x p50 is below the 2.0s ceiling, and no `git_snapshot` stage remains.
6. The cleanup pass removed the legacy `snap`, `snapshot_timings`, and `git_snapshot_timings` envelope/result fields from the active overlay/RPC path.
7. `phase-08-implementation-report.md` records the chosen mechanism, code changes, verification, and performance evidence.
