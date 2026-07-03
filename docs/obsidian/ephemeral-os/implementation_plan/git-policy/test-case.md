---
title: Git Policy — Live-Docker Test Catalog for exec_command Git Operations
tags:
  - ephemeral-os
  - layerstack
  - git-policy
  - testing
status: draft
updated: 2026-07-03
---

# Git Policy Test Catalog — 30 cases, exec_command-driven

Companion to `spec.md` (design truth, to be authored alongside) and the
publish routing code that is the contract's implementation:
`crates/sandbox-runtime/layerstack/src/stack/publish/{route.rs,plan.rs}`.
This document defines the **live Docker sandbox** catalog that drives **real
git through `exec_command`** and asserts the publish outcome: 10 easy (EZ),
10 medium (MED), 10 complex (CX).

## The contract under test

Layerstack no longer knows what "git" is. The `.git` mutation forbid was
removed; `forbidden_path()` now protects only layerstack's own on-disk storage
(`manifest.json`, `workspace.json`, `layers/`, `staging/`, `.layer-metadata`).
Every other path — including everything under `.git/` — flows through the two
routes that already existed:

1. **Non-gitignored → Source → first-writer-wins.** OCC on the base
   fingerprint: a path commits if unchanged since the writer's base; a
   concurrent divergence attempts a 3-way line merge and rejects
   (`source_conflict`) only when the merge is conflicting or ineligible
   (binary/oversized). Committed lines carry a per-line `Origin`.
2. **Gitignored → Ignored → last-writer-wins, wholesale.** Not
   source-validated, not merged; a single wholesale blame range.

Consequences this catalog pins:

- **`.git` is ordinary.** A git command's `.git/**` writes publish as source
  (or, if `.git/` is gitignored, as wholesale ignored) — never a forbidden
  mutation. Nested repos (`pkg/.git/**`) are equally ordinary. Only an **exact**
  `.git` path component was ever special; `.gitignore`, `.github/`,
  `.gitattributes`, `.gitmodules` were and remain ordinary source.
- **Binary `.git` divergence is safe.** Concurrent divergence on a binary
  `.git` file (`index`, packfiles) is merge-ineligible → clean
  `source_conflict`, never a corrupt merged blob. The object DB is
  content-addressed, so it is naturally conflict-free under OCC.
- **What is NOT protected.** Destructive working-tree operations
  (`git reset --hard`, `git checkout .`, `git clean -fd`, `git rm -r`) publish
  their deletions as ordinary changes. Layerstack does not guard against them;
  that is the caller's responsibility (e.g. an agent pre-hook). OCC still
  rejects the *concurrent* clobber race; a settled-base deletion is allowed by
  design. This catalog documents the behavior rather than forbidding it (CX-06,
  CX-10).

### Routing decision table (the heart of the contract)

| Path shape (example) | Route | Policy | Case |
| --- | --- | --- | --- |
| `README.md`, `src/main.rs` | Source | first-writer-wins | EZ-03, MED-01 |
| `.git/config`, `.git/refs/heads/main` (text) | Source | first-writer-wins | EZ-01/02, CX-03 |
| `.git/index`, `.git/objects/**` (binary) | Source | first-writer-wins; binary divergence → clean reject | MED-05, CX-01 |
| `pkg/.git/**` (nested repo) | Source | first-writer-wins | EZ-06 |
| `.gitignore`, `.github/`, `.gitattributes` | Source | first-writer-wins (editable) | EZ-04, EZ-05 |
| gitignored (`target/`, `*.log`) | Ignored | last-writer-wins, wholesale | EZ-09, MED-02 |
| `.git/**` when `.git/` is gitignored | Ignored | last-writer-wins, wholesale | MED-07 |
| `manifest.json`, `layers/**`, `.layer-metadata` | — | **rejected** `protected_path` | EZ-10, MED-10 |

---

## 1. Environment & fixture toolkit

### 1.1 Bring-up

```sh
export PATH="$PWD/bin:$PATH"
bin/start-sandbox-docker-gateway --rebuild-binary        # rebuild + start gateway

RUN_ID=git-$(date +%Y%m%d-%H%M%S)
WS=$(mktemp -d /tmp/$RUN_ID-ws.XXXX)

# Every case runs on ubuntu:24.04. The stock image ships no git, so setup
# installs it once per sandbox before any case body runs.
sandbox-manager-cli create_sandbox --image ubuntu:24.04 --workspace-root "$WS" > /tmp/$RUN_ID-create.json
SID=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["id"])' /tmp/$RUN_ID-create.json)
sandbox-runtime-cli --sandbox-id "$SID" exec_command "apt-get update -qq && apt-get install -y -qq git"
```

