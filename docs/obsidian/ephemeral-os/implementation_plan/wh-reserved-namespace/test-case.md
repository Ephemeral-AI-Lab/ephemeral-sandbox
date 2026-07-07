---
title: Reserved `.wh.` Namespace — Live-Docker E2E Test Catalog
tags:
  - ephemeral-os
  - layerstack
  - workspace
  - capture
  - testing
status: draft
updated: 2026-07-07
---

# Reserved `.wh.` Namespace — live e2e catalog (16 cases)

Companion to `spec.md` (same folder — design truth) and to the code under
test: `crates/sandbox-runtime/workspace/src/overlay/capture.rs` and
`crates/sandbox-runtime/layerstack/src/stack/publish/route.rs`. This document
defines the **live Docker sandbox** catalog that drives real kernel-overlayfs
sessions, one-shot execs, and sessionless file ops through
`sandbox-manager-cli` / `sandbox-runtime-cli`, asserting on structured JSON —
never log scraping. 6 easy (EZ), 6 medium (MED), 4 complex (CX).

Cases are implemented in
`cli-operation-e2e-live-test/runtime/reserved_paths/`, one file per tier
(`test_wh_reserved_easy.py` / `test_wh_reserved_medium.py` /
`test_wh_reserved_complex.py`, markers `whreserved and easy` / `whreserved
and medium` / `whreserved and complex`), reusing the primitives of the
sibling suites: `runtime/file/helpers.py` (`file_read`, `file_write`,
`file_edit`, `exec_command`, `layerstack`, `assert_ok`, `assert_error`,
`assert_content`, `assert_manifest_delta`) and
`runtime/workspace_session/helpers.py` (session create/finalize,
`wait_finalized`). Every executed case writes
`test-reports/<RUN_ID>/<CASE_ID>/verdict.json`.

## The contract under test

`.wh.`-prefixed path components are a reserved layerstack-internal namespace.
The whole catalog pins four consequences:

1. **Never reinterpreted.** A user-created `.wh.foo` / `.wh..wh..opq` entry
   is never converted into a `Delete` or `OpaqueDir` — base content it would
   have masked stays intact and readable. This is the data-safety heart of
   the catalog.
2. **Fail closed, whole changeset.** Every route (session finalize, one-shot
   exec publish, sessionless file op) rejects the changeset with
   `publish_reject_class: "protected_path"` naming the offending path; the
   stack is byte-identical afterward (`_assert_stack_unchanged`).
3. **Component rule, not basename rule.** `a/.wh.d/f.txt` rejects the same
   as `.wh.foo`; the bare `.wh.` and the literal marker name `.wh..wh..opq`
   reject; `.wh`, `.whx`, `x.wh.y`, `wh.foo` are ordinary paths.
4. **Kernel encodings unaffected.** Real deletion (`rm`, `rm -rf` +
   recreate) still captures, publishes, masks, and squashes exactly as
   before the change.

### Decision table (the heart of the contract)

| Path shape written in-sandbox | Expected outcome | Case |
| --- | --- | --- |
| `.wh.foo` (regular file, session) | finalize `protected_path`; base `foo` survives | EZ-01 |
| `.wh.probe` (one-shot exec) | terminal `publish_rejected: true`, `protected_path`; discarded | EZ-02 |
| `dir/.wh..wh..opq` (session) | reject; lower `dir/**` stays visible | EZ-03 |
| `.wh.foo` (sessionless `file_write`) | structured rejection; stack unchanged | EZ-04 |
| `.wh`, `.whx`, `x.wh.y`, `wh.foo` | publish `ok`, readable | EZ-05 |
| `rm foo` / `rm -rf dir` (real deletes) | publish `ok`; `not_found` after | EZ-06, MED-06 |
| `good.txt` + `.wh.bad` in one changeset | whole changeset rejected; `good.txt` absent | MED-01 |
| `a/.wh.d/f.txt` (nested component) | reject; base `a/d/**` survives | MED-02 |
| `ln -s t .wh.link` | reject | MED-03 |
| bare `.wh.` file | reject; later base materialization intact | MED-04 |
| `.wh.manifest.json` | one clean reject naming `.wh.manifest.json` (never a fabricated delete of `manifest.json`) | MED-05 |
| A normal ∥ B `.wh.`-poisoned (two sessions) | A lands, B rejects whole | CX-01 |
| real deletes → squash → `.wh.` attempt | squash stable; attempt still rejects | CX-02 |
| randomized reserved-name storm | invariants hold ×N | CX-03 |
| reject on a deep stack | prompt reject, clean teardown | CX-04 |

