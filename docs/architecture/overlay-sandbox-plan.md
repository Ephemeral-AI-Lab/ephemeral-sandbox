# Overlay daytona_shell Sandbox — Canonical Plan

Status: implemented
Replaces: `docs/architecture/overlay-codeact-plan.md`,
          `docs/architecture/overlay-sandbox-implementation-plan.md`,
          `docs/architecture/git-worktree-codeact-migration.md`,
          `docs/architecture/git-workspace-codeact.md`

---

## 0. Decisions taken up front

These close the open questions from the prior two overlay docs. No flag, no
soak, one-shot cutover.

### Routing terminology

There are exactly two routes, keyed by `git check-ignore` against the live
workspace:

* **Gitinclude route** — every upperdir path that `git check-ignore` does
  *not* flag. Goes through strict-base OCC against `git show $SNAP:path`.
  Concurrent writers to the same path → **first-writer-wins**; later writers
  abort with `aborted_version`. This route covers files currently in the
  git index *and* brand-new files that are not matched by any `.gitignore`
  rule (agent-created scratch files, fresh source modules, generated
  outputs not added to ignore patterns).
* **Gitignore route** — every upperdir path that `git check-ignore` flags.
  Direct-merged into the live workspace via per-file
  `tempfile.mkstemp + os.rename`. Concurrent writers to the same path →
  **per-file last-writer-wins**. See §5.1 for the per-file vs per-tree
  caveat.

If the requested user-facing semantics are "all files not in the git index
are last-writer-wins," the implementation does not provide that. It
provides "all gitignore-route files are per-file last-writer-wins;
everything on the gitinclude route — including brand-new files not yet in
the git index but also not matched by any `.gitignore` rule — is
first-writer-wins via OCC." Routing decisions are made entirely from
`.gitignore` membership, not from git-index state.

| Decision | Choice | Why |
|---|---|---|
| Cutover | **One-shot replacement** of the git-workspace backend. Delete `git_workspace_*`, `git_diff_committer` in the same PR that lands overlay. | User directive. Git-workspace is capability-broken for gitignore deps; no value in dual-path soak. |
| Mount model | **Rootless, per-op `unshare -Urm`** with `userxattr` overlay. No pool, no persistent bind mount. | Chosen for namespace-scoped cleanup (kernel releases every mount when the ns exits — no stale-mount sweep, no leak on crash), no sudo in the user-command path, and per-op tmpfs that dies with the op. `sudo mount -t overlay` with a cross-fs live lower **also** works on Daytona (§9 probe A), so Design A from the prior implementation plan is technically viable; rejected on *operational* grounds above, not capability grounds. |
| Baseline | **`SNAP = git commit-tree`** over live tracked + staged + unstaged + untracked via a redirected `GIT_INDEX_FILE`. | Only primitive that captures the full dirty tree without moving a ref or firing hooks. `git rev-parse HEAD` is **not** sufficient — dirty/staged/untracked files would be invisible. |
| Diff + merge location | **Inside the unshare namespace, before exit.** One sandbox-side Python script walks `upperdir`, classifies, direct-merges gitignore paths into the live workspace via the lower bind, and emits an NDJSON payload of gitinclude changes for the orchestrator. | Upperdir is tmpfs inside the ns; it is gone the moment the ns exits. An orchestrator-side walker would see nothing. |
| `.git/**` writes | **REJECT whole run** with `git_conflict_reason = "overlay_rejected_dotgit_writes: <paths>"`. `success=False`, exit code preserved. Exception: `.git/index` / `.git/index.lock` refresh artifacts are ignored. | Agents should not mutate `.git` from inside daytona_shell. Failing loud surfaces misbehavior; silent-skip hides it. The index exception covers benign Git read probes whose copied-up index is discarded with the overlay upperdir and never reaches the live repository. |
| Whiteouts on gitignore paths | **Refuse by default.** Surface as `git_conflict_reason = "overlay_refused_gitignore_whiteout: <paths>"`. Agents that legitimately want to rebuild deps use `daytona_delete_file` (OCC-aware) or a future explicit rebuild tool. | Ecosystem-specific allowlists (`.venv/`, `node_modules/`, `target/`, `.next/`, `.gradle/`, `dist/`, …) rot. Refuse-all is the only policy that does not drift. |
| Whiteouts on gitinclude paths | **Emit as DELETE to OCC** (normal path). | OCC already handles this correctly via strict-base. |
| Mode-only changes on gitinclude paths | **Ignore.** Overlay copies up on mode change with identical content; classifier short-circuits on content equality with the SNAP base. | OCC does not track mode. Avoids spurious no-op changes. |
| Symlinks / opaque-dir markers on gitinclude paths | **REJECT.** | Matches current V1 OCC policy. Defer until OCC represents them. |
| Non-UTF-8 content on gitinclude paths | **REJECT.** | Matches current OCC policy. |
| Non-UTF-8 content on gitignore paths | **Direct-merge as bytes.** | Binary deps (`.whl`, `.so`, `.pyc`) must pass through. |
| Mixed gitinclude + gitignore writes | **ACCEPT; non-atomic across routes by design.** Gitinclude writes go through strict OCC and may abort (first-writer-wins). Gitignore writes direct-merge independently per file (per-file last-writer-wins) and are not rolled back if the OCC pass aborts. | daytona_shell is a live shell runner. Real commands inevitably touch source/config (gitinclude) plus runtime state (`.venv/`, `node_modules/`, caches, build outputs — gitignore). Rejecting mixed writes would make normal install/test/build workflows brittle. The contract is explicit metadata/warnings, not transactional rollback. |
| Tmpfs upper full (`ENOSPC`) | Surface as `git_conflict_reason = "overlay_upper_full"` and fail the run. Tmpfs size set via `EOS_OVERLAY_UPPER_SIZE_MB` (default 512). | Fail-fast beats silent truncation of writes. |
| Multi-shell-per-daytona_shell | **One overlay per `svc.cmd`.** N `shell()` calls inside the wrapper share the same merged view; classifier sees cumulative upperdir after wrapper exits. | Matches how `_WRAPPER_TEMPLATE` already works — `shell()` invocations happen inside a single wrapper process. |
| Concurrency | **Per-sandbox `asyncio.Semaphore(N)`** (`EOS_OVERLAY_MAX_CONCURRENT`, default 20). No pool, no slot state machine. | Pool existed only to amortize `git clone --shared`. Per-op unshare has nothing to amortize. |
| Read isolation / live lower mutation | **No full read isolation; live changes are accepted.** Concurrent ops share live as lower; peer writes to live (via OCC commits or gitignore direct-merges) can mutate another op's active lower while its overlay is mounted. Per kernel docs this lower mutation is "undefined behavior" but explicitly "will not result in a crash or deadlock." | daytona_shell is a live-workspace execution tool, not a snapshot-isolated runner. Practical surface is **weak read consistency inside the running command**: live peer changes may be visible, stale, partially visible through directory caches, or missed by a command that already read inputs. `SNAP` is only the gitinclude-write OCC base; it is not a read snapshot contract. Callers that require stable inputs must serialize above daytona_shell or rerun. |
| Env-var | `EOS_OVERLAY_MAX_CONCURRENT`, `EOS_OVERLAY_UPPER_SIZE_MB`. | Picked one naming scheme (`EOS_OVERLAY_*`). `CI_CODEACT_*` retired. |

