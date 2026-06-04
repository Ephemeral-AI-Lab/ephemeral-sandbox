# Workflow lifecycle — Rust parity remediation plan (PLAN ONLY)

Status: **plan only, do not implement.** Scope: the `agent-core / workflow_lifecycle`
findings in `docs/reviews/rust_parity/REPORT.html` (areas/workflow_lifecycle +
verify): the HIGH continuation-compensation gap (**D5**, the area's only code bug)
and the MEDIUM `delegate_workflow` error-flag divergence (**D3**). The low/intentional
items (D1, D2, D4, planner-cancel outcome, `agent_id` scoping) are recorded at the
end as one-liners.

Verified against the Python reference in `backend/src/workflow/{lifecycle,starter}.py`
+ `backend/src/workflow/iteration/attempt_coordinator.py`, and the current Rust under
`agent-core/crates/eos-workflow/src/{lifecycle,starter,iteration/mod}.rs` +
`agent-core/crates/eos-tools/src/model_tools/workflow.rs`. Every anchor below was
re-opened on both sides.

---

## 0. The issue in one paragraph

The workflow→iteration→attempt lifecycle *logic* is a faithful Rust port: creation
rules, the `default_attempt_budget = 2`, the strict-`<` budget check, the
PLAN→RUN→CLOSED + reducer-exit-gate cascade, the retry loop with retry-start-failure
refold, and the initial-path compensation saga all match Python and are unit-tested.
The defect is a **single missing failure path on the deferred-goal continuation**:
when an iteration closes SUCCEEDED with a deferred goal, Rust creates the next
iteration and calls `create_and_start_first_attempt()` with **no rollback**
(`lifecycle.rs:164-172`). If that first attempt fails to *start*, the new iteration
stays OPEN, its coordinator stays REGISTERED, and the workflow stays OPEN forever —
`check_workflow_status` reports "running" indefinitely and the coordinator leaks in
`OpenIterationCoordinatorRegistry`, with nothing surfacing the error to the parent
agent. Python compensates this exact path (`lifecycle.py:207-231`: cancel the new
iteration, deregister it, close the workflow FAILED) and additionally deregisters the
*old* iteration in a `finally` (`lifecycle.py:146-147`). The fix is to restore both,
in one file.

---

## 1. Root cause: the init-vs-continuation asymmetry

Rust already compensates a first-attempt startup failure on the **initial** launch
path — `WorkflowStarter::compensate_failed_start` (`starter.rs:116-161`) closes the
attempt STARTUP_FAILED, sets the iteration CANCELLED, sets the workflow CANCELLED,
and deregisters the coordinator (proven by `compensation_rolls_back`,
`starter.rs:263-302`). It just never wrote the equivalent on the **continuation**
path. Python compensates both. That asymmetry is the decisive proof this is a gap,
not a deliberate simplification.

Two distinct leaks, both confirmed on re-derivation:

- **Primary (FP1 — attempt start fails).** `coordinator.create_and_start_first_attempt()`
  → `start_attempt` → `orchestrator.start()`. A startup failure (e.g. the planner
  profile is missing → `AgentLaunchFactory::for_planner` returns
  `WorkflowError::AgentDefinition`, `orchestrator.rs:79-81`) makes `start_attempt`
  close only the **attempt** (FAILED/StartupFailed, `mod.rs:139-142, 277-295`) and
  return `Err`. Rust's `.map(|_| ())` propagates that `Err` with no cleanup → new
  iteration OPEN, new coordinator REGISTERED, workflow OPEN.
- **Secondary (FP2 — iteration create fails).** `create_iteration_with_coordinator(...)
  .await?` (`lifecycle.rs:166-168`) early-returns on `?` *before* the
  `deregister(&iteration.id)` at `lifecycle.rs:183`, so the **old** coordinator also
  leaks. Lower probability (the predecessor is SUCCEEDED+deferred, so create rarely
  fails) but real. Python's `finally` (`lifecycle.py:146-147`) makes the old-iteration
  deregister unconditional.

**Semantic nuance that constrains the fix:** the initial-path saga closes the workflow
**CANCELLED**; the continuation path closes it **FAILED** (`lifecycle.py:228-231`,
`close_workflow(succeeded=False)`). So the fix must *not* reuse
`compensate_failed_start` verbatim — it would set the wrong terminal status and would
redundantly re-close the attempt that `start_attempt` already closed.

---

## 2. The fix — `handle_iteration_closed` (one file)

`agent-core/crates/eos-workflow/src/lifecycle.rs`. Flatten the nested branch, move the
old-coordinator deregister to the top (unconditional), and add the continuation
compensation.

**Before** (`lifecycle.rs:157-185`):

```rust
pub async fn handle_iteration_closed(&self, closed: IterationClosed) -> Result<()> {
    let iteration = self
        .deps.iteration_store.get(&closed.iteration_id).await?
        .ok_or_else(|| WorkflowError::not_found("iteration", closed.iteration_id.as_str()))?;
    let result = if closed.succeeded {
        if closed.deferred_goal.is_some() {
            let (_next, coordinator) = self
                .create_iteration_with_coordinator(&iteration.workflow_id).await?;  // FP2: `?` skips deregister
            coordinator.create_and_start_first_attempt().await.map(|_| ())           // FP1: no rollback
        } else {
            self.close_workflow(&iteration.workflow_id, true).await.map(|_| ())
        }
    } else {
        self.close_workflow(&iteration.workflow_id, false).await.map(|_| ())
    };
    self.iteration_coordinators.deregister(&iteration.id);
    result
}
```

**After**:

```rust
pub async fn handle_iteration_closed(&self, closed: IterationClosed) -> Result<()> {
    let iteration = self
        .deps.iteration_store.get(&closed.iteration_id).await?
        .ok_or_else(|| WorkflowError::not_found("iteration", closed.iteration_id.as_str()))?;
    // The closed iteration's coordinator has finished its job; release it up front
    // so no early return below can leak it (mirrors Python's `finally`).
    self.iteration_coordinators.deregister(&iteration.id);

    if closed.succeeded && closed.deferred_goal.is_some() {
        self.start_iteration_with_deferred_goal(&iteration.workflow_id).await
    } else {
        // succeeded+no-deferral -> SUCCEEDED; not-succeeded -> FAILED.
        self.close_workflow(&iteration.workflow_id, closed.succeeded).await.map(|_| ())
    }
}

/// Start the deferred-goal continuation iteration, compensating on a start failure.
async fn start_iteration_with_deferred_goal(&self, workflow_id: &WorkflowId) -> Result<()> {
    let (next, coordinator) = self.create_iteration_with_coordinator(workflow_id).await?;
    if coordinator.create_and_start_first_attempt().await.is_err() {
        // Continuation could not start: cancel the new iteration, release its
        // coordinator, and fail the workflow (parity with Python
        // `_start_deferred_iteration`). The error is swallowed after compensation,
        // exactly as Python's `except` does — `handle_iteration_closed` returns Ok.
        self.iteration_coordinators.deregister(&next.id);
        self.deps
            .iteration_store
            .set_status(
                &next.id,
                IterationStatus::Cancelled,
                Some(eos_state::UtcDateTime::now()),
                None,
            )
            .await?;
        self.close_workflow(workflow_id, false).await?;
    }
    Ok(())
}
```

Notes:

- **No new imports.** `IterationStatus` is already imported (`lifecycle.rs:6`);
  `eos_state::UtcDateTime::now()` is already used at `lifecycle.rs:212`.
- **Swallow/propagate split matches Python exactly.** FP1 (attempt start) → compensate
  then `Ok(())` (Python's `except` does not re-raise). FP2 (iteration create) → the `?`
  propagates `Err` after the old coordinator is already deregistered (Python's `finally`
  runs, then the exception propagates; the workflow is left OPEN on both sides — an
  accepted, low-probability edge, see §7).
- `start_iteration_with_deferred_goal` is a private helper purely for readability; it can be inlined.
  No struct, field, or public-API change.

---

## 3. Why the retry + deferral dynamics are preserved

Both dynamics live in `iteration/mod.rs` (the coordinator), which this fix does **not**
touch. `handle_iteration_closed` is only the router that runs *after* an iteration has
already closed.

- **Attempt retry** — `retry_or_close_failed` (`mod.rs:219-236`), budget check, and the
  retry-start-failure refold are unchanged. The continuation iteration gets a fresh
  coordinator (`lifecycle.rs:150`) and therefore the identical full-budget retry loop.
  Pinned by the existing `retry_and_continue` test (`mod.rs:372`).
- **Iteration deferral** — creation (`create_iteration_with_coordinator`: seq+1 /
  `DeferredGoalContinuation` / goal=deferred / predecessor-SUCCEEDED guard) is unchanged.
  The branch-merge reproduces the routing for every emittable `IterationClosed`:

  | `IterationClosed` (emitter) | Original | After: `else { close_workflow(succeeded) }` | Match |
  |---|---|---|---|
  | `true, Some` (`close_iteration_passed:211`) | continuation | `if succeeded && deferred` → continuation | ✓ |
  | `true, None` (`close_iteration_passed:211`) | `close_workflow(true)` | else → `close_workflow(true)` | ✓ |
  | `false, None` (`close_iteration_failed:254`) | `close_workflow(false)` | else → `close_workflow(false)` | ✓ |

  Pinned by `deferred_goal_starts_next_iteration` (`mod.rs:405`).
- **Startup-failure ≠ run-failure.** A first-attempt *startup* failure never reaches
  `handle_attempt_closed`, so it is fatal-by-design on both the initial and continuation
  paths in both languages — compensating to FAILED (rather than retrying) is correct
  parity, not a retry-budget regression.

---

## 4. Edit surface — file & folder structure

```
agent-core/crates/eos-workflow/
└── src/
    ├── lifecycle.rs        ★ EDIT  handle_iteration_closed (rewrite) + start_iteration_with_deferred_goal (new private fn)
    │                       ★ EDIT  mod tests { + continuation_start_failure_compensates() }
    ├── starter.rs              (reference only — compensate_failed_start / compensation_rolls_back)
    ├── iteration/mod.rs        (untouched — owns retry + deferral creation; provides the test failure lever)
    └── testsupport.rs          (untouched — MemoryStores + agent_registry_without_planner already suffice)
```

**One production file changed** (`lifecycle.rs`), one test added in its existing
`#[cfg(test)] mod tests`. No new file, no new module, no new struct/field, no new
testsupport primitive. (§6 adds a second, independent one-line change in a different
crate.)

---

## 5. Test plan

Add `continuation_start_failure_compensates()` to `lifecycle.rs` `mod tests` — the
deferred-path analogue of `starter.rs::compensation_rolls_back`. It drives
`handle_iteration_closed` directly (no async run loop / `wait_for_workflow_status`
needed) by pre-seeding a SUCCEEDED+deferred predecessor and forcing the continuation's
planner launch to fail via `agent_registry_without_planner()`.

Sketch:

1. `MemoryStores::default()`; `deps` with `agent_registry = agent_registry_without_planner()`
   (so the continuation's `create_and_start_first_attempt` → `for_planner` fails with
   `WorkflowError::AgentDefinition`). Keep the `iteration_coordinators` handle.
2. `create_workflow(...)` then `create_iteration_with_coordinator(&wf.id)` → iter1
   (registers iter1's coordinator).
3. Mark iter1 SUCCEEDED + deferred: `IterationStore::close_succeeded(iter1, "[]", now)`
   then `set_deferred_goal_for_next_iteration(iter1, Some("continue"))`.
4. `lifecycle.handle_iteration_closed(IterationClosed { iteration_id: iter1, succeeded: true,
   deferred_goal: Some("continue".into()) }).await.unwrap();` — returns `Ok` (swallowed).
5. Assert:
   - `workflow(wf.id).status == WorkflowStatus::Failed`
   - the new iteration (seq 2, `DeferredGoalContinuation`) `status == IterationStatus::Cancelled`
   - `coordinators.get(&iter2.id).is_none()` (new coordinator deregistered)
   - `coordinators.get(&iter1.id).is_none()` (old coordinator deregistered — the secondary leg)
   - parent task untouched (mirror `compensation_rolls_back`'s final assertion)

Guard tests that must still pass unchanged: `retry_and_continue` (`mod.rs:372`),
`deferred_goal_starts_next_iteration` (`mod.rs:405`), `close_does_not_touch_parent`
(`lifecycle.rs:252`), `compensation_rolls_back` (`starter.rs:263`).

Commands: `cargo test -p eos-workflow` (and `cargo clippy -p eos-workflow` for the
unused-binding / lint check on the rewritten fn).

---

## 6. Secondary actionable — D3 (`delegate_workflow` "already outstanding" error flag)

Independent of D5; a different crate. Python returns the already-outstanding
short-circuit payload with `is_error=True` (`delegate_workflow.py:67-81`); Rust returns
`ToolResult::ok(...)` (`model_tools/workflow.rs:67-77`), i.e. `is_error=false`. The flag
is **not** inert in Rust — it is consumed by `eos-engine/.../background/supervisor.rs:169`
(Failed vs Completed), `tool_call/dispatch.rs:44,56`, and `audit/stream.rs:63-70` — so an
agent or telemetry keyed on it reacts differently.

Fix: return `ToolResult::error(payload)` for that one branch (confirm the exact
`ToolResult::error` constructor/signature against the other error returns in the same
file). Cosmetic sub-item in the same branch: Rust hardcodes `"status":"running"` in the
payload (`:73`) where Python emits `existing.status.value` — align if cheap, otherwise
leave. Add/adjust the `delegate_workflow` unit test to assert `is_error` on the
outstanding-workflow branch.

This is a 1-line behavioral change + 1 test; keep it in a separate commit from D5.

---

## 7. Low / intentional items (recorded, not scheduled)

- **D1 — `is_nested_workflow` 1-hop vs ancestry walk** (`ports.rs:228-236` vs
  `workflow_depth.py:10-49`): boolean-equivalent for well-formed trees; loses cycle /
  missing-row defenses. Acceptable; optionally document the 1-hop simplification in a
  code comment.
- **D2 — `close_workflow` outcomes pass-through vs re-normalize** (`lifecycle.rs:227-239`
  vs `lifecycle.py:157-159`): idempotent under current writers (iteration `outcomes` are
  already canonical). No fix required.
- **D4 — dropped `delegate_workflow` metadata keys** (`attempt_id`,
  `initial_iteration_id`, `initial_attempt_id`; `workflow.rs:90-103`): structural (the
  `StartedWorkflow` return type doesn't carry them, `ports.rs:151-154`). Harmless unless a
  consumer reads them — verify, then port if needed.
- **Planner/root cancel-outcome** (`ports.rs:375` returns empty vec for Root/Planner vs
  Python's raw `{"role":"planner"}` task-row outcome): low; planner is not an execution
  role and the record is not surfaced in iteration/workflow outcomes.
- **`agent_id` scoping dropped** (`ports.rs:208-212` ignores caller `agent_id`; the handle
  registry is global): low authorization-scope simplification (one running task ↔ one
  agent in practice).
- **FP2 leaves the workflow OPEN** (after this fix): matches Python (its `finally`
  deregisters the old coordinator, then the create-iteration exception propagates with no
  compensation). Not closing the workflow on FP2 is intentional parity; revisit only if a
  create-iteration failure becomes a realistic, surfaced concern.

---

## 8. Acceptance criteria

- `handle_iteration_closed` on a deferred continuation whose first attempt fails to start
  leaves: workflow **FAILED**, new iteration **CANCELLED**, **both** coordinators
  deregistered, parent task untouched — asserted by
  `continuation_start_failure_compensates()`.
- FP2 (create-iteration failure) deregisters the old coordinator and propagates the error
  (no new leak), matching Python.
- All four guard tests (§5) pass unchanged; `retry_and_continue` and
  `deferred_goal_starts_next_iteration` confirm the retry + deferral dynamics are intact.
- D3: the `delegate_workflow` outstanding-workflow branch returns `is_error=true`, asserted
  by its unit test.
- `cargo test -p eos-workflow` green; `cargo clippy -p eos-workflow` clean.

---

## 9. Alternatives considered

- **Reuse `compensate_failed_start`** (the initial-path saga) on the continuation path:
  **rejected** — it sets the workflow CANCELLED (continuation needs FAILED) and redundantly
  re-closes the attempt `start_attempt` already closed. The bespoke 3-call compensation is
  smaller and correct.
- **Emulate `try/finally` with a scope guard / RAII deregister:** **rejected** as
  unnecessary — the old iteration is already closed, so deregistering it up front (before
  any `?`) is order-independent and simpler than a guard type.
- **Propagate the FP1 error instead of swallowing:** **rejected** for parity — Python's
  `except` swallows after compensating, and propagating would surface a spurious error on
  the already-closed predecessor's completion path. (Revisit only if the
  `IterationClosedCallback` caller is later changed to consume the error meaningfully.)
- **Route the continuation startup failure through the normal retry path:** **rejected** —
  a startup failure never enters `handle_attempt_closed`/`retry_or_close_failed` on either
  the initial or continuation path; treating it as fatal is the established, parity-faithful
  behavior.