---

## 1. Environment & fixture toolkit

### 1.1 Bring-up

```sh
export PATH="$PWD/bin:$PATH"
bin/start-sandbox-docker-gateway --rebuild-binary

RUN_ID=whres-$(date +%Y%m%d-%H%M%S)
WS=$(mktemp -d /tmp/$RUN_ID-ws.XXXX)

sandbox-manager-cli create_sandbox --image ubuntu:24.04 --workspace-root "$WS" > /tmp/$RUN_ID-create.json
SID=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["id"])' /tmp/$RUN_ID-create.json)
```

One sandbox per case unless the case is explicitly multi-session (CX-01,
CX-03). Serial by default. Base content is seeded either host-side
(`sandbox_from_workspace(files={...})` — write under `tmp_path/workspace`,
then `create_sandbox --workspace-root`) or by a setup publish
(`file_write` / one seed exec), whichever the case names.

**Environment preconditions — asserted once per suite, hard-fail (never
skip):**

| # | Precondition | Check | Expected |
| --- | --- | --- | --- |
| P1 | layer-stack root not on overlayfs | `findmnt -no FSTYPE /eos/layer-stack` | `ext4` |
| P2 | kernel overlayfs accepts `.wh.` names at the VFS layer (the collision is *creatable* — overlayfs, unlike aufs, does not reserve the name) | `exec_command "touch /workspace/.wh.smoke && ls -a /workspace"` **inside a live session that is destroyed without finalizing changes** (or with its reject asserted and swallowed) | `touch` exit 0; `.wh.smoke` listed in the live merged view |
| P3 | in-session deletes produce kernel whiteouts (root-in-container mknod path) | seed `probe.txt`, session `rm probe.txt`, finalize | finalize `ok`; `file_read probe.txt` → `not_found` |

P2 is load-bearing: if the kernel ever refused the name, every reject case
would be vacuous. P3 pins the environment half of invariant 4 before any
case depends on it.

### 1.2 Fixture vocabulary

| Fixture | Meaning | Construction |
| --- | --- | --- |
| `base(F)` | sandbox whose base layer holds files `F` | `sandbox_from_workspace(files=F)` or one seed `file_write`/exec publish before the case body |
| `session` | one live workspace session | `create_workspace_session`; finalize = destroy with capture/publish; result via `wait_finalized` |
| `poisoned(session, P)` | session in which an exec created reserved entry `P` | `exec_command --workspace-session-id … "…"` (`touch`, `printf`, `mkdir -p`, `ln -s` per case) |
| `agentA` / `agentB` | two sessions leased at the same base revision | two `create_workspace_session` before either finalizes |
| `squashed` | stack after `checkpoint_squash` | `sandbox-cli manager checkpoint_squash`, squash-suite pattern |

Shared assertion helpers are imported from the sibling suites
(`assert_ok`, `assert_error`, `assert_content`, `assert_manifest_delta`,
`_assert_publish_rejection`, `_assert_stack_unchanged`,
`assert_exec_workspace_not_found`) or wrapped locally in
`runtime/reserved_paths/helpers.py`.

### 1.3 Teardown contract (part of every case)

After each case, in order — a teardown failure **fails the case loudly**:

1. Destroy every session; destroy the sandbox last.
2. `observability layerstack` shows `active_lease_count == 0` on every layer.
3. `manifest.json` / `root_hash` match the last **accepted** publish — no
   rejected changeset advanced the stack.
4. Artifact bundle written even on failure (`cmd.log`, `result.json`,
   `layerstack.json`, per-case `verdict.json`).

