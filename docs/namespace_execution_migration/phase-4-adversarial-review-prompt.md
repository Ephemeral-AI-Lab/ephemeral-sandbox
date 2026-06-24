# Phase 4 — Adversarial Review Prompt (completeness · correctness · cleanliness)

**Use this to drive an adversarial, multi-agent review of the Phase 4
namespace-execution migration ("Mount family onto the engine").** The
implementation is assumed to be already in-tree. The review's job is to *break*
it, not bless it. Default mode is **read-only**: find defects, adversarially
verify each, and report a verdict — do not edit unless the human explicitly
enables the fix lane (§9).

Authoritative source of truth: [`phase-4-spec.md`](./phase-4-spec.md). Design of
record: [`../namespace-execution.md`](../namespace-execution.md). Phasing:
[`./migration-phases.md`](./migration-phases.md). Where this prompt and the spec
disagree, **the spec wins** and the disagreement is itself a finding.

---

## 1. Mission & stance

You are a panel of adversarial reviewers. Your incentive is to find real
defects. A reviewer who returns "looks good" has failed unless they first tried
hard to break it and show their attack attempts. Rules of engagement:

- **Refute-first.** For every claim of correctness, attempt to construct an input,
  ordering, platform, or error path that violates it. Report the attack and
  whether it succeeded.
- **Evidence or it didn't happen.** Every finding cites `path:line`, quotes the
  exact code, and quotes the spec/invariant clause it violates (or the cleanliness
  rule from `CLAUDE.md`). No vibes, no "could be cleaner" without a concrete edit.
- **Kill your own false positives.** Before filing, check whether the compiler,
  `clippy -D warnings`, an existing test, or a *documented* spec decision already
  covers it. If so, drop it (or downgrade to nit).
- **Stay in scope.** In-scope crates: `workspace`, `daemon`, plus *read-only*
  inspection of `namespace-execution` (the Phase-2 engine it depends on) to the
  extent Phase 4's correctness rests on it. Do **not** file findings against the
  in-namespace runner (`namespace-process`) or the wire protocol — Phase 4 leaves
  them untouched by mandate (spec §1.3).

---

## 2. Ground-truth crib (so reviewers don't re-derive it)

### 2.1 Files Phase 4 changed
- `crates/sandbox-runtime/workspace/Cargo.toml` — engine dep (P4-R1).
- `crates/sandbox-runtime/workspace/src/model.rs` — `From<WorkspaceEntry> for NamespaceTarget` (P4-R2).
- `crates/sandbox-runtime/workspace/src/namespace/mod.rs` — `NamespaceRuntime` owns `Arc<NamespaceExecutionEngine>` + `NoopObserver`, `new(setup_timeout_s)`, `MOUNT_MAX_ACTIVE` (P4-R8).
- `crates/sandbox-runtime/workspace/src/namespace/setns_runner.rs` — two `run_mount(...).wait()` call sites; six helpers + builder + `isolated-…` id deleted (P4-R3..R7).
- `crates/sandbox-runtime/workspace/src/profile/manager.rs` — threads `caps.setup_timeout_s` (P4-R8/R12).
- `crates/sandbox-runtime/workspace/src/lifecycle/create.rs` + `.../lifecycle/remount/transaction.rs` — call sites drop the timeout arg (P4-R12).
- `crates/sandbox-daemon/src/runner.rs` — `MountOverlay` failure → `RunResult.payload` via `pub(crate) mount_overlay_result`; `dispatch_runner_mode` param `mode`→`operation` (P4-R9/R14); start-ack untouched (P4-R18); `#[cfg]`-free (P4-R17).
- Tests: `workspace/tests/setns_runner.rs` deleted; daemon **D1** in `tests/unit/runner.rs`; workspace **W1–W5** in `tests/`.

### 2.2 The failure-signaling contract (the riskiest seam — §3.3)
Trace these chains link-by-link and attack each link:

- **Mount success:** daemon `ok_result()` ⇒ `exit_code 0` → launcher reads `RunResult`
  → `RunnerOutcome.exit_code()==0` → no short-circuit → closure `|_| Ok(())` →
  promise `Ok(())` → `wait()` `Ok(())`.
- **Mount failure:** daemon `mount_overlay_result(Err(e))` ⇒ `exit_code 1` +
  `{"error":…}` → launcher reads it → `RunnerOutcome.exit_code()==1` → engine
  `MountExitHandling::ShortCircuitNonZero` fires **before** the closure →
  `NamespaceExecutionError::Finalize` → promise `Err` → `wait()` `Err` →
  `setup_error` → `WorkspaceModeError::SetupFailed`.
- **Remount verification failure:** daemon `RemountOverlay` writes `exit_code 0` +
  report → closure `from_payload` → `Ok(RemountOverlayResult{mount_verified:false})`
  → `wait()` `Ok` → `apply_remount` inspects the flag and returns `SetupFailed`
  (the *caller* errors, not `wait()`).
- **Remount syscall error:** daemon `?`-propagates → child exits non-zero, **no**
  `RunResult` → launcher `synthesize_result` (`exit_code` from code/signal, payload
  `{status:error}`) → non-zero → short-circuit → `wait()` `Err`.

### 2.3 Non-negotiables (§3.1/§3.3) — a violation of any is **critical**
1. mount failure ⇒ `wait()` `Err`.
2. remount verification failure stays `Ok(mount_verified=false)`; only the caller
   (`apply_remount`) turns the flag into an error.
3. mount/remount **never** appear in `active_namespace_executions` — guaranteed
   structurally by the no-op observer and the absence of any `begin` call below
   operation. No new pseudo-`operation_name`, no new rows.
4. the engine crate keeps **zero** `workspace` dependency.

### 2.4 §10 verification commands (a reviewer must actually run these)
```sh
export PATH="$PWD/bin:$PATH"
cargo fmt --check
cargo build -p sandbox-runtime-workspace && cargo test -p sandbox-runtime-workspace
cargo build -p sandbox-daemon && cargo test -p sandbox-daemon --test unit
cargo test -p sandbox-runtime observability
cargo test -p sandbox-runtime --tests
cargo clippy -p sandbox-runtime-workspace --all-targets --no-deps -- -D warnings
cargo clippy -p sandbox-daemon            --all-targets --no-deps -- -D warnings
cargo test -p xtask --test cfg_policy
rg -n "fn run_child|fn ns_runner_request|fn wait_for_child|fn terminate_child|fn read_pipe" crates/sandbox-runtime/workspace/src   # expect: nothing
rg -n "isolated-" crates/sandbox-runtime/workspace/src                                                                            # expect: nothing
cargo tree -p sandbox-runtime-namespace-execution -e normal | rg -q "sandbox-runtime-workspace" && echo CYCLE || echo "no workspace dep ✓"
```

### 2.5 Known-expected — do NOT file as defects (but you MAY challenge with evidence)
- The **unscoped** grep `fn (run_child|…|wait_for_child|terminate_child|read_pipe)`
  matches `wait_for_child_with_timeout` / `terminate_child` in
  `namespace-execution/src/launcher.rs`. Those are **Phase-2 engine** helpers (the
  §12-G SIGTERM→SIGKILL timeout that was *moved* into the launcher), not the deleted
  workspace functions. The spec's grep is scoped to `workspace/src` and returns
  nothing. Only file a finding if the *workspace* copies survive, or if the engine's
  escalation does **not** actually match the old behavior.
- `active_commands` in `workspace/src/error.rs` and `operation/.../workspace_remount/`
  is pre-existing remount-quiesce code, not a Phase-4 classification axis.
- Real overlay-mount / live-remount **success** (W1/W2 happy path) is owned by the
  Linux e2e/live suite, not a dev-host unit test (spec §9.2). Absence of a real-syscall
  unit test is *not* a finding; absence of coverage for the *contract* (mount-failure→`Err`,
  remount `Ok(false)`, observability) **is** a candidate finding — see lane COMPLETENESS.