### Workspace invariants

- **The workspace must be a git repository.** `build_live_snapshot`
  invokes `git commit-tree`; non-repo directories fail at SNAP build.
  This is the only hard precondition.
- **A `.gitignore` is strongly recommended but not required for
  correctness.** Without one, every upperdir entry classifies as
  gitinclude-route → routed through OCC. A `pip install` writing 10k files
  would push 10k entries through `WriteCoordinator`. The system stays
  correct, but throughput collapses and concurrent installs to shared
  paths produce many OCC aborts. Subdir `.gitignore`, `.git/info/exclude`,
  and `core.excludesfile` are all honored by `git check-ignore`, so a
  root-level file is not strictly required either — any of those sources
  is enough to keep dep installs on the fast path.

---

## 1. Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│                        Daytona Sandbox                               │
│                                                                      │
│   Live workspace: $WORKSPACE_ROOT                                    │
│     ├── src/*.py                (gitinclude source)                     │
│     ├── .venv/, node_modules/   (gitignore deps — present!)         │
│     ├── __pycache__/            (gitignore caches — present!)       │
│     ├── .git/                                                        │
│     └── .gitignore                                                   │
│                                                                      │
│   Per svc.cmd op:                                                    │
│   ┌─────────────────────────────────────────────────────────────────┐│
│   │  [orchestrator] build SNAP = commit-tree  (no ref moved)        ││
│   │  [orchestrator] unshare -Urm bash -c "<setup+cmd+diff>"         ││
│   │                                                                 ││
│   │  inside the ns:                                                 ││
│   │   tmpfs       ┌─── upperdir  (size-capped; tracks writes)       ││
│   │   bind-mount  ┌─── lowerdir  = $WORKSPACE_ROOT (live)           ││
│   │   overlay     ┌─── merged    = upper over lower (userxattr)     ││
│   │   bind-mount  ┌─── merged remounted at $WORKSPACE_ROOT          ││
│   │               │                                                 ││
│   │               ▼                                                 ││
│   │   [wrapper]   user command runs; reads pass through to lower;   ││
│   │               writes land in upper tmpfs                        ││
│   │                                                                 ││
│   │   [diff.py]   walks upperdir                                    ││
│   │       → classify each entry (whiteout? gitinclude? gitignore?) ││
│   │       → direct-merge gitignore creates/modifies into live      ││
│   │         workspace through the lower bind                        ││
│   │       → emit gitinclude changes as NDJSON to $RUN_DIR/diff.ndjson  ││
│   │                                                                 ││
│   │   [ns exits] tmpfs destroyed; only live-workspace writes persist││
│   └─────────────────────────────────────────────────────────────────┘│
└──────────────────────────────────────────────────────────────────────┘
        │ stdout + exit_code              │ diff.ndjson
        ▼                                 ▼
┌──────────────────────────────────────────────────────────────────────┐
│                       Orchestrator (host)                            │
│                                                                      │
│  parse NDJSON → OperationChange[] (strict_base=True)                 │
│  WriteCoordinator.commit_operation_against_base(...)                 │
│    match    → commit to live                                         │
│    mismatch → status=aborted_version, this op's gitinclude writes skipped│
│  cleanup run dir; release semaphore                                  │
└──────────────────────────────────────────────────────────────────────┘
```

### Per-op lifecycle

1. Acquire semaphore.
2. `SNAP = build_live_snapshot(sandbox)` — dangling commit, no ref moved,
   live `.git/index` byte-identical.
3. Write the user command, SNAP sha, and `$RUN_DIR` path into the
   sandbox-side wrapper.
4. `unshare -Urm bash -c '<setup-mounts> && { <user-cmd>; rc=$?; <diff-script>; exit $rc; }'`. **The diff script must run unconditionally on user-command exit, not chained with `&&`** — failed commands routinely leave meaningful state (modified caches, partial artifacts, half-written files) that the auditor must classify and surface. Setup itself stays `&&`-chained: if mount fails, there is nothing to audit.
5. Read `$RUN_DIR/diff.ndjson` → `OperationChange[]`.
6. `WriteCoordinator.commit_operation_against_base(changes)`.
7. Cleanup `$RUN_DIR`; release semaphore.
8. If any of steps 2–6 fail, raise. Tracked writes from this op are either
   committed through OCC or skipped on abort; peer live writes and already
   direct-merged gitignore writes remain live by design.

### Filesystem layer composition

Unchanged from prior plan doc §3. Reads free, writes to tmpfs upper.

---

## 2. Baseline primitive: `build_live_snapshot`

Shared primitive from the (now absorbed) worktree migration doc. One module,
reused verbatim by the overlay auditor.

**Module:** `backend/src/code_intelligence/routing/git_snapshot.py` (new)

```python
async def build_live_snapshot(
    sandbox: Any,
    exec_process: Callable[..., Awaitable[Any]],
    repo_root: str,
) -> str:
    """
    Return SHA of a dangling commit capturing tracked + staged +
    unstaged + untracked live state. No ref is moved. Live .git/index
    is byte-identical before and after.
    """
```

Invariants:
- `git commit-tree` does not fire `pre-commit` / `commit-msg` hooks
  (they bind to `git commit`, not plumbing).
- `GIT_INDEX_FILE` redirected to a tempfile → live index untouched.
- `git add -A` honors `.gitignore` → SNAP does not contain dep trees.
- `SNAP` is reachable via `git show $SNAP:path` for classifier base lookups.
- `SNAP` is GC-eligible after `gc.pruneExpire` (default 2w) — never run
  `git gc` from inside daytona_shell against live.
- The snapshot source is the canonical repository checkout with a real `.git`
  directory. Linked Git worktrees are rejected because this baseline is copied
  from the repository workspace, not from an auxiliary worktree.

---

## 3. Sandbox-side script: `overlay_run.py`

One Python script that runs **inside the unshare namespace**. Does:
mount setup → run user command → walk upperdir → classify →
direct-merge gitignore → emit NDJSON.

**Module:** `backend/src/code_intelligence/routing/overlay_run.py` (new,
deployed into the sandbox as a data file, not imported on the orchestrator).

### 3.1 Mount setup (inside the ns)

```bash
mkdir -p /ns/{tmp,merged}
mount -t tmpfs -o size=${UPPER_SIZE_MB}m tmpfs /ns/tmp
mkdir /ns/tmp/upper /ns/tmp/work
mount --bind $WORKSPACE_ROOT /ns/lower
mount -t overlay overlay \
      -o lowerdir=/ns/lower,upperdir=/ns/tmp/upper,workdir=/ns/tmp/work,userxattr \
      /ns/merged
mount --bind /ns/merged $WORKSPACE_ROOT
cd $WORKSPACE_ROOT
```

**Critical**: `upperdir` and `workdir` must live on the **same filesystem**
(kernel-enforced; overlay refuses the mount with `EXDEV` otherwise). Two
separate `tmpfs` mounts are two separate superblocks even though both are
tmpfs, so they fail this check. Use one tmpfs with two subdirs, as above.

The final `mount --bind /ns/merged $WORKSPACE_ROOT` ensures the user command
sees its expected absolute path — no command-string rewriting.

### 3.2 Classifier

Walk `/ns/tmp/upper` recursively. For each entry:

1. **Path policy first** (cheap; runs before any per-entry classifier work):
   - `rel.startswith(".git/")` → **REJECT** run with
     `overlay_rejected_dotgit_writes`. Run this gate **before** invoking
     `git check-ignore`, since the ignore matcher itself reads `.git`
     state and we don't want to consult a `.git` the user just mutated.
     Ignore `.git/index` and `.git/index.lock` refresh artifacts, and run
     user commands with `GIT_OPTIONAL_LOCKS=0`, so read-only Git probes do
     not become false protected-write failures.
2. **Overlay-kind** (from upperdir metadata; **must handle both
   privileged and rootless representations** — the `userxattr` mount
   option changes how overlayfs encodes whiteouts and opaque markers):

   | Form | Privileged overlay | Rootless (`userxattr`) overlay |
   |---|---|---|
   | Whiteout | char-device `S_IFCHR` with `rdev=0` | regular file (size 0) with `user.overlay.whiteout` xattr — rootless cannot `mknod` char devices |
   | Opaque dir | dir with `trusted.overlay.opaque="y"` xattr | dir with `user.overlay.opaque="y"` xattr |
   | Create / modify | regular file | regular file |
   | Symlink | symlink | symlink |

   The classifier must check **both** xattr namespaces and **both**
   whiteout encodings; otherwise rootless mode silently misses every
   deletion and every wholesale-replaced directory. Concretely:

   ```python
   def is_whiteout(st, xattrs):
       if S_ISCHR(st.st_mode) and st.st_rdev == 0:
           return True                                  # privileged form
       if S_ISREG(st.st_mode) and st.st_size == 0:
           if b"user.overlay.whiteout" in xattrs:
               return True                              # rootless form
       return False

   def is_opaque_dir(st, xattrs):
       if not S_ISDIR(st.st_mode):
           return False
       return (xattrs.get(b"trusted.overlay.opaque") == b"y" or
               xattrs.get(b"user.overlay.opaque")    == b"y")
   ```

3. **Route classification** (after `.git/` REJECT and overlay-kind):
   - `git check-ignore -z --stdin` (one batch call for the surviving
     entries) says ignored → **gitignore route**.
   - Otherwise → **gitinclude route**.
4. **Per-route actions** — see §3.3 and §3.4.

`git check-ignore` is invoked once against the live repo (lower side) with
every candidate path streamed over stdin. At the 10k-path end of the scale
(`pip install torch`) that stays under any reasonable stdin limit; chunk at
1 MiB if it ever exceeds.

### 3.3 Tracked route (emit to NDJSON)

For each gitinclude entry:

- **Kind-gate first** (fail-closed):
  - symlink → REJECT (`overlay_unsupported_symlink`).
  - opaque-dir → REJECT (`overlay_unsupported_opaque_dir`).
  - non-UTF-8 content → REJECT (`overlay_non_utf8_gitinclude`).
  - mode-only change (content equal to `git show $SNAP:rel`) → skip.
- Otherwise emit:
  ```json
  {
    "path": "<rel>",
    "kind": "modify" | "create" | "delete",
    "base_content": "<git show $SNAP:rel>" or "",
    "base_existed": <bool>,
    "final_content": "<read /ns/tmp/upper/rel>" or null,
    "strict_base": true
  }
  ```

`base_content` always comes from `git show $SNAP:path`, never from a live
filesystem read. This is what makes OCC strict-base work under concurrent
peer edits (see §5).

### 3.4 Gitignored route (direct-merge inside the ns)

Inside the ns, `/ns/lower` is bind-mounted to the live workspace. Writes to
`/ns/lower/<rel>` land on the real disk. That is how we merge.

- **whiteout** → REJECT run (`overlay_refused_gitignore_whiteout`).
- **create / modify** → atomic write via `tempfile.mkstemp` in the
  target's parent directory, then `rename` over target. Each op gets a
  unique temp filename, so concurrent writers to the same gitignore
  path produce clean last-writer-wins on the final rename without torn
  intermediate state:
  ```python
  fd, tmp_path = tempfile.mkstemp(
      dir=os.path.dirname(live_target),
      prefix=os.path.basename(live_target) + ".",
  )
  os.close(fd)
  shutil.copyfile(upper_source, tmp_path)
  os.rename(tmp_path, live_target)
  ```
  Bytes copy, not text. Parents created as needed.
- **opaque-dir** → REJECT run (`overlay_refused_opaque_dir`)
  (rare; equivalent to a whiteout of the whole dir).

No OCC on this path. Last-writer-wins semantics across concurrent ops
writing to the same gitignore path — matches how `pip install` behaves
natively.

Mixed gitinclude + gitignore writes are accepted. Gitignore direct-merges are
applied inside the namespace before gitinclude OCC runs on the orchestrator. If
gitinclude OCC later aborts, the gitignore writes remain live. That is intentional
live-shell behavior, not a rollback bug. The auditor must surface this as
metadata/warning so agents know runtime state changed even though gitinclude source
writes did not land.

### 3.5 NDJSON transport

Path: `$RUN_DIR/diff.ndjson`. `$RUN_DIR` is on the container fs (not under
the overlay), so it survives ns exit. One JSON object per line. Metadata
line first:

```json
{"_meta": {"snap": "<sha>", "exit_code": 0,
           "upper_bytes": 12345, "upper_files": 42,
           "gitinclude_changes": 3, "gitignore_changes": 39,
           "whiteouts": 0, "dotgit_rejects": 0,
           "direct_merged_bytes": 4567890}}
```

Then one object per emitted gitinclude change.

### 3.6 Error exits from the script

The script exits with the user command's exit code on success. On REJECT
it exits with a distinct sentinel (e.g. `200 + policy_code`) and writes a
`_reject` meta line to NDJSON so the orchestrator can surface the reason
without parsing log lines.

---

## 4. Orchestrator side

### 4.1 New modules

```
backend/src/code_intelligence/routing/
  git_snapshot.py               # §2, build_live_snapshot
  overlay_config.py             # EOS_OVERLAY_MAX_CONCURRENT, EOS_OVERLAY_UPPER_SIZE_MB
  overlay_types.py              # OverlayLease, OverlayCommandResult,
                                # OverlayRunError, OverlayPolicyReject, OverlayDiff
  overlay_run.py                # §3, sandbox-side script (data file)
  overlay_auditor.py            # run-one-op: snap → exec → parse → OCC → cleanup
  overlay_command_committer.py  # thin adapter that feeds NDJSON changes into
                                # WriteCoordinator.commit_operation_against_base
```

### 4.2 Modified modules

- `command_executor.py` — swap all `GitWorkspace*` for `OverlayAuditor`.
  Rename `_git_workspace_pool` → *delete*; rename
  `_ensure_git_workspace_auditor` → `_ensure_overlay_auditor`. No flag.
- `service.py` — remove `_git_workspace_pool` shim property; expose no
  pool (there isn't one).
- `telemetry.py` — add overlay counters (§6).
- `tools/daytona_toolkit/shell_tool.py` — error-string sweep;
  `FileChangeResult.git_commit_status` / `git_conflict_reason` /
  `ambient_changed_paths` shape preserved.

### 4.3 Deleted modules

```
backend/src/code_intelligence/routing/git_workspace_pool.py
backend/src/code_intelligence/routing/git_workspace_auditor.py
backend/src/code_intelligence/routing/git_workspace_types.py
backend/src/code_intelligence/routing/git_workspace_config.py
backend/src/code_intelligence/routing/git_diff_committer.py
```

And their tests:

```
backend/tests/test_code_intelligence/test_git_workspace_auditor.py
backend/tests/test_code_intelligence/test_git_workspace_pool.py   (if present)
backend/tests/test_code_intelligence/test_git_diff_committer.py   (if present)
```

Note: `WorkspaceDiff` types leave with them. The orchestrator-side overlay
path speaks `OverlayDiff` → `OperationChange[]` directly. No intermediate
compatibility shim.

### 4.4 Absorbed / retired docs

```
docs/architecture/overlay-codeact-plan.md             → delete
docs/architecture/overlay-sandbox-implementation-plan.md → delete
docs/architecture/git-worktree-codeact-migration.md   → delete
docs/architecture/git-workspace-codeact.md            → delete
```

`docs/architecture/code-intelligence.md` now describes the active overlay
`svc.cmd` path.

### 4.5 Auditor output shape (preserved downstream contract)

`OverlayAuditor.execute(...)` returns a `SimpleNamespace` with the same
fields the git-workspace auditor emitted:
`result`, `exit_code`, `changed_paths`, `ambient_changed_paths`,
`git_commit_status`, `git_conflict_reason`, `git_conflict_file`.

That keeps `shell_tool.py`'s `FileChangeResult` assembly untouched.

Overlay may also include additive metadata fields on the raw response:

- `gitinclude_changed_paths`: gitinclude paths committed through OCC.
- `gitignore_direct_merged_paths`: gitignore paths direct-merged into live.
- `gitignore_direct_merged_count`: count of direct-merged ignored paths when
  the full path list is too large to return.
- `mixed_gitinclude_gitignore`: true when both routes had changes.
- `mixed_partial_apply`: true when gitignore writes landed but gitinclude OCC
  aborted.
- `warnings`: includes
  `"gitinclude changes aborted by OCC; gitignore runtime changes were already applied"`
  when `mixed_partial_apply` is true.

For downstream compatibility, `changed_paths` remains the committed gitinclude
path set. Gitignored dependency/cache paths stay out of `changed_paths` so
existing write-scope hooks do not treat runtime artifacts as source edits.

---

## 5. Concurrency and write correctness

Overlay daytona_shell intentionally does **not** provide snapshot read isolation.
Each command reads through an overlay lowerdir bound to the live workspace.
While the command is running, peer OCC commits and gitignore direct-merges may
become visible, remain stale, or be observed inconsistently through lowerdir
caching and directory walks. That is accepted live-workspace behavior.

`SNAP` only protects gitinclude writes from this operation. It freezes the base
contents used for strict OCC comparison after the command exits; it does not
freeze what the command was able to read while it was running.

```
Op1: t0 SNAP1 (base A) | t2 upper writes A' | t5 OCC: live==A → commit A'
Op2 (peer):                        t1.5 OCC: live A → B
Op3: t6 SNAP3 (base B) | t8 upper writes B'' | t11 OCC: live==B → commit B''

Race:
Op1: t0 SNAP1 (base A) | t2 upper writes A' | t3.5 peer lands A → C
                                            | t5 OCC: live==C ≠ A → ABORT
     live still = C; upper already dead; Op1's gitinclude intent is skipped.
```

**Invariant** — `base_content` always comes from
`git show $SNAP:path`, frozen in git's object store at snapshot time. OCC's
live-vs-base hash compare catches every peer write between SNAP and commit.

**Direct-merge (gitignore-route) paths skip OCC by design.** Per-file
last-writer-wins is accepted; this is how concurrent `pip install` works on
any host. **It is per-file, not per-tree.** See §5.1 — concurrent installs
of *different versions* into the same gitignore prefix can interleave at
the file level and produce a Frankenstein tree (file `A` from op1, file `B`
from op2). The atomic-rename guarantee is freedom from torn writes within a
single file, not coherent package-level swap.

**Mixed gitinclude + gitignore writes are non-atomic by design.** A command like
`pip install foo && echo foo >> requirements.txt` may leave `.venv/` updated
even if the gitinclude `requirements.txt` OCC commit aborts. This matches live
shell behavior: runtime/dependency/cache effects can persist independently from
source-file commit success. The tool reports the partial outcome with
`mixed_partial_apply=true`, separated path lists, and a warning.

**Read visibility is weak by design.** A test/build command may observe peer
changes that land mid-run, may continue seeing older lowerdir state, or may see
mixed directory contents. Callers that need deterministic inputs must avoid
concurrent daytona_shell runs against the same workspace or rerun after peer writes
settle.

### 5.1 Throughput and scaling characteristics

Correctness (above) and throughput are different questions. This subsection
describes which concurrency loads scale well and which produce expected
friction. "High concurrency" claims are workload-bounded, not universal.

**Scales well — full parallelism, no friction:**
- Read-heavy ops (`pytest`, `ruff`, type-check, `git status`): no merge
  contention, all complete in parallel. Weak read consistency is
  tolerable for tools that read inputs at startup only.
- Independent installs to different deps (parallel `pip install foo`
  + `pip install bar`): different upperdir paths, no merge collision.
- Concurrent edits to **different** gitinclude files: OCC's
  `WriteCoordinator` handles in parallel — the recent "OCC parallelism
  unlock" commit landed this hot path.

**Bounded by design — correct but throughput-limited:**
- Concurrent edits to the **same** gitinclude file: produces N−1 OCC
  aborts. First-writer-wins. Agents retry against the new base. Applies
  equally to files already in the git index and to brand-new files
  that are not matched by any `.gitignore` rule.
- Concurrent installs of the **same version** to the same gitignore
  prefix (e.g., 10 parallel `pip install requests==2.31.0`): per-file
  last-writer-wins on direct-merge. Final state is content-equivalent
  to one op's writes since every file's bytes are identical. Per-op
  unique tmp filenames (§3.4) prevent torn writes during the rename
  race.

**Hazardous by design — silent inconsistency possible:**
- Concurrent installs of **different versions** to the same gitignore
  prefix (e.g., `pip install requests==2.30.0` and `pip install
  requests==2.31.0` in parallel): per-file last-writer-wins runs
  independently per upperdir entry, so the final tree can interleave —
  `__init__.py` from version 2.30, `models.py` from 2.31, etc. This
  matches the documented contract ("per-file last-writer-wins, not
  per-tree") but is the worst failure mode in this system: silent,
  non-deterministic, only reproducible under load, and capable of
  producing an importable-but-broken tree. The orchestrator cannot
  detect this from the NDJSON alone — both ops' direct-merges are
  successful by their own measure.

  The plan does not introduce sandbox-side coordination to prevent
  this. Callers that install dependencies concurrently into the same
  gitignore prefix must serialize at the agent layer (e.g., one
  install-tool call at a time per prefix) or accept the risk. The
  bench in §8 PR 2 should include a "concurrent different-version
  install to the same prefix" check so the failure mode is visible
  rather than implicit.

**Hard limits:**
- `EOS_OVERLAY_MAX_CONCURRENT` (default 20) caps per-sandbox parallelism.
  Memory ceiling: `N × EOS_OVERLAY_UPPER_SIZE_MB` = `20 × 512 MB = 10 GB`
  tmpfs per sandbox.
- Linux mount-table churn: ~5 mounts created and torn down per op.
  At 100-load that is ~500 mounts/sec churned per sandbox. Kernel
  handles it; measurable but not a wall.
- `git check-ignore` batched stdin: chunk at 1 MiB for ops writing
  >10k paths (already noted in §3.2).

**Workloads that should NOT run concurrently in daytona_shell:**
- Filesystem watchers (`pytest-watch`, `nodemon`): re-read mid-run,
  exposed to the weak-read-consistency surface above.
- Build tools that hash inputs and rely on consistent reads (some
  Cargo / Gradle workflows): may produce wrong outputs that OCC will
  not catch (OCC's base is `$SNAP`, not the lower contents the build
  actually saw).
- Detached background processes via `nohup ... &`: killed on ns exit.
  Not strictly a concurrency issue but worth flagging in the same
  context — daytona_shell ops are namespace-scoped lifetimes, not container
  lifetimes.

**Capacity baseline** (extrapolating from prior `git-workspace` at
100-load = 20.9s wall):
- Overlay should be **better** for small-write ops (delta-only walker
  vs whole-snapshot walker).
- Overlay should be **comparable** for big-write ops (copy-up + atomic
  rename storm vs git-workspace's batched rsync).
- Realistic per-sandbox throughput: ~5–10 ops/sec at default
  `semaphore=10`, dominated by per-op mount setup (~100–200 ms) +
  merge-back I/O.
- Bench gates in §8 PR 2 confirm these estimates against real workloads.

---

## 6. Observability

Per-`svc.cmd` telemetry (added to `routing/telemetry.py`):

- `overlay.snap_build_ms`
- `overlay.mount_setup_ms`
- `overlay.cmd_ms`
- `overlay.diff_ms`
- `overlay.merge_back_ms`
- `overlay.upper_bytes` (peak tmpfs use)
- `overlay.upper_files`
- `overlay.gitinclude_changes`
- `overlay.gitignore_changes`
- `overlay.direct_merged_bytes`
- `overlay.whiteouts_gitinclude` / `overlay.whiteouts_gitignore_refused`
- `overlay.dotgit_rejects`
- `overlay.upper_full_failures`
- `overlay.gitignore_changes_after_aborted_gitinclude` (count of ops where
  gitignore writes landed on live but gitinclude OCC aborted — expected under
  the accepted mixed-write contract)
- `overlay.mixed_gitinclude_gitignore_ops`
- `overlay.mixed_partial_apply_ops`

Threshold alarm when `overlay.upper_bytes` exceeds 80% of
`EOS_OVERLAY_UPPER_SIZE_MB` for a given op.

---

## 7. Risk register

| # | Severity | Risk | Mitigation |
|---|---|---|---|
| 1 | HIGH | Rootless overlay + userxattr probe regresses on a new Daytona image | Re-run `probe_overlay_capability.py` + `probe_overlay_followup.py` as CI smoke on every base-image pin bump; fail the build if either regresses. |
| 2 | HIGH | Copy-up amplification (`sed -i` on a huge file) fills tmpfs | Tmpfs `size=` cap + fail-fast ENOSPC surfaced as `overlay_upper_full`. Default 512 MB, overridable per-env. |
| 3 | HIGH | Shape drift in `FileChangeResult` / downstream tools | Golden-output test comparing `shell_tool` shape before/after the cutover for a fixed set of fixture commands. |
| 4 | HIGH | SNAP GC mid-op | Rely on `gc.pruneExpire=2.weeks` default; daytona_shell ops complete in seconds. Do not invoke `git gc` from inside a daytona_shell command against live — enforced by `.git/**` reject policy. |
| 5 | MED | `git check-ignore` stdin size on huge dep installs | Chunk at 1 MiB stdin. Monitored via `overlay.gitignore_changes` count; page on outliers. |
| 6 | MED | Concurrent dep installs race on the same gitignore prefix | Accepted (per-file last-writer-wins). Documented in §3.4 and §5.1. Same-version concurrent installs are content-equivalent. **Different-version concurrent installs can interleave at the file level into a Frankenstein tree — silent, non-deterministic.** No sandbox-side coordination is introduced; callers that need coherent dep trees must serialize at the agent layer, or limit one install-style command per prefix per `svc.cmd`. |
| 7 | MED | Whiteout-refuse too strict for real workflows | Observable via `overlay.whiteouts_gitignore_refused`. If agents hit it frequently, add an explicit `rebuild-env` tool rather than relaxing the policy. |
| 8 | LOW | Namespace signal / uid semantics shift breaks `pytest`, `npm` | Downgraded. §9 probe F confirmed `unshare -Urm` does **not** create a new PID ns (inside-pid inode == host-pid inode), so `ps`, `/proc` walking, and PID-based tools behave identically to the host. Mount ns changes as expected; uid remapping is the standard rootless pattern already exercised by probes A and B. Residual risk is edge-case tools that walk `/proc/mounts` (risk 9); Phase 1 E2E catches those. |
| 9 | LOW | `.git/**` writes in upperdir because the user command ran `git commit` | REJECT is intentional. daytona_shell is not a git client. Agents that want to commit use a different tool. |
| 10 | LOW | Mount leakage on crash | Per-op ns owns every mount; kernel releases on exit. No persistent state, no stale-mount sweep needed. |
| 11 | LOW | Tmpfs OOM across parallel sandboxes | Bounded by `semaphore × upper_size_mb`. Default 20 × 512 MB = 10 GB RAM ceiling per sandbox. Documented. |
| 12 | LOW | Concurrent ops mutate each other's overlay lower (kernel docs: "undefined behavior") | **Accepted weak read semantics** — see §0 row "Read isolation / live lower mutation." daytona_shell commands run against a live lowerdir, so peer writes may be visible, stale, partially visible, or missed. `SNAP` only protects gitinclude write commit, not command reads. If a future workload needs defined-behavior reads, serialize above daytona_shell or swap to a per-op hardlink-clone lower (`cp -al $WORKSPACE_ROOT $RUN_DIR/lower`). |
| 13 | LOW | Mixed gitinclude + gitignore writes are non-atomic | **Accepted by contract.** The script direct-merges gitignore writes into live inside the ns, then emits gitinclude changes to NDJSON for orchestrator-side OCC. If OCC aborts, gitignore writes remain live while gitinclude writes are skipped. Coupled work like `pip install foo && echo foo >> requirements.txt` can update `.venv/` while `requirements.txt` aborts. Surface this via separated path metadata, `mixed_partial_apply=true`, a tool warning, and `overlay.mixed_partial_apply_ops`. |

---

## 8. Phased migration (one branch, four PRs)

No env flag. Each PR passes the full E2E suite before the next starts.
TDD: RED → GREEN → optional refactor, per project discipline.

### PR 1 — Capability gate + SNAP primitive (non-functional)

- Re-run `probe_overlay_capability.py` and `probe_overlay_followup.py`
  on the current Daytona target. Append the transcripts to this doc
  under §9 "Probe results." If either fails, **stop here** and re-plan.
- Add `git_snapshot.py` (§2) + tests:
  - snapshot of clean tree equals `HEAD`'s tree
  - snapshot captures dirty tracked file
  - snapshot captures untracked file
  - snapshot respects `.gitignore`
  - live `.git/index` is byte-identical before/after
  - no ref moved (`for-each-ref` equality)
  - `pre-commit` / `commit-msg` hooks do not fire
- No wire-up yet. Pure addition. Reverts cleanly.

### PR 2 — Sandbox-side overlay run script + auditor (functional, behind dispatcher swap)

- Add `overlay_config.py`, `overlay_types.py`, `overlay_run.py`,
  `overlay_auditor.py`, `overlay_command_committer.py`.
- Unit tests against synthetic upperdir trees for every classifier
  branch (gitinclude add/modify/delete, gitignore create/modify, whiteout
  gitinclude whiteout, gitignore whiteout refused, `.git/*` reject, mode-only
  short-circuit, non-UTF-8 gitinclude reject, non-UTF-8 gitignore pass,
  opaque-dir reject, symlink reject, tmpfs-full).
- E2E tests on live sandbox:
  - `pip install requests && python -c "import requests"` across two
    consecutive daytona_shell ops → second op sees `requests` (capability
    gate that git-workspace fails).
  - Concurrent peer edit to the same gitinclude path during overlay run aborts
    this op's gitinclude commit with `aborted_version`; the peer edit remains
    live (mirrors existing `test_live_daytona_occ_load.py`).
  - Mixed gitinclude + gitignore command with a forced gitinclude OCC abort still
    leaves gitignore writes live, skips gitinclude writes, and returns
    `mixed_partial_apply=true` plus the partial-apply warning.
  - 100-op load bench: wall, p95, throughput. Acceptance: ≤ git-workspace
    at every scale point. Record results in §9.
- Swap `AuditedCommandExecutor` to `OverlayAuditor`. `GitWorkspace*`
  modules are deleted. CI confirms nothing imports them.

### PR 3 — Delete git-workspace modules + tests

- Deleted the five routing modules listed in §4.3.
- Deleted the retired tests listed in §4.3.
- `ripgrep` for any surviving identifier referenced in §4.3 or
  `WorkspaceDiff`, `GitWorkspace*`, `snapshot_path`, `live_head`,
  `prepare_baseline`, `_PREWARM_SCRIPT`, `_CREATE_SLOT_SCRIPT`,
  `copy_baseline_snapshot`, `git_workspace_pool_size_per_sandbox`.
  Zero hits outside archived/deleted docs.
- Removed `service.py :: _git_workspace_pool`.

### PR 4 — Doc consolidation

- Deleted the retired docs listed in §4.4.
- Updated `docs/architecture/code-intelligence.md` to remove git-workspace
  references and describe the overlay model.

Each PR reverts cleanly on its own. No intermediate state ships a
broken backend.

---

## 9. Acceptance criteria

Filled in during PR 1 and PR 2.

**Probe results** (PR 1):

```text
date:          2026-04-19
daytona image: (recorded via sandbox spin-up; /proc/mounts excerpt in probe stdout)
decision:      rootless + userxattr + bind lower works: YES
```

Raw output: `backend/scripts/probe_overlay_followup.py` run 2026-04-19.
Summary of the sections load-bearing to this plan:

- **A (sudo cross-fs with live $HOME as lowerdir):** `SUDO_OVERLAY_CROSSFS_LIVELOW: YES`, `UPPERDIR_CAPTURES_WRITE: YES`, `LIVE_LEAK: NO`. Privileged overlay works on Daytona — contrary to the earlier probe's "not available" read. Chosen mount model is still rootless per §0 rationale, not capability.
- **B (rootless bind-overlay of $HOME):** `USERNS_CROSSFS_BIND_OVERLAY: YES`, `USERNS_UPPER_CAPTURES: YES`. Canonical plan path works.
- **D (mount ordering):** `D_OVERLAY_MOUNT: OK`, `D_REBIND_MERGED_ON_WS: OK`, `D_LOWER_INODE_AFTER_REBIND == pre-rebind inode` → `D_LOWER_STILL_ORIGINAL: YES`. Binding `/ns/merged` back onto `$WORKSPACE_ROOT` does **not** redirect `/ns/lower`; the overlay is not recursive. Upper writes stay in upper (`D_UPPER_LEAKED_TO_LOWER: NO`), merged is visible at `$WORKSPACE_ROOT` (`D_WS_SEES_MERGED: YES`), and upperdir is gone after ns exit (`D_POSTEXIT_UPPER_LEAKED: NO`).
- **E (write-to-lower-under-overlay, direct-merge path):** lower-side writes via the bind **do** persist on the real disk after ns exit (`E_POSTEXIT_DIRECT_ADD_PERSISTED: YES`), and the last lower-side write wins (`E_POSTEXIT_TRACKED_CONTENT=lower-loses`). Merged observes lower writes in real time (`E1_MERGED_SEES_LOWER_ADD: YES`, `E2_MERGED_AFTER_LOWER_MODIFY=lower-modified`); this is the "weak read consistency" surface §5 already calls out. Upper wins over lower for the same path (`E3_MERGED_AFTER_BOTH=upper-wins`). No `dmesg` overlay/EIO warnings.
- **F (userns/PID-ns at concurrency, N=10):** `max_user_namespaces=31322` (ample), `F_PID_NS_UNCHANGED: YES` (host-pid inode == inside-pid inode), `F_MNT_NS_CHANGED: YES`. Ten concurrent `unshare -Urm` + overlay ops each hold 2s; `F_CONCURRENT_WALL_MS=2321` → truly parallel (serial would be ~20s). All ten ops report `OK`.

First probe run exposed a bug in the previous §3.1 recipe that used two separate tmpfs mounts for `upperdir` and `workdir`; the kernel rejected the overlay with `EXDEV`. §3.1 has been corrected (single tmpfs at `/ns/tmp` with `upper`/`work` subdirs) and the re-run above confirms the fix.

**Bench** (PR 2):

```
warmup: Xs
 10 ops:  ok 10/10, wall Xs, throughput X ops/s, p95 Xs
 30 ops:  ...
 50 ops:  ...
100 ops:  ...
```

Acceptance:

1. All probes pass.
2. Unit tests green for every classifier branch listed in §8 PR 2.
3. Gitignored-persistence capability test passes (git-workspace fails it).
4. OCC abort test passes — peer edit to the same gitinclude path mid-run →
   `aborted_version`; this op's gitinclude writes are not applied and the peer
   edit remains live.
5. Mixed gitinclude + gitignore partial-apply test passes — gitignore writes
   persist, gitinclude writes abort, and the response exposes
   `mixed_partial_apply=true` with separated path metadata.
6. Bench p95 ≤ git-workspace p95 at 10/30/50/100. If worse, diagnose
   before PR 3.
7. `ripgrep` clean of deleted identifiers after PR 3.
8. `FileChangeResult` golden-output test passes before and after.

---

## 10. What this plan does *not* do

- Does not change `WriteCoordinator` or OCC engine semantics.
- Does not provide snapshot read isolation for daytona_shell commands. `SNAP` is an
  OCC write base only; commands run against a live lowerdir and may observe
  concurrent workspace activity.
- Does not make mixed gitinclude + gitignore writes transactional.
  Gitignore runtime effects can persist even when gitinclude OCC writes
  abort; this is surfaced rather than rolled back.
- Does not key routing on git index membership. The route key is
  `git check-ignore`. Brand-new files that are not in any `.gitignore` rule
  go through the gitinclude / OCC route and inherit first-writer-wins,
  even when they are absent from the git index. If a stricter "anything
  not in the index = last-writer-wins" policy is required, that is a
  different routing decision and is out of scope here.
- Does not provide per-tree atomicity on the gitignore route. Each upperdir
  file is renamed independently; concurrent multi-file installs of different
  versions to the same prefix can interleave. No sandbox-side locks or
  per-prefix coordination are introduced; serialization, if required, is the
  caller's responsibility.
- Does not change LSP / symbol-index refresh. `ambient_changed_paths`
  flows through the same hook as before.
- Does not touch the Daytona base image. If the probe regresses,
  Phase 1 stops the rollout and we escalate for a base-image change —
  there is no fallback to git-workspace after PR 3.
- Does not keep a feature flag. If the overlay backend proves unstable
  in production, rollback is a PR-revert, not a runtime toggle.