---

## 2. Measurement — the three axes

Every case is verified on three axes; a case passes only when all three pass.

1. **Correctness (reject & stack)** — the operation rejected or landed
   exactly as specified: `publish_rejected` + `publish_reject_class ==
   "protected_path"` (and the offending path where the response carries it)
   on the terminal/finalize response for reject cases; `assert_manifest_delta`
   equals the expected number of accepted layers; `_assert_stack_unchanged`
   across every reject.
2. **Data-safety (the load-bearing axis)** — no silent reinterpretation:
   every base path a marker-shaped name *would* have masked is still
   readable with byte-equal content (`file_read`) **and** visible to a fresh
   exec (`ls` / `cat` in a new one-shot) after the reject. The reserved
   entry itself never appears in the merged view.
3. **Isolation (multi-session)** — for CX cases: the poisoned session's
   reject never disturbs a concurrent session's accepted publish; a fresh
   session sees exactly the accepted content. Single-session cases mark this
   axis `n/a`.

Per-case `verdict.json` (one schema for all 16):

```json
{
  "case_id": "EZ-01",
  "run_id": "whres-20260707-120000",
  "status": "pass",
  "axes": {
    "correctness": { "pass": true, "reject_class": "protected_path", "manifest_delta": 0 },
    "data_safety": { "pass": true, "base_paths_verified": 2, "masked": false },
    "isolation":   { "pass": true }
  },
  "teardown": { "pass": true, "lease_registry_empty": true, "stack_unchanged": true },
  "defects": []
}
```

---

## 3. Test catalog

Per-case format: **Spec** (invariant from `spec.md`) · **Fixture** · **Steps**
· **Correctness** · **Data-safety** · **Isolation**. Every case ends with the
§1.3 teardown contract; it is not repeated.

### 3.1 Easy — EZ-01…06

Single session or single op, one behavior each. Whole tier is the rebuild
gate (`pytest -m "whreserved and easy"`).

#### EZ-01 — a session file named `.wh.foo` never deletes `foo` (THE regression)
- **Spec**: invariants 1+2. **Fixture**: `base({"foo": "keep-me\n"})`, `session`.
- **Steps**: in-session `printf 'payload' > /workspace/.wh.foo` → finalize → `file_read foo` → fresh exec `cat /workspace/foo && ls -a /workspace`.
- **Correctness**: finalize carries `publish_rejected: true`, `publish_reject_class: "protected_path"` (path `.wh.foo` where surfaced); manifest delta 0; `_assert_stack_unchanged`.
- **Data-safety**: `file_read foo` → `keep-me\n` byte-equal; fresh exec sees `foo`, does **not** see `.wh.foo`; `file_read .wh.foo` → `not_found`. (Pre-fix this case fails with `foo` → `not_found`: the silent-delete bug.)
- **Isolation**: n/a.

#### EZ-02 — one-shot exec `touch .wh.probe` rejects and discards
- **Spec**: invariants 2+3 on the command route. **Fixture**: `base({})`.
- **Steps**: bare `exec_command "touch /workspace/.wh.probe"` → inspect terminal response → fresh exec `ls -a /workspace`.
- **Correctness**: terminal response `status: ok` **and** `publish_rejected: true` with class `protected_path`; manifest delta 0.
- **Data-safety**: fresh exec does not list `.wh.probe`; `file_read .wh.probe` → `not_found` (the whole one-shot changeset was discarded).
- **Isolation**: n/a.

#### EZ-03 — a user file named `.wh..wh..opq` never masks its directory
- **Spec**: invariant 1 (opaque half). **Fixture**: `base({"cfg/a.txt": "A\n", "cfg/b.txt": "B\n"})`, `session`.
- **Steps**: in-session `printf 'x' > /workspace/cfg/.wh..wh..opq` → finalize → `file_read cfg/a.txt`, `file_read cfg/b.txt` → fresh exec `ls /workspace/cfg`.
- **Correctness**: finalize rejects `protected_path`; manifest delta 0.
- **Data-safety**: both `cfg/a.txt` and `cfg/b.txt` byte-equal and listed — the directory was never opaque-masked. (Pre-fix: both vanish.)
- **Isolation**: n/a.