- Two engine instances (workspace-local mount engine vs. command engine) is a
  **documented decision** (§3.2/§12-C). Challenge only if the implementation
  contradicts that rationale (e.g., it actually shares the command admission pool).

---

## 3. The three axes (what counts as a defect)

**COMPLETENESS** — something the spec mandates is missing, partial, or stubbed.
- Any of P4-R1..R19 not realized in code.
- A required test (W1–W5, D1) absent, or present-but-vacuous (asserts nothing that
  could fail, e.g. tests the `From` conversion but never exercises the
  mount-failure→`Err` or remount-`Ok(false)` contract at any layer).
- A deletion mandated but not done (helper, builder, `isolated-…` id, obsolete test).
- A call site / non-Linux stub / signature not updated consistently.

**CORRECTNESS** — behavior diverges from the spec's contract or from pre-Phase-4 behavior.
- Any non-negotiable (§2.3) broken.
- The failure chains (§2.2) breakable at any link.
- The `entry.layer_paths = layer_paths.to_vec()` override not behavior-equivalent to
  the old `ns_runner_request` (which set `layer_paths` from the caller param, not the
  launch context) — check both mount and remount, and the empty-slice case.
- `From<WorkspaceEntry>`: `upperdir`/`workdir` must be `Some`, `ns_fds` fully populated,
  no field dropped/swapped.
- Timeout MOVE: `caps.setup_timeout_s` actually reaches the piped wait; escalation
  matches the old SIGTERM→100ms grace→SIGKILL→reap and the old message shape.
- Non-Linux stub: `remount_overlay` returns a `RemountOverlayResult::default()` whose
  `mount_verified` does not spuriously trip `apply_remount`.
- `allocate_id` change (`isolated-…` → `namespace_execution_N`): no surviving consumer
  depended on the old id shape (logs, diagnostics, persisted state).

**CLEANLINESS** — violates `CLAUDE.md` or SOLID/SRP/"prefer less".
- Inline comments in production code (only `///`/`//!` allowed); test code under `src/`;
  `#[cfg(test)]` in sources; `#[cfg]` in daemon sources.
- Dead/unused imports or now-orphaned helpers left behind (e.g. `ns_fds_from_mode`
  imported but unused in `setns_runner.rs`).
- Redundant indirection, needless clones beyond the one acknowledged in §4.2,
  duplicated fds-mapping (the spec forbids a second mapping), a field/method that
  duplicates an existing responsibility.
- Naming/idiom drift from the surrounding file; `MOUNT_MAX_ACTIVE` magic without
  rationale; mutate-then-convert smell in the call sites if a cleaner form exists.

