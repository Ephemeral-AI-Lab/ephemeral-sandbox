# Agent-Core Rust Migration — Implementation Conformance Review

> Independent verification of the `agent-core/` Rust workspace against the plan
> (`overview.md` + `spec-conventions.md` anchor + the 15 `impl-eos-*.md` specs).
> Method: parallel per-crate review agents (spec ↔ Rust ↔ Python source), each
> finding re-verified by hand for the actionable/contradictory ones. Date: 2026-06-03.

## 0. Scope boundary (important)

"Remove unused / backward-compatible / legacy code" was scoped to the **Rust
`agent-core` crates only**. The Python `backend/src` tree is **untouched**: Phase 7
(cutover) is `NOT STARTED`, so Python is still the live system — deleting it now
*would be* the cutover, which is out of scope. The "legacy" markers inside the Rust
crates (`thinking`→`Reasoning` serde alias, `LEGACY_ENV_MAP`, env-placeholder
resolution, recovery state machines) are **required cutover-parity ports**, not
removable legacy, and were verified as faithful.

## 1. Gate status (after fixes)

| Gate | Result |
|---|---|
| `cargo fmt --check` | ✅ clean |
| `cargo clippy --workspace --all-targets -D warnings` | ✅ clean |
| `cargo test --workspace` | ✅ 360 passed / 0 failed / 0 ignored |

The workspace lints (`spec-conventions.md` §7–9: `unwrap_used`, `await_holding_lock`,
`unsafe_code=forbid`, `#[non_exhaustive]` pedantic set, etc.) are enforced and green,
so the "rust requirement" is satisfied at the gate level. All 15 production crates
plus the Rust-only `workspace-guard` test member build; Phase 0–6 are implemented;
**Phase 7 (cutover) is not started** (expected).

## 2. Per-crate conformance matrix

`layout` = `src/*.rs` vs spec §4 · `contracts` = owned traits/types per spec §5 ·
`ac` = spec §11 ACs have proving tests · `majors` = blocker/major findings.

| Crate | layout | contracts | ac | majors | Verdict |
|---|---|---|---|---|---|
| eos-types | full | all | full | 1 | Conformant; 1 spec-scope decision (CoreError::Store) |
| eos-config | minor | all | full | 0 | Conformant; +`markdown.rs` (authorized, spec doc stale) |
| eos-state | full | all | full | 0 | Conformant |
| eos-audit | full | all | full | 0 | Conformant |
| eos-sandbox-api | full | all | full | 1 | Conformant; **`exec_stdin` wire-string decision** |
| eos-agent-def | full | all | full | 0 | Conformant |
| eos-llm-client | full | all | full | 0 | Conformant |
| eos-skills | full | all | full | 0 | Conformant |
| eos-db | full | all | partial | 1* | Conformant; *eos-db-01 is a false major (see §4) |
| eos-sandbox-host | minor | all | partial | 0 | Conformant; `BackgroundManager` off-map seam (justified) |
| eos-plugin-catalog | full | partial | full | 0 | Conformant; audit ownership moved to eos-audit (spec stale) |
| eos-tools | minor | all | full | 0 | Conformant; `model_tools` flat-file layout vs spec subdirs |
| eos-engine | full | all | partial | 3→fixed | 2 parity bugs **fixed**; ENG-3 golden-test gap remains |
| eos-workflow | minor | partial | full | 3 | Phase-6/7 dual-submission-path residuals (decisions) |
| eos-runtime | major | partial | full | 2 | Layout diverged via parallel-agent refactor (spec stale) |
| (workspace) | full | all | full | 0 | Conformant; dependency-topology/test-dep flags only |

Overall: **structurally faithful**. No FORBIDDEN-list violations were found
(verified per crate): no global orchestrator, no synthetic root workflow, no
`class_path` dynamic dispatch (`class_path` is migration data only), no tool
visibility enum, no lazy tool loading, no PostgreSQL support (`DbError::PostgresRejected`
is a fail-fast rejection), no p2p messaging.

## 3. Fixes applied this pass (code, verified against Python source, gated green)