#### EZ-04 — sessionless `file_write` to `.wh.foo` rejects without poisoning
- **Spec**: invariant 3 (file-op route); spec Open Question 1 pins the exact response kind here. **Fixture**: `base({"foo": "keep\n"})`.
- **Steps**: `file_write ".wh.foo" "evil"` → assert structured error → `file_read foo` → `file_edit ".wh.bar" [...]` (edit route, same expectation).
- **Correctness**: both operations return the structured rejection (protected-path class per the implemented mapping — pinned exactly at implementation time); manifest delta 0.
- **Data-safety**: `file_read foo` still `keep\n` — no stored `.wh.foo` marker masks it (pre-fix the write *succeeds* and `foo` becomes `not_found`: store poisoning).
- **Isolation**: n/a.

#### EZ-05 — lookalike names are ordinary paths
- **Spec**: invariant 5. **Fixture**: `base({})`, `session`.
- **Steps**: one in-session exec writing `.wh`, `.whx`, `x.wh.y`, `wh.foo` (four files) → finalize → read all four.
- **Correctness**: finalize `ok`, not rejected; manifest +1; route summary shows 4 source writes.
- **Data-safety**: all four `file_read` byte-equal; all listed by a fresh exec.
- **Isolation**: n/a.

#### EZ-06 — real deletion still works end to end
- **Spec**: invariant 4 (the fix didn't break kernel-whiteout capture). **Fixture**: `base({"victim.txt": "doomed\n"})`, `session`.
- **Steps**: in-session `rm /workspace/victim.txt` → finalize → `file_read victim.txt` → fresh exec `ls -a /workspace`.
- **Correctness**: finalize `ok`; manifest +1 (the delete published).
- **Data-safety**: `file_read victim.txt` → `not_found`; fresh exec does not list it; **no literal `.wh.victim.txt` appears in any view** (marker encoding stays internal).
- **Isolation**: n/a.

### 3.2 Medium — MED-01…06

One interaction dimension per case.

#### MED-01 — mixed changeset is atomic: one reserved name discards everything
- **Spec**: invariant 2 (whole-changeset). **Fixture**: `base({})`, `session`.
- **Steps**: in-session `printf 'good' > /workspace/good.txt && printf 'bad' > /workspace/.wh.bad` → finalize → `file_read good.txt`.
- **Correctness**: finalize rejects `protected_path`; manifest delta 0.
- **Data-safety**: `good.txt` → `not_found` (atomically discarded with the changeset — no partial publish); `.wh.bad` absent everywhere.
- **Isolation**: n/a.

#### MED-02 — the rule is per-component, not per-basename
- **Spec**: invariant 2 component rule. **Fixture**: `base({"a/d/keep.txt": "K\n"})`, `session`.
- **Steps**: in-session `mkdir -p /workspace/a/.wh.d && printf 'x' > /workspace/a/.wh.d/f.txt` → finalize → `file_read a/d/keep.txt` → fresh exec `ls /workspace/a`.
- **Correctness**: finalize rejects `protected_path` (offending path carries the `.wh.d` component); manifest delta 0.
- **Data-safety**: `a/d/keep.txt` byte-equal and `a/d` listed — a stored dir named `.wh.d` would have masked the sibling `d` subtree; it never lands.
- **Isolation**: n/a.

#### MED-03 — a symlink named `.wh.link` rejects
- **Spec**: invariant 2 applies to every change kind. **Fixture**: `base({})`, `session`.
- **Steps**: in-session `ln -s target /workspace/.wh.link` → finalize.
- **Correctness**: finalize rejects `protected_path`; manifest delta 0; `_assert_stack_unchanged`.
- **Data-safety**: `.wh.link` absent from merged view and fresh exec listing.
- **Isolation**: n/a.

#### MED-04 — the bare `.wh.` file rejects; base materialization stays intact
- **Spec**: invariants 2+6 (the F5 projection landmine can no longer be planted). **Fixture**: `base({"root.txt": "R\n", "sub/leaf.txt": "L\n"})`, `session`.
- **Steps**: in-session `touch '/workspace/.wh.'` → finalize (reject) → open a **fresh session** and read both base files (and, where the harness supports a daemon restart, restart to re-run base materialization before reading).
- **Correctness**: finalize rejects `protected_path`; manifest delta 0.
- **Data-safety**: in the fresh view, `root.txt` and `sub/leaf.txt` are byte-equal — nothing resembling the pre-fix `remove_dir_all` wipe is reachable because the entry never published. (The projection-side guard itself is pinned by the unit test `bare_wh_layer_entry_projects_as_file_not_directory_clear`, since post-fix e2e can no longer plant the entry.)
- **Isolation**: n/a.

#### MED-05 — `.wh.<protected-name>` is one clean reject, not a fabricated protected delete
- **Spec**: F3 regression; the reject must name the path the user actually created. **Fixture**: `base({})`, `session`.
- **Steps**: in-session `printf 'x' > /workspace/.wh.manifest.json` → finalize → inspect the rejection payload.
- **Correctness**: exactly one `protected_path` reject whose path is `.wh.manifest.json` (pre-fix: capture fabricated `Delete { manifest.json }` and the reject named a path the user never wrote); manifest delta 0.
- **Data-safety**: stack files (`manifest.json` at the storage root) untouched — `observability layerstack` healthy.
- **Isolation**: n/a.

#### MED-06 — `rm -rf` + recreate (opaque/whiteout kernel flow) still round-trips
- **Spec**: invariant 4, directory-shaped. **Fixture**: `base({"reports/daily/r1.txt": "r1"})`, `session`.
- **Steps**: in-session `rm -rf /workspace/reports && mkdir -p /workspace/reports/daily && printf 'r2' > /workspace/reports/daily/r2.txt` → finalize → read `r1`/`r2` → fresh exec `ls -a /workspace/reports/daily`.
- **Correctness**: finalize `ok`; manifest +1.
- **Data-safety**: `r1.txt` → `not_found`, `r2.txt` → `r2`; no `.wh.*` names in any listing.
- **Isolation**: n/a.

### 3.3 Complex — CX-01…04

Multi-session, interplay with squash, storm, scale.

#### CX-01 — poisoned session B never disturbs concurrent session A
- **Spec**: invariant 2 + isolation. **Fixture**: `base({"shared.txt": "base\n"})`; `agentA`, `agentB` at the same base revision.
- **Steps**: A appends to `shared.txt`; B writes `/workspace/.wh.shared.txt`; finalize A (lands) then B (rejects) → reads → fresh session `ls -a`.
- **Correctness**: A's finalize `ok` (manifest +1); B's finalize `publish_rejected: true`, `protected_path`; net manifest delta exactly 1.
- **Data-safety**: merged `shared.txt` equals A's content; `.wh.shared.txt` absent.
- **Isolation**: a fresh session/exec sees exactly A's world; B's reject left no residue (`_assert_stack_unchanged` relative to post-A state).

#### CX-02 — squash interplay: internal markers squash cleanly, the reservation survives squash
- **Spec**: invariants 4+6+7 across `checkpoint_squash`. **Fixture**: `base({"d/x.txt": "X", "d/y.txt": "Y", "solo.txt": "S"})`.
- **Steps**: publish real churn via execs (delete `solo.txt`; `rm -rf d` + recreate `d/new.txt`) → `checkpoint_squash` → verify merged view (deleted stay deleted, `d/new.txt` readable, no `.wh.*` visible) → then in a fresh session attempt `.wh.z` → finalize.
- **Correctness**: squash result `ok`; post-squash attempt still rejects `protected_path` — the gate is stack-shape-independent.
- **Data-safety**: post-squash reads identical to pre-squash merged view; no marker name ever surfaces via `file_read`/`ls`.
- **Isolation**: n/a (single session at a time; squash is the interleaved actor).

#### CX-03 — reserved-name storm: standing invariants ×10
- **Spec**: all invariants at once. **Fixture**: `base({"k0.txt": "0", …, "k9.txt": "9"})`; 10 seeded-random iterations (seed logged for replay) across 1–2 sessions.
- **Steps**: per iteration draw one poison shape from `{.wh.<name>, dir/.wh..wh..opq, a/.wh.d/f, symlink .wh.l, bare .wh.}` **and** one legitimate publish (normal write or real `rm`); interleave; finalize both; sweep.
- **Correctness (standing, ×10)**: every poisoned finalize rejects with class `protected_path` — never a second class, never a panic, never an `internal_error`; `manifest_version` advances by exactly the accepted count.
- **Data-safety (standing, ×10)**: after every iteration, every still-live base file `k*.txt` reads byte-equal; every legitimately deleted path is `not_found`; no `.wh.*` name in any listing.
- **Isolation**: daemon healthy across the storm (fd count stable ±16; a fresh exec succeeds after every iteration).

#### CX-04 — reject on a deep stack is prompt and clean
- **Spec**: invariant 2 at depth; teardown contract under load. **Fixture**: 50 accepted layers published up front (structured `file_write` loop, squash-suite pattern), then `session`.
- **Steps**: in-session `touch /workspace/.wh.deep` → finalize → timers.
- **Correctness**: reject `protected_path`; manifest delta 0 on a 50-layer manifest.
- **Data-safety**: spot-check 3 sampled deep-layer files byte-equal after the reject.
- **Isolation**: teardown contract (lease registry empty, staging clean) holds despite depth; finalize-to-reject latency recorded in `verdict.json` (informational, no hard bound in v1).

---

## 4. Traceability — spec invariant → cases

| Spec invariant | Cases |
| --- | --- |
| 1 — no fabricated deletes/opaques from names | EZ-01, EZ-03, MED-05 |
| 2 — fail closed, whole changeset, `protected_path` | EZ-01…04, MED-01, MED-03…05, CX-01, CX-03, CX-04 |
| 3 — every route gated (session / exec / file op) | EZ-01, EZ-02, EZ-04 |
| 4 — kernel delete/opaque encodings still work | EZ-06, MED-06, CX-02, P3 |
| 5 — lookalikes unaffected | EZ-05 |
| 6 — readers unchanged; bare-`.wh.` landmine dead | MED-04, CX-02 |
| 7 — marker purity after the change | EZ-06, MED-06, CX-02 (no `.wh.*` ever user-visible) |
| component rule (not basename) | MED-02 |
| atomic discard | MED-01, CX-01 |
| concurrency isolation | CX-01, CX-03 |
| squash interplay | CX-02 |
| depth/teardown | CX-04 |

Environment preconditions P2/P3 gate the suite; they are assertions, not
cases.

## 5. Execution order & suite composition

1. **Preconditions** (§1.1 table) — once, hard-fail.
2. **EZ-01…06** serial (`-m "whreserved and easy"`) — the rebuild gate;
   EZ-01 first (it is the bug). Budget ≤ 3 min.
3. **MED-01…06** serial — one interaction each. Budget ≤ 5 min.
4. **CX-01, CX-02, CX-04** serial — each rebuilds its own sandbox.
5. **CX-03 storm** — last; owns the standing-invariant sweep and the
   fd-leak sentinel. Budget ≤ 10 min including setup publishes.
6. **Suite report** generated even on abort: `SUMMARY.md` plus every
   `verdict.json` under `test-reports/<RUN_ID>/` is the sign-off artifact.

**Pre-fix expectation (red-first discipline):** run EZ-01, EZ-03, EZ-04
against the unfixed daemon and record the failing verdicts (silent delete,
opaque mask, store poisoning) in the first `test-reports/` bundle. The catalog
is written so those three fail *on data-safety* before the fix and pass
after; the remaining cases must pass on both sides except where noted
(EZ-02/MED-01/MED-05 change reject shape, EZ-05/EZ-06/MED-06 are
pure regressions guards that must never change).