Cases are implemented in this folder:
`cli-operation-e2e-live-test/runtime/command/`, alongside the existing
`test_exec_command_layer_depth_benchmark.py`, one file per tier
(`test_git_policy_easy.py` / `test_git_policy_medium.py` /
`test_git_policy_complex.py`, markers `git and easy` / `git and medium` /
`git and complex`). Sandboxes are created with `--image ubuntu:24.04`
(overridable via `E2E_IMAGE`, matching the benchmark's `IMAGE` env knob). Every
operation goes through `sandbox-manager-cli` / `sandbox-runtime-cli` and asserts
on its structured JSON — never log scraping.

**Environment preconditions — asserted once per suite, hard-fail (never skip):**

| # | Precondition | Check | Expected |
| --- | --- | --- | --- |
| P1 | `git` installed on the stock `ubuntu:24.04` (no git by default) | setup `exec_command "apt-get update -qq && apt-get install -y -qq git"`, then `git --version` | install exit 0; version prints — hard-fail otherwise |
| P2 | git identity set (commits need it) | fixture exports `GIT_AUTHOR_*`/`GIT_COMMITTER_*` or `git -c user.email=… -c user.name=…` | commits succeed |
| P3 | layer-stack root not on overlayfs | `findmnt -no FSTYPE /eos/layer-stack` | `ext4` |
| P4 | deterministic commits | `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` pinned per fixture | reproducible object hashes where asserted |

The git install is part of per-sandbox setup (the stock `ubuntu:24.04` has no
git); it runs once, before the case body, and its cost is excluded from any
case assertion. If the sandbox has no network to reach the apt mirror, the
suite hard-fails at P1 with a clear message rather than skipping.

One sandbox per case unless the case is explicitly multiagent. The suite is
**serial by default**; only CX cases spawn concurrent sessions *inside* one
case.

### 1.2 Fixture vocabulary

Layers are published by one-shot `exec_command` (each one-shot with changes
publishes exactly one layer). Multiagent cases open two long-lived
`create_workspace_session` leases at the **same base revision**.

| Fixture | Meaning | Construction |
| --- | --- | --- |
| `repo(init)` | fresh initialized repo | `exec_bare "cd /workspace && git init -q"` |
| `repo(committed N)` | repo with `N` commits on `main` | loop: write file `f$i` → `git add -A && git commit -qm c$i` |
| `repo(gitignore P)` | repo whose base `.gitignore` holds patterns `P` | `sandbox_from_workspace(tmp, files={".gitignore": P})` then `git init` |
| `agentA` / `agentB` | two sessions leased at one base revision `R` | two `create_workspace_session` on the same sandbox before either finalizes |
| `stale(agentB)` | B finalizes **after** A landed a publish (B's base `R` is now behind) | order the two finalizes |
| `nested(pkg)` | a repo inside a subdir (`pkg/.git`) | `mkdir pkg && cd pkg && git init -q` |
| `bin-index` | a repo whose `.git/index` is a real binary object (contains NUL) | any `git add` on a non-empty tree |

The suite builds on this folder's existing primitives from `core.cli`
(`manager(...)`, `runtime(sandbox_id, "exec_command", …)`, `is_error`) and
`core.cleanup`, as used by `test_exec_command_layer_depth_benchmark.py`. The
Step pseudo-code below names intent: `exec_bare` / `exec_in` denote an
`exec_command` (sessionless, or bound with `--workspace-session-id`), and
`file_read` / `file_write` / `file_blame` / `layerstack` denote the
corresponding `runtime` / `manager` CLI verbs whose JSON the case asserts on. A
seeded workspace (`sandbox_from_workspace(files={…})`) is the benchmark's
host-dir pattern — write files under a `tmp_path/workspace`, then
`create_sandbox --workspace-root`. Shared assertion helpers (`assert_ok`,
`assert_output`, `assert_content`, `assert_manifest_delta`,
`_assert_publish_rejection`, `_assert_stack_unchanged`,
`assert_exec_workspace_not_found`) are imported from the sibling file/workspace
suites or wrapped locally.

### 1.3 Teardown contract (part of every case)

After each case, in order — a teardown failure **fails the case loudly**:

1. Destroy every session; destroy the sandbox last.
2. `observability layerstack` shows `active_lease_count == 0` on every layer.
3. `manifest.json`/`root_hash` match the last **accepted** publish (no protected
   or discarded changeset advanced the stack).
4. Artifact bundle written even on failure (`cmd.log`, `result.json`,
   `layerstack.json`, per-case `verdict.json`).

---

## 2. Measurement — the three axes

Every case is verified on three axes; a case passes only when all three pass.

1. **Correctness (route & outcome)** — the change took the expected route
   (`source` / `ignored` / rejected `protected_path`), the publish landed or was
   rejected/discarded as specified, and `manifest_version` advanced by exactly
   the expected number of layers (`assert_manifest_delta`). The result JSON is
   the surface: `publish_rejected` + `publish_reject_class` on the terminal
   response for the reject cases.
2. **Attribution** — merged-view content after the git op (`file_read` byte
   equality) **and** `file_blame` line-origin: source paths tile per-line to the
   command/session owner; ignored paths return a single wholesale range
   (`start_line = 1`, `line_count = 1`) owned by `workspace_session:<id>`.
3. **Isolation (multiagent)** — for CX cases: OCC first-writer-wins, clean
   `source_conflict` on divergence (binary → always ineligible → reject, never
   corruption), gitignored last-writer-wins, and **git remains operable** after
   the concurrency (a fresh `git status`/`git log` in a new exec succeeds).
   Single-agent cases mark this axis `n/a`.

Per-case `verdict.json` (one schema for all 30):

```json
{
  "case_id": "MED-05",
  "run_id": "git-20260703-140000",
  "status": "pass",
  "axes": {
    "correctness": { "pass": true, "route": "source", "manifest_delta": 0,
                     "reject_class": "source_conflict" },
    "attribution": { "pass": true, "assertions": 3 },
    "isolation":   { "pass": true, "git_operable_after": true, "corruption": false }
  },
  "teardown": { "pass": true, "lease_registry_empty": true, "stack_unchanged": true },
  "defects": []
}
```

---

## 3. Test catalog

Per-case format: **Spec** (rule / routing-table row) · **Fixture** · **Steps**
(git via `exec_command`) · **Correctness** · **Attribution** · **Isolation**.
Every case ends with the §1.3 teardown contract; it is not repeated.

### 3.1 Easy — EZ-01…10

Single agent, single git operation, happy path. Whole tier is the rebuild gate
(`pytest -m "git and easy"`).

#### EZ-01 — `git init` publishes `.git` as ordinary source
- **Spec**: `.git` ordinary (table rows 2/3). **Fixture**: `repo(init)` in an empty workspace.
- **Steps**: `exec_bare "cd /workspace && git init -q"` → `file_read .git/HEAD` → `layerstack` before/after.
- **Correctness**: exec `status: ok`, **not** `publish_rejected`; manifest advances by 1 (`assert_manifest_delta 1`); the `.git/HEAD`, `.git/config` writes route `source` (route summary `source_count > 0`, `ignored_count == 0`).
- **Attribution**: `file_read .git/HEAD` returns `ref: refs/heads/main\n`; `file_blame .git/config` tiles to the one-shot's owner.
- **Isolation**: n/a (single agent).

#### EZ-02 — `git commit` persists; state survives into a fresh exec
- **Spec**: `.git` ordinary; object DB is normal source. **Fixture**: `repo(init)`.
- **Steps**: one exec: `git add -A && git -c user.email=t@e -c user.name=t commit -qm c1` over a seeded `README.md` → **new** bare exec `git -C /workspace log --oneline`.
- **Correctness**: commit exec `ok`, not rejected; manifest advances; `.git/objects/**`, `.git/index`, `.git/refs/heads/main` all route `source`.
- **Attribution**: the fresh `git log` reads exactly one commit `c1` — proving the object DB + refs persisted through publish and re-mount; `file_read README.md` byte-equal to the committed content.
- **Isolation**: n/a.

#### EZ-03 — a git-tracked working file is per-line attributed
- **Spec**: table row 1 (source). **Fixture**: `repo(init)`.
- **Steps**: `printf 'a\nb\nc\n' > /workspace/src.txt && git add src.txt` in one exec → `file_blame src.txt`.
- **Correctness**: `ok`; `src.txt` routes `source`; manifest +1.
- **Attribution**: `file_blame src.txt` returns a per-line tiling owned by the command owner (not a single wholesale range) — the source route attributes line origins.
- **Isolation**: n/a.

#### EZ-04 — `.gitignore` is editable and drives routing
- **Spec**: table rows for `.gitignore` + gitignored. **Fixture**: `repo(init)`.
- **Steps**: `printf 'out.log\n' > /workspace/.gitignore` (one exec) → second exec `printf 'x\n' > /workspace/out.log`.
- **Correctness**: the `.gitignore` write routes `source`, **not** forbidden (manifest +1); the later `out.log` write routes `ignored` per the now-published base `.gitignore`.
- **Attribution**: `file_blame .gitignore` per-line to owner; `file_blame out.log` a single wholesale range.
- **Isolation**: n/a.

#### EZ-05 — `.github/`, `.gitattributes`, `.gitmodules` are ordinary source
- **Spec**: exact-component match only. **Fixture**: `repo(init)`.
- **Steps**: one exec writing `.github/workflows/ci.yml`, `.gitattributes`, `.gitmodules`.
- **Correctness**: all three route `source`, none `protected_path`/rejected; manifest +1 (one layer, three source paths).
- **Attribution**: each readable via `file_read` with byte equality.
- **Isolation**: n/a.

#### EZ-06 — nested repo `pkg/.git` is ordinary source
- **Spec**: nested `.git` allowed. **Fixture**: `nested(pkg)`.
- **Steps**: `mkdir -p /workspace/pkg && cd /workspace/pkg && git init -q && echo hi > f && git add -A && git -c … commit -qm c1`.
- **Correctness**: `ok`, not rejected; `pkg/.git/**` routes `source`; manifest +1.
- **Attribution**: fresh exec `git -C /workspace/pkg log --oneline` reads the commit.
- **Isolation**: n/a.

#### EZ-07 — `git rm` publishes a deletion (not forbidden)
- **Spec**: deletions are ordinary changes. **Fixture**: `repo(committed 1)` with tracked `doomed.txt`.
- **Steps**: `git rm -q doomed.txt && git -c … commit -qm drop` → `file_read doomed.txt`.
- **Correctness**: `ok`, not rejected; the working-tree delete + `.git` updates route `source`; manifest +1.
- **Attribution**: `file_read doomed.txt` returns structured not-found in the merged view; `file_blame` on it is not-found.
- **Isolation**: n/a.

#### EZ-08 — `git mv` is delete-old + write-new
- **Spec**: rename = delete + write, both source. **Fixture**: `repo(committed 1)` with `old.txt`.
- **Steps**: `git mv old.txt new.txt && git -c … commit -qm move`.
- **Correctness**: `ok`; `old.txt` absent, `new.txt` present in the merged view; manifest +1.
- **Attribution**: `file_read new.txt` byte-equals the original content; `old.txt` not-found.
- **Isolation**: n/a.

#### EZ-09 — gitignored build output takes the ignored route
- **Spec**: table row gitignored. **Fixture**: `repo(gitignore "target/\n")`.
- **Steps**: one exec `git init -q` (publishes `.gitignore` base) then a build exec `mkdir -p /workspace/target && printf 'x\ny\n' > /workspace/target/out.bin`.
- **Correctness**: `target/out.bin` routes `ignored` (route summary `ignored_count == 1`, `source_count == 0` for that path); manifest advances.
- **Attribution**: `file_blame target/out.bin` is a single wholesale range owned by `workspace_session:<id>` — not per-line.
- **Isolation**: n/a.

#### EZ-10 — a protected path still rejects
- **Spec**: table protected row. **Fixture**: `repo(init)`.
- **Steps**: one exec `mkdir -p /workspace/layers && echo x > /workspace/layers/evil.txt` (a repo dir literally named `layers`).
- **Correctness**: terminal response `status: ok` **and** `publish_rejected: true` with `publish_reject_class: "protected_path"`; the whole changeset is discarded (`_assert_stack_unchanged`); a fresh exec proves `layers/evil.txt` is absent.
- **Attribution**: n/a (nothing committed).
- **Isolation**: n/a.

### 3.2 Medium — MED-01…10

One interaction or fault dimension per case; two-step or two-publish setups.

#### MED-01 — first-writer-wins on a tracked file
- **Spec**: table row 1; OCC. **Fixture**: `repo(committed 1)` with tracked `README.md`; `agentA`, `agentB` at base `R`.
- **Steps**: A edits + commits `README.md` (`echo A >> README.md; git commit -am A`) and finalizes → B, from stale `R`, edits + commits the same file (`echo B >> README.md`) and finalizes.
- **Correctness**: A lands (manifest +1); B's finalize carries `publish_rejected: true`, `publish_reject_class: "source_conflict"`; B's changeset discarded whole.
- **Attribution**: merged `README.md` = A's content; `file_blame` shows A's lines only.
- **Isolation**: after the reject, a fresh exec `git -C /workspace status` succeeds; no partial B state.

#### MED-02 — last-writer-wins on a gitignored path
- **Spec**: table gitignored row. **Fixture**: `repo(gitignore "cache.bin\n")`; `agentA`, `agentB` at base `R`.
- **Steps**: A writes `cache.bin=alpha` and finalizes → B, from stale `R`, writes `cache.bin=beta` and finalizes.
- **Correctness**: **both** finalize `ok`, neither rejected (ignored paths are not source-validated); manifest advances twice.
- **Attribution**: merged `cache.bin` = `beta` (last writer); `file_blame` a single wholesale range owned by B's session.
- **Isolation**: no `source_conflict` anywhere — the ignored route never OCC-rejects.

#### MED-03 — `git reset --hard` discards edits; the revert publishes
- **Spec**: destructive op = ordinary changes (not protected). **Fixture**: `repo(committed 1)` with `keep.txt`.
- **Steps**: one exec `echo dirty >> keep.txt && git checkout -- keep.txt` (or `git reset --hard HEAD`) → `file_read keep.txt`.
- **Correctness**: `ok`, not rejected; the net working-tree change (revert to committed content) publishes as `source`; manifest advances only if bytes net-changed vs base.
- **Attribution**: `file_read keep.txt` equals the committed content (the dirty line gone).
- **Isolation**: n/a (single agent — documents that layerstack does not block the destructive op).

#### MED-04 — `git clean -fd` removes untracked files
- **Spec**: deletions publish. **Fixture**: `repo(committed 1)` + untracked `scratch/a`, `scratch/b`.
- **Steps**: `git clean -fdq` (one exec) → `file_read scratch/a`.
- **Correctness**: `ok`; the removals publish as `source` deletions; manifest advances.
- **Attribution**: `scratch/a`, `scratch/b` both not-found in the merged view.
- **Isolation**: n/a.

#### MED-05 — binary `.git/index` divergence rejects cleanly (the safety case)
- **Spec**: binary divergence → ineligible → `source_conflict`, never corruption. **Fixture**: `repo(committed 1)`; `agentA`, `agentB` at base `R`.
- **Steps**: A stages a change (`git add`) rewriting the binary `.git/index` and finalizes → B, from stale `R`, stages a **different** change (different `.git/index`) and finalizes.
- **Correctness**: A lands; B rejects `source_conflict` on `.git/index`; B discarded whole.
- **Attribution**: merged `.git/index` = A's; no interleaved/torn index.
- **Isolation**: **the load-bearing assertion** — a fresh exec `git -C /workspace fsck` (or `git status`) succeeds; the object DB is intact; no corrupt merged blob was committed.

#### MED-06 — text ref divergence is merge-or-reject, never corruption
- **Spec**: the one sharp edge (text `.git` pointers). **Fixture**: `repo(committed 2)`; `agentA`, `agentB` at base `R`, `.git/packed-refs` or `.git/logs/HEAD` present.
- **Steps**: A and B each append a **disjoint** ref/log line from stale `R`; finalize A then B.
- **Correctness**: either B's disjoint append 3-way merges cleanly (manifest advances, both lines present) **or** it rejects `source_conflict` — assert it is one of these two, never a third.
- **Attribution**: if merged, both ref lines present and syntactically valid; if rejected, the ref equals A's.
- **Isolation**: fresh `git -C /workspace log`/`git show-ref` succeeds either way — the repo is operable; this case pins "surprising-but-harmless", not corruption.

#### MED-07 — `.git/` in `.gitignore` flips internals to last-writer-wins
- **Spec**: the user-facing knob (table row). **Fixture**: `repo(gitignore ".git/\n")`; `agentA`, `agentB` at base `R`.
- **Steps**: publish the base `.gitignore` containing `.git/`; A commits, B commits from stale `R`; finalize A then B.
- **Correctness**: both A and B finalize `ok`; **no** `source_conflict` on any `.git/**` path (they route `ignored` now); manifest advances twice.
- **Attribution**: `file_blame .git/index` (if read) is wholesale; `.git` reflects the last writer (B).
- **Isolation**: proves the routing knob works with zero git-specific code — a user who wants clobber-tolerant `.git` opts in via gitignore.

#### MED-08 — mixed source+ignored changeset is atomic
- **Spec**: mixed-route atomicity. **Fixture**: `repo(gitignore "out.log\n")` with tracked `src.txt`; `agentA` advances `src.txt` to make B's base stale.
- **Steps**: B (stale base) commits both an edit to `src.txt` (source, now conflicting) and a write to `out.log` (ignored) in one changeset; finalize.
- **Correctness**: the whole publish rejects `source_conflict` — the ignored `out.log` does **not** land on its own (source failure rejects the whole publish).
- **Attribution**: `out.log` absent in the merged view (atomic rejection).
- **Isolation**: A's `src.txt` is the surviving content.

#### MED-09 — branch switch rewrites the working tree
- **Spec**: net working-tree change publishes. **Fixture**: `repo(committed 2)` with branches `main` (has `only_main.txt`) and `feat` (has `only_feat.txt`).
- **Steps**: one exec `git checkout -q feat` → `file_read only_feat.txt` / `only_main.txt`.
- **Correctness**: `ok`; the checkout's working-tree delta (add `only_feat.txt`, remove `only_main.txt`, update `.git/HEAD`) publishes as `source`; manifest +1.
- **Attribution**: `only_feat.txt` present, `only_main.txt` absent; `.git/HEAD` points at `feat`.
- **Isolation**: n/a.

#### MED-10 — protected-path rejection surfaces on the terminal response + discard
- **Spec**: EX-05 shape, git-driven. **Fixture**: `repo(init)`.
- **Steps**: one bare exec `mkdir -p /workspace/.layer-metadata && echo x > /workspace/.layer-metadata/state` → follow-up exec into the (destroyed) session → fresh exec proves discard.
- **Correctness**: terminal response `status: ok`, `publish_rejected: true`, `publish_reject_class: "protected_path"`; session destroyed anyway (id unresolvable via `assert_exec_workspace_not_found`); `.layer-metadata/state` absent afterward.
- **Attribution**: n/a.
- **Isolation**: n/a.

### 3.3 Complex — CX-01…10

Multiagent concurrency, adversarial, scale. Each rebuilds its own sandbox; the
soak (CX-09) runs last.

#### CX-01 — two agents commit **different** files: both land
- **Spec**: disjoint concurrency; content-addressed object DB. **Fixture**: `repo(committed 1)`; `agentA`, `agentB` at base `R`.
- **Steps**: A commits new `a.txt`, B commits new `b.txt` (both from `R`); finalize A then B.
- **Correctness**: both finalize `ok`; manifest advances twice; no `source_conflict` (disjoint working-tree paths; git objects are distinct content-addressed paths).
- **Attribution**: merged tree has both `a.txt` and `b.txt`; each per-line attributed to its author session.
- **Isolation**: fresh `git -C /workspace fsck` clean; both blobs present in the object DB.

#### CX-02 — two agents commit the **same** file: one wins, no lost update
- **Spec**: OCC first-writer-wins. **Fixture**: `repo(committed 1)` tracked `shared.txt`; `agentA`, `agentB` at `R`.
- **Steps**: A and B each edit + commit `shared.txt` with different content from `R`; finalize A then B.
- **Correctness**: A lands; B rejects `source_conflict`; exactly one winner, no silent merge of working-tree content.
- **Attribution**: merged `shared.txt` = A's; `file_blame` = A's lines.
- **Isolation**: B's repo state discarded; fresh `git status` clean; A's commit is the head.

#### CX-03 — concurrent ref updates to `refs/heads/main` don't interleave
- **Spec**: text-ref OCC (single-line ref file). **Fixture**: `repo(committed 1)`; `agentA`, `agentB` at `R`.
- **Steps**: A and B each create a new commit (advancing `.git/refs/heads/main` to different hashes) from `R`; finalize A then B.
- **Correctness**: A lands; B's `.git/refs/heads/main` diverged from a single-line base → merge conflict → `source_conflict` (a one-line ref cannot cleanly 3-way merge two different hashes); no interleaved/duplicated ref line.
- **Attribution**: `.git/refs/heads/main` = A's commit hash exactly.
- **Isolation**: fresh `git -C /workspace log` shows A's history; B discarded; repo operable.

#### CX-04 — `git gc` churn coexists with a concurrent commit
- **Spec**: packfile churn under concurrency; object DB integrity. **Fixture**: `repo(committed 20)` (enough loose objects to pack); `agentA`, `agentB` at `R`.
- **Steps**: A runs `git gc -q` (repacks: loose objects removed, packfiles written), B commits a new file; finalize A then B (or the reverse) — assert whichever lands leaves a consistent DB.
- **Correctness**: the first finalize lands; the second either lands (disjoint) or rejects `source_conflict` on a shared `.git` path — assert one of the two; never a corrupt DB.
- **Attribution**: every commit reachable in the surviving history is readable (`git cat-file -p HEAD`).
- **Isolation**: fresh `git -C /workspace fsck --full` reports no corruption regardless of order.

#### CX-05 — large repo import stays bounded and unforbidden
- **Spec**: scale; opaque-dir expansion interplay for `.git/objects`. **Fixture**: seed a workspace with a moderately large repo (`git clone --bare` a local fixture of ~2k objects into `/workspace/repo`).
- **Steps**: one exec importing the repo → `layerstack` before/after → fresh `git -C /workspace/repo log --oneline | wc -l`.
- **Correctness**: `ok`, not rejected; the thousands of `.git/objects/**` writes route `source`; manifest +1; **no** `protected_path`/`opaque_dir_expansion_limit` reject for ordinary object trees (if an opaque-dir replace of `.git/objects/pack` is captured, its expansion stays within `OPAQUE_DIR_EXPANSION_LIMIT` — assert the limit is not tripped by a normal import; if it is, the case records the bound explicitly rather than hiding it).
- **Attribution**: the fresh `git log` count equals the imported history length.
- **Isolation**: n/a beyond the single import (documents scale behavior).

#### CX-06 — destructive multiagent: stale `reset --hard` loses to a concurrent publish
- **Spec**: OCC guards the concurrent clobber; settled deletions are allowed. **Fixture**: `repo(committed 1)` tracked `victim.txt`; `agentA`, `agentB` at `R`.
- **Steps**: A edits + commits `victim.txt` and finalizes → B, from stale `R`, runs `git reset --hard` / `git rm victim.txt` and finalizes.
- **Correctness**: A lands; B's deletion of `victim.txt` — a path that moved since `R` — rejects `source_conflict` (the concurrent clobber is caught). Contrast leg: a B that deletes a **settled** file untouched since `R` finalizes `ok` (allowed by design — the catalog documents this is pre-hook territory, not a layerstack forbid).
- **Attribution**: after the reject, merged `victim.txt` = A's edited content.
- **Isolation**: no cross-agent data loss on the concurrent path; the settled-delete leg is explicitly permitted.

#### CX-07 — routing uses the base `.gitignore`, not a racing one
- **Spec**: ignored routing keyed on `request.base.manifest`. **Fixture**: `repo(init)` with **no** ignore rule at base `R`; `agentA`, `agentB` at `R`.
- **Steps**: A publishes a `.gitignore` adding `data.bin` (advancing the head) → B, still based at `R` (where `data.bin` is **not** ignored), writes `data.bin` and finalizes.
- **Correctness**: B's `data.bin` routes by **B's base** gitignore (`R`, no rule) → `source`, not `ignored`; if `data.bin` did not exist at `R` it commits, else OCC applies. The routing is deterministic from the base, never from A's racing gitignore.
- **Attribution**: `file_blame data.bin` is per-line (source), confirming it did not silently take the ignored route.
- **Isolation**: demonstrates the base-pinned oracle under a gitignore race.

#### CX-08 — symlinks and special files under a repo route as source
- **Spec**: symlink change is ordinary `source`. **Fixture**: `repo(init)`.
- **Steps**: one exec `ln -s target /workspace/link && git add -A && git -c … commit -qm link` (and a `.git/**` symlink if the git version writes one).
- **Correctness**: `ok`, not rejected; the `Symlink` change routes `source`; manifest +1.
- **Attribution**: `file_read link` resolves per the merged view's symlink semantics; the link (not its target) is what was committed.
- **Isolation**: n/a.

#### CX-09 — ⏱ interleaved git soak: randomized ops across 2–3 agents
- **Spec**: everything at once; standing invariants. **Fixture**: `repo(committed 3)`; 15 iterations of seeded-random composition across 2–3 sessions: `{commit new, commit edit (conflict-prone), rm, checkout branch, write gitignored, gc}`; finalize order randomized. Seed logged for replay.
- **Steps**: per iteration: mutate across agents → finalize in seeded order → **invariant sweep**; final: destroy all → assert stack.
- **Correctness (standing invariants, ×15)**: every finalize result parses and is either `ok` or a clean `source_conflict` (never a third class, never a panic); `manifest_version` is monotonic and advances exactly by the number of accepted publishes; **no** publish ever advanced a `protected_path`; the ignored route never produced `source_conflict`.
- **Attribution**: after every iteration, `file_blame` on a sampled source path tiles to a real owner; sampled ignored path is wholesale.
- **Isolation**: after every iteration, a fresh exec `git -C /workspace fsck` reports **no corruption**, and `git status` succeeds — the corruption sentinel for the whole soak. Daemon fd count stable (±16) across iterations (leak sentinel).

#### CX-10 — pre-hook enforces destructive-git policy (the design boundary)
- **Spec**: "let the user decide" — safety is the caller's, not layerstack's. **Fixture**: `repo(committed 1)` + an agent/exec **pre-hook** configured to reject commands matching a destructive-git denylist (e.g. `reset --hard`, `clean -fd`, `push --force`).
- **Steps**: run a benign `git commit` (allowed by the hook) → then a `git reset --hard` (blocked by the hook) → inspect both outcomes and the stack.
- **Correctness**: the benign commit lands (`source`, manifest +1); the destructive command is **blocked by the pre-hook** (command rejected before capture), so **layerstack sees no changeset** — `_assert_stack_unchanged` across the blocked op. The reject originates from the hook, not a layerstack `publish_reject_class`.
- **Attribution**: merged tree reflects only the benign commit.
- **Isolation**: proves the policy boundary — layerstack stays git-agnostic; destructive-op prevention is composed above it, exactly as the design intends.

---

## 4. Traceability — contract → cases

| Contract clause | Cases |
| --- | --- |
| `.git` not special-cased (routes as source) | EZ-01, EZ-02, EZ-06, EZ-08 |
| exact-component match only (`.gitignore`/`.github` ordinary) | EZ-04, EZ-05 |
| first-writer-wins (source OCC) | MED-01, CX-02, CX-03 |
| last-writer-wins (ignored, wholesale) | EZ-09, MED-02, MED-07 |
| binary `.git` divergence → clean reject (no corruption) | MED-05, CX-04 |
| text-ref divergence → merge-or-reject | MED-06, CX-03 |
| gitignore knob flips `.git` to ignored | MED-07 |
| mixed-route atomicity | MED-08 |
| base-pinned ignore oracle under race | CX-07 |
| protected paths still reject | EZ-10, MED-10 |
| deletions publish (no destructive-op forbid) | EZ-07, MED-03, MED-04, CX-06 |
| concurrent clobber caught by OCC; settled delete allowed | CX-06 |
| object DB integrity under concurrency/scale | CX-01, CX-04, CX-05, CX-09 |
| line-origin attribution on source paths | EZ-03, CX-01 |
| safety is the caller's (pre-hook) | CX-10 |

---

## 5. Execution order & suite composition

1. **Preconditions** (§1.1 table) — once, hard-fail (git present, identity set).
2. **EZ-01…10** serial (`-m "git and easy"`) — the rebuild gate; ≤ 5 min.
3. **MED-01…10** serial — one interaction/fault each; two-agent cases construct
   both sessions before either finalizes.
4. **CX-01…08, CX-10** serial — each rebuilds its own sandbox; the pre-hook case
   (CX-10) requires the hook fixture wired.
5. **CX-09 soak** — final; owns the corruption-sentinel invariant sweep and the
   fd-leak baseline.
6. **Suite report** generated even on abort; the SUMMARY table plus every
   `git fsck` verdict is the sign-off artifact.

Budget for a full run: easy ≈ 5 min, medium ≈ 15 min, complex ≈ 30–45 min
(CX-05 import and CX-09 soak dominate). Multiagent cases log their finalize
order so a `source_conflict` flake is diagnosable from the bundle alone.