1. **ENG-1 — terminal-call reminder turn-1 parity** (`eos-engine/src/notifications.rs`).
   The reminder ignored the message list and fired on turn 1 (user-only transcript);
   Python `must_submit_terminal_tool.py` gates on `terminal_result is None AND any
   assistant message`. Added the assistant-message guard so the reminder is never
   written into a turn-1 prompt-report. *Matches source.*

2. **ENG-2 — notification body strings** (`eos-engine/src/notifications.rs`). Both
   `TerminalCallReminder` and `ToolCallBudget` bodies were paraphrased and dropped
   the `{used}/{limit}` budget + `ceil(1.5*limit)` ceiling + `turns_remaining` that
   the Python rule bodies (and the engine spec §8.4) require. Ported the exact Python
   strings + ceiling math. Added regression test `terminal_reminder_needs_assistant_turn_and_reports_budget`.

3. **Removed `ToolRegistry::register_many`** (`eos-tools/src/registry.rs`). Zero callers
   workspace-wide; the eos-tools spec §3 maps the Rust registry API to exactly
   `register/get/list/remove/restrict` (no `register_many`), so removal makes the code
   match the spec.

## 4. Findings verified as NON-issues (no action)

- **eos-db-01 (claimed major)** — `list_tasks_for_attempt` "missing despite a live
  consumer". **Verified false**: zero references to `list_tasks_for_attempt` /
  `list_for_attempt` anywhere in the workspace; the eos-state `TaskStore` trait does
  not declare it; eos-workflow reads an attempt's tasks via the
  `generator_task_ids`/`reducer_task_ids` stored on the Attempt row + per-id `get_task`
  (matching Python `project_attempt_outcomes`). This is an **over-specified AC**, not a
  code defect → reconcile the AC text (see decision #10), no code change.
- **eos-db-05** — `enum_to_db` uses `.expect()` in non-test code. **Compliant**: the
  status/stage/reason enums are fieldless and always serialize to a JSON string, so this
  is the anchor §8-sanctioned "true invariant" `.expect()`, not a violation.

## 4b. Targeted hand-verifications (gaps a passing-test self-check cannot reach)

- **Notification shared guard (in-scope of the ENG-1/2 fix).** The shared top guard
  bails on `terminal_result.is_some_and(is_terminal)`, while Python bails on
  `terminal_result is None`. Traced every production assignment to `ctx.terminal_result`
  (`dispatch.rs:280`, gated by `if completion.result.is_terminal`; `dispatch.rs:295`, from
  `first_terminal_result` which `.filter(|r| r.is_terminal)`): it is **never `Some` with
  `is_terminal == false`** in production, so the guard is equivalent to Python's `is None`.
  No second residual divergence. The extra `!terminal_tools.is_empty()` condition (which
  Python lacks) is a benign intentional refinement. **Notification parity confirmed.**

- **eos-db outcome projection (the highest-risk port, §6.8).** Hand-verified
  `normalize_task_outcomes`/`normalize_attempt_outcomes` (`eos-db/src/rows.rs`) against
  `backend/src/workflow/_core/outcomes.py` line-by-line. All asymmetries are faithful:
  present-status `"done"`→failed vs missing-status (task path) `present_status("done")`→
  success; task-path role/task_id fallback to the owning row vs attempt-path
  `→generator`/`""`; `present_status` body matches exactly. The sole divergence is the
  **documented** eos-db-04 (empty/unparseable `task_id` record dropped, not emitted with
  `task_id=""`), unreachable via the real serializer. **Projection parity confirmed.**

## 5. Decisions for the user (25) — grouped by theme

These are not auto-fixable: they need a spec-blessing, an architecture/ownership call,
or a dependency-topology rebaseline. Nothing here blocks the build.

### 5a. Spec-vs-live-daemon correctness (highest cutover risk)
- **[#4] eos-sandbox-api `exec_stdin` wire string (MAJOR).** Rust emits
  `api.v1.exec_stdin` (per anchor §4 glossary + spec §6.4), but the live Python daemon
  (`transport.py`) uses `api.v1.write_stdin`; the string `exec_stdin` appears in **zero**
  Python files. The rename is applied consistently across the Rust tree (sandbox-api +
  sandbox-host). **Decide:** is `eosd` being renamed in lockstep to accept
  `api.v1.exec_stdin`? If not, this breaks the stdin tool at cutover. (Cannot be verified
  here — the `eosd` op-dispatch table is in the compiled binary.)

### 5b. Leaf-error contract / type-parity (spec-blessing)
- **[#1] CoreError::Store(String) (MAJOR).** Added beyond spec §6.4's "exactly two
  variants" to flatten `DbError` at the eos-db→eos-state seam. Bless in §6.4 **or** move
  the persistence-error contract into eos-state. (Load-bearing across 7 call sites.)
- **[#2] UtcDateTime emits `Z` not Python's `+00:00`.** RFC-3339-equivalent and
  parse-identical, but the §6.2/AC-03 byte-parity claim is unmet. Decide whether cutover
  compares timestamp *bytes* or *instants*; amend the AC or add a `+00:00` shim.
- **[#6] eos-agent-def: derived `Deserialize` bypasses construction invariants.** Narrow
  the §6 "unrepresentable" claim to the file-parse path, or route deserialize through
  `#[serde(try_from = "Raw…")]`.

### 5c. eos-workflow Phase-6/7 submission architecture (already flagged "loud" in tracker)
- **[#15] F1 / [#16] F2 / [#17] F3 / [#18] F4 / [#19] F5.** Two submission paths coexist
  (store-mediated `run_stage` single-writer loop vs the injected `PlanSubmissionPort`
  adapter that re-enters `advance_run_stage` with a fresh `AttemptStageAdvancer`/
  `CancellationToken`). Harmless today (only the single-writer path is exercised; the
  guards never fire), but **exactly one path must advance once Phase-7 wires real
  delegated execution** — the port path as written would break the single-writer rule.
  Also: GC-04 lane/acyclic validation + task materialization were relocated from
  eos-tools into the orchestrator; `serde_json` is a non-test dep despite spec §2.
  **Decide the canonical path** before cutover and reconcile the spec; do **not** delete
  either path now.

### 5d. eos-runtime layout vs spec (parallel-agent refactor; code correct, spec stale)
- **[#20] RT-01 / [#21] RT-02 / [#22] RT-03 / [#23] RT-04.** Provisioning was moved to
  eos-sandbox-host by a parallel agent; `agent_loop.rs` holds the `run_ephemeral_agent`
  wrapper locally (spec §5 places the contract in eos-engine); `RequestEntry` is a free
  fn, not a struct. Reconcile spec §3/§4/§5 to the real module set, or move the
  engine-run wrapper into eos-engine.

### 5e. Spec-AC accuracy / minor gaps (doc reconciliation)
- **[#10] eos-db-02** strike `list_tasks_for_request`/`list_for_request`/`get_by_sequence`
  from the ACs (unneeded, zero callers). **[#11] eos-db-03** `acquire_timeout` omitted
  from the pool builder vs spec §7 — add or amend. **[#3] eos-state-01** keep the
  spec-owned `latest_iteration` projection but make eos-workflow `lifecycle.rs` call it
  instead of the inline `max_by_key` (DRY; verify the tie-break semantics match before
  applying). **[#14] ENG-3** AC-engine-04 prompt-report golden is missing
  (`tests/fixtures/` empty) — commit a golden or amend the AC. **[#12] SBH-01**
  `BackgroundManager` is an off-seam-map trait (justified by DIP; record it on the §5 map).

### 5f. Dependency topology / deferred test tooling — resolved cleanup
- **[#5] eos-agent-def→eos-types, [#8] eos-skills→eos-types, [#13] eos-plugin-catalog→eos-config,
  [#24] eos-tools→eos-audit.** Resolved 2026-06-04: these stale internal edges
  were removed and the Rust-only `workspace-guard` member now asserts the current
  dependency topology and acyclicity.
- **[#25] `wiremock`/`loom` pins.** These remain separate deferred test-tooling
  pins; they are no longer coupled to a frozen dependency-DAG decision.
- **[#7] eos-llm-client `LlmRequestBuilder::message` (singular)** — used only in tests;
  harmless ergonomic surface. Optional YAGNI removal; left in place (it completes the
  builder API and has test consumers).

## 6. Phase-7 gap (known, expected)

`impl-cutover.md` is unimplemented: no subprocess JSON-RPC adapter, config/env switch,
parity comparator, or Python-retirement gates. Several decisions above (5a–5d) are the
natural inputs to that phase. This review does not attempt cutover.