Severity rubric: **critical** (non-negotiable broken / unsound / data-loss),
**high** (contract divergence, missing mandated behavior or its only test),
**medium** (completeness gap with low blast radius, latent correctness risk),
**low/nit** (cleanliness, naming, redundancy). Every finding must include a
**concrete fix** (the edit you'd make) and a **confidence** (0–1).

---

## 4. Review lanes (the fan-out)

Run these lanes independently and concurrently. Each lane is blind to the others.
Each returns a list of *candidate* findings in the §6 schema. Lane mandates below
are attack checklists, not exhaustive — go beyond them.

- **L1 — COMPLETENESS / requirements trace.** Build the P4-R1..R19 → code map; mark
  each Done / Partial / Missing with evidence. Verify every §2.1 edit landed and every
  mandated deletion happened (run the absence greps). Audit the test inventory: do
  W1–W5 + D1 exist, and does each actually exercise its contract? Specifically hunt:
  *is the mount-failure→`Err` path tested anywhere? is remount `Ok(false)` tested at a
  layer that drives `apply_remount`? is "no mount row" asserted?* If those need a fake
  `NsRunnerLauncher` injected into the workspace engine, check whether `NamespaceRuntime`
  even exposes an injection seam — if not, say so (it bounds what's testable).

- **L2 — CORRECTNESS / failure-signaling contract.** Walk every chain in §2.2 and try to
  break each link. Adversarial cases: daemon writes `exit_code 1` but the launcher can't
  read the result fd; remount runner emits `exit_code != 0` *with* a report (does the
  short-circuit wrongly swallow `mount_verified`?); `from_payload` on a missing/garbage
  payload; the synthesized-result fallback's exit code for both modes; panic inside the
  parse closure. Confirm `Err`-vs-`Ok(false)` lands exactly where §2.3 says.

- **L3 — CORRECTNESS / boundary & timeout.** Attack `From<WorkspaceEntry>` (field mapping,
  `Some` wrapping, `ns_fds` completeness), the `layer_paths` override behavior-equivalence
  (mount and remount; empty slice; does it shadow the launch-context paths exactly as the
  old builder did?), the `entry()` early-failure equivalence (§4.3), and the timeout MOVE
  (value provenance `caps → new → engine → spawn_piped`; escalation parity; message shape;
  the fact that `signal_net_ready` keeps its own `setup_timeout_s`).

- **L4 — CORRECTNESS / invariants & boundaries.** Prove or break the four non-negotiables
  (§2.3): grep for any `begin`/ledger/`mark_namespace_execution`/`snapshot_active` reachable
  from the mount path; confirm `NoopObserver` is the only observer wired; confirm
  `--start-ack-fd`/`wait_for_start_ack` byte-identical to pre-Phase-4; confirm daemon
  sources `#[cfg]`-free; confirm `cargo tree` shows no engine→workspace edge; confirm the
  observability snapshot gains no new row/axis.

- **L5 — CLEANLINESS / CLAUDE.md & SOLID.** Read every changed hunk for inline comments in
  prod, test code in `src/`, dead imports, leftover helpers, duplicated mapping, redundant
  indirection, naming drift, and "prefer less" violations. Run `clippy -D warnings` and
  `fmt --check` and treat any output as findings. Judge whether `mount_overlay_result`,
  the `MOUNT_MAX_ACTIVE` constant, and the mutate-then-convert call sites are the simplest
  correct shape.

- **L6 — BUILD/REGRESSION (gate).** Actually execute §2.4. Report any non-green command
  verbatim. This lane's pass is a precondition for trusting the others; a red build here
  outranks every stylistic finding.

> A dynamic run may add lanes (e.g. split L2 by mode) or spawn extra finders until a
> completeness critic (§5) reports nothing new for two consecutive rounds.

---

## 5. Adversarial verification (kill false positives) + completeness critic

1. **Per-finding verification.** For each candidate finding, spawn 2–3 *independent*
   skeptics, each with a distinct lens, prompted to **refute** it (default to
   `refuted=true` when unsure):
   - *spec lens*: "Quote the spec clause. Does the code actually violate it, or did the
     finder misread the spec / a known-expected item (§2.5)?"
   - *mechanism lens*: "Construct the concrete failing input/ordering/platform. Does it
     truly reach the bug, or does a compiler/clippy/test/early-return already prevent it?"
   - *equivalence lens* (for correctness): "Compare against pre-Phase-4 behavior via git
     history. Is this a real divergence or behavior-preserving?"
   Keep the finding only if a majority **fail to refute** it. Record the surviving
   rationale and drop the rest (optionally retain as nits).

2. **Completeness critic (loop-until-dry).** After verification, one critic asks: *what
   did we not look at?* — an untouched changed hunk, an unverified contract link, a test
   whose failure mode was never asserted, a platform path (non-Linux) not walked. Anything
   it surfaces seeds another finder round. Stop when two consecutive critic rounds find
   nothing new. **Log what was deferred or capped** — never imply full coverage you didn't
   achieve.

---

## 6. Finding schema (every reviewer returns this)

```json
{
  "findings": [
    {
      "id": "L2-001",
      "axis": "correctness",                  // completeness | correctness | cleanliness
      "severity": "critical",                 // critical | high | medium | low
      "title": "one line",
      "location": "crates/.../file.rs:120-128",
      "evidence": "exact quoted code",
      "violates": "spec §3.3 / P4-R10 / CLAUDE.md rule — quoted",
      "attack": "the concrete input/ordering/platform that triggers it (or 'static: …')",
      "why_real": "why the compiler/clippy/tests/known-expected do NOT already cover it",
      "fix": "the concrete edit",
      "confidence": 0.0
    }
  ]
}
```

Verifier verdict schema:
```json
{ "id": "L2-001", "refuted": false, "lens": "mechanism", "reason": "…", "confidence": 0.0 }
```

---

## 7. Final synthesis & verdict

A single synthesizer merges all **surviving** findings (deduped by location+axis),
then emits:

- **Verdict:** `ship` | `ship-with-nits` | `fix-required` | `blocked`.
  `fix-required`/`blocked` iff any surviving critical/high finding.
- **Findings table** sorted by severity, each with location, the violated clause, and the fix.
- **Requirements coverage:** P4-R1..R19 → Done/Partial/Missing, and W1–W5/D1 → present &
  meaningful / present-but-vacuous / missing.
- **Contract assurance:** the §2.2 chains and the four non-negotiables → upheld/broken,
  each with the evidence that settled it.
- **§10 results:** every command's pass/fail, verbatim on failure.
- **Coverage gaps / deferrals:** what was capped, sampled, or not reached.

---

## 8. Run mode A — dynamic Workflow (script skeleton)

Hand this to the `Workflow` tool (edit lanes/rounds to taste). It pipelines each lane
through review→verify, runs a completeness critic loop, then synthesizes. Read-only.

```javascript
export const meta = {
  name: 'phase4-adversarial-review',
  description: 'Adversarial review of Phase 4 (completeness/correctness/cleanliness) with per-finding verification',
  phases: [
    { title: 'Gate' }, { title: 'Review' }, { title: 'Verify' },
    { title: 'Critic' }, { title: 'Synthesize' },
  ],
}

const FINDINGS = { type: 'object', properties: { findings: { type: 'array', items: { type: 'object',
  properties: { id:{type:'string'}, axis:{type:'string'}, severity:{type:'string'}, title:{type:'string'},
    location:{type:'string'}, evidence:{type:'string'}, violates:{type:'string'}, attack:{type:'string'},
    why_real:{type:'string'}, fix:{type:'string'}, confidence:{type:'number'} },
  required:['id','axis','severity','title','location','violates','fix','confidence'] } } }, required:['findings'] }
const VERDICT = { type:'object', properties:{ id:{type:'string'}, refuted:{type:'boolean'},
  lens:{type:'string'}, reason:{type:'string'}, confidence:{type:'number'} }, required:['id','refuted','reason'] }

const BRIEF = 'Read docs/namespace_execution_migration/phase-4-adversarial-review-prompt.md and phase-4-spec.md. ' +
  'Be adversarial: try to break the implementation; cite path:line; quote the violated clause; respect the ' +
  'known-expected list (§2.5). Return findings in the §6 schema.'

const LANES = [
  { key:'L1-completeness', m:'Lane L1 COMPLETENESS: trace P4-R1..R19 to code, run the absence greps, audit whether W1–W5/D1 exist AND exercise their contract (esp. mount-failure→Err, remount Ok(false), no mount row), and whether NamespaceRuntime exposes a launcher-injection seam.' },
  { key:'L2-contract', m:'Lane L2 CORRECTNESS: walk every failure chain in §2.2 and break each link (unreadable result fd; remount exit_code!=0 with report; bad payload to from_payload; synthesized-result exit code; closure panic). Confirm Err-vs-Ok(false) lands exactly per §2.3.' },
  { key:'L3-boundary', m:'Lane L3 CORRECTNESS: attack From<WorkspaceEntry> field mapping, the layer_paths override behavior-equivalence (mount+remount, empty slice) vs git history, entry() early-failure equivalence, and the timeout MOVE (provenance + SIGTERM→SIGKILL parity + message shape).' },
  { key:'L4-invariants', m:'Lane L4 CORRECTNESS: prove/break the four non-negotiables (§2.3) — no begin/ledger reachable from mount; NoopObserver only; start-ack byte-identical; daemon #[cfg]-free; engine has zero workspace dep; observability gains no row/axis.' },
  { key:'L5-clean', m:'Lane L5 CLEANLINESS: inline comments in prod, test code in src/, dead imports/helpers, duplicated fds mapping, redundant indirection, naming/idiom drift, prefer-less. Run clippy -D warnings and fmt --check; treat output as findings.' },
]

phase('Gate')
const gate = await agent(BRIEF + ' Lane L6 BUILD/REGRESSION: execute every command in §2.4 and report each pass/fail verbatim. Return findings only for non-green results.', { label:'gate:build', phase:'Gate', schema: FINDINGS })

phase('Review')
const reviewed = await pipeline(
  LANES,
  lane => agent(`${BRIEF}\n\n${lane.m}`, { label: `review:${lane.key}`, phase: 'Review', schema: FINDINGS }),
  (r, lane) => parallel((r?.findings ?? []).map(f => () =>
    parallel(['spec','mechanism','equivalence'].map(lens => () =>
      agent(`Refute this Phase-4 review finding via the ${lens} lens; default refuted=true if unsure. Use git history for equivalence. Finding: ${JSON.stringify(f)}`,
        { label: `verify:${f.id}`, phase: 'Verify', schema: VERDICT })))
      .then(vs => ({ finding: f, survives: vs.filter(Boolean).filter(v => !v.refuted).length >= 2, votes: vs.filter(Boolean) }))))
)

const survivors = reviewed.flat().filter(Boolean).filter(x => x.survives).map(x => x.finding)
  .concat((gate?.findings ?? []))

phase('Critic')
const critic = await agent(`${BRIEF} COMPLETENESS CRITIC: given these surviving findings, name what was NOT reviewed — an untouched changed hunk, an unverified contract link, a test whose failure mode was never asserted, the non-Linux path. Findings: ${JSON.stringify(survivors)}`,
  { label:'critic', phase:'Critic', schema: FINDINGS })

phase('Synthesize')
const verdict = await agent(`Synthesize the Phase-4 adversarial review per §7 (verdict, findings table by severity, P4-R1..R19 coverage, W1–W5/D1 meaningfulness, contract assurance for §2.2 + the four non-negotiables, §10 results, coverage gaps). Surviving findings: ${JSON.stringify(survivors)}. Critic additions: ${JSON.stringify(critic?.findings ?? [])}.`,
  { label:'synthesize', phase:'Synthesize' })

return verdict
```

## 8b. Run mode B — manual multi-agent fan-out

If not using `Workflow`: spawn one `Agent` (subagent) per lane L1–L6 **in a single
message** (concurrent), each given §1–§6 plus its lane mandate from §4 and the §6
output schema. Collect their candidate findings; for each, spawn 2–3 verifier agents
(§5.1) with the spec/mechanism/equivalence lenses to refute. Run one completeness-critic
agent (§5.2); if it surfaces new ground, fan out another finder round. Finally, one
synthesizer agent produces the §7 verdict. Use the `Explore` agent for read-only sweeps
and `general-purpose`/`claude` for analysis lanes.

---

## 9. Optional fix lane (OFF by default)

Only if the human says "apply fixes": after synthesis, pipeline each surviving
**critical/high** finding through a fixer agent (one per finding, `isolation: 'worktree'`
if they touch the same files), then re-run §2.4 to confirm green, then re-review the diff.
Never auto-fix nits. Never commit or push unless explicitly told. Respect the parallel-worker
rule in `CLAUDE.md`: additive, localized edits; never revert work you didn't make.
