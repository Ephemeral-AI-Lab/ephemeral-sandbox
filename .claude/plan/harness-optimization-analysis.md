# Analysis: Task Planning & Execution Engine vs Anthropic Harness Pattern

> Created: 2026-03-30
> Status: Architecture Analysis & Optimization Proposal

---

## Part 1: Current System Flow

### Task Plan Creation Flow

```
Goal (user/benchmark)
    ↓
run_planning_workflow()  [planning/workflow/workflow.py]
    ↓  4-phase pipeline: analyze → explore → synthesize → plan_tasks
    ↓  Each phase: work_step (agent reasoning) → posthook_step (structured extraction)
    ↓
plan_tasks posthook → submit_plan.py
    ↓  parse_task_list() → validate_task_graph() → persist_plan()
    ↓  (Kahn's cycle detection, max 200 tasks, ID/schema validation)
    ↓
CoordinationPlan persisted to DB
    ↓
emit("run_planned")  ← event-driven handoff
    ↓
ExecutionEngine._on_run_planned()
    ↓
cascade_dispatch() → wave-based parallel execution
```

### Execution Engine Flow

```
cascade_dispatch()
    ↓  get_ready_tasks() — all deps completed/skipped, status=PENDING
    ↓  CAS: pending → queued (atomic)
    ↓
dispatch_task()
    ├── Worker path: acquire model semaphore → build context → run agent → terminal_hook
    └── TaskPlanner path: expand_task() → child run_planning_workflow → child plan
    ↓
_process_terminal_outcome()
    ↓  finalize_task_result (CAS + artifact)
    ↓  update domain context
    ↓  block failed dependents (transitive)
    ↓  cascade_dispatch() (next wave)
    ↓  finalize_run() (if all terminal)
```

### Key Abstractions

| Component | Role |
|-----------|------|
| `TeamTask` | Atomic/macro work unit with deps, conditions, CI metadata |
| `CoordinationPlan` | Task graph + workspace contract + stage config |
| `ExecutionEngine` | Semaphore pools, run locks, lifecycle, event bus |
| `RunContext` | Shared infrastructure threaded through all modules |
| `ExpansionGuard` | State machine for macro → atomic decomposition (max depth 4) |
| `PhaseRunner` | 2-step phase execution (work + posthook extraction) |
| `AsyncBridge` | Thread↔event-loop scheduling |
| `CoordinationCIAdapter` | File prediction, symbol pre-warming, artifact building |

---

## Part 2: Anthropic Harness Pattern (Plan → Generate → Evaluate Loop)

Anthropic's canonical harness is a **single-agent iterative loop**:

```
while not done:
    plan = agent.plan(goal, context, feedback)
    output = agent.generate(plan)
    result = evaluate(output, criteria)
    if result.pass:
        done = True
    else:
        feedback = result.feedback
        # loop back
```

### Characteristics

1. **Single persistent agent** — one agent owns the full lifecycle
2. **Serial execution** — plan, generate, evaluate happen sequentially
3. **Polling/iteration** — explicit `while not done` loop drives progress
4. **In-memory state** — plan and feedback live in context window
5. **Evaluation-gated** — external eval function decides convergence
6. **Simple mental model** — easy to reason about, easy to debug
7. **Stateless between runs** — no persistence; crash = restart from scratch

### Strengths

- **Simplicity**: One file, one loop, one agent — trivial to understand
- **Flexibility**: Agent can pivot strategy each iteration based on feedback
- **Self-correction**: Built-in retry with eval feedback
- **Low infrastructure**: No database, no event bus, no concurrency control
- **Battle-tested**: Used successfully in SWE-bench, GPQA, etc.

### Weaknesses

- **Serial bottleneck**: One task at a time, no parallelism
- **Context window pressure**: All state accumulates in one context
- **No crash recovery**: In-memory plan lost on failure
- **Poor observability**: No structured task tracking
- **Eval dependency**: Requires well-defined evaluation criteria
- **Scalability ceiling**: Complex tasks overwhelm single-agent capacity

---

## Part 3: Head-to-Head Comparison

| Dimension | Anthropic Harness | Ephemeral OS |
|-----------|-------------------|--------------|
| **Agent model** | 1 persistent agent, iterating | N ephemeral principals, parallel |
| **Execution** | Serial loop (plan→gen→eval) | Event-driven wave dispatch |
| **Concurrency** | None (single-threaded) | Per-model semaphores, wave parallelism |
| **Task decomposition** | One-shot or iterative re-plan | Hierarchical expansion (depth ≤ 4) |
| **State persistence** | In-memory (context window) | DB-backed with CAS transitions |
| **Crash recovery** | None — restart from scratch | Resume from last persisted state |
| **Self-correction** | Eval feedback loop | Dependency blocking + conditional dispatch |
| **Observability** | Print/log statements | EventBus + audit middleware + query API |
| **Configuration** | Hardcoded or minimal config | Teams, workspace contracts, stage configs, planner definitions |
| **Infrastructure cost** | ~0 (just an LLM client) | Database, event bus, async bridge, semaphore pools |
| **Complexity** | ~100 LOC | ~5,000+ LOC across 30+ files |
| **Time to first result** | Immediate (no planning overhead) | 4-phase planning + dispatch latency |

### Where Ephemeral OS Wins

1. **Parallelism** — 5+ workers executing simultaneously vs 1 serial agent
2. **Crash resilience** — DB persistence means no lost work
3. **Complex task handling** — hierarchical expansion handles problems too large for one context
4. **Observability** — structured task tracking, audit logs, query API
5. **Resource efficiency** — ephemeral agents don't hold context between tasks

### Where Anthropic Harness Wins

1. **Simplicity** — dramatically less code and infrastructure
2. **Self-correction** — natural retry loop with eval feedback
3. **Startup speed** — no 4-phase planning overhead for simple tasks
4. **Debugging** — single linear execution trace
5. **Adaptability** — agent re-plans from scratch each iteration based on actual results

---

## Part 4: Critical Gaps in Current Design

### Gap 1: No Eval/Retry Loop

The current system has **no built-in mechanism for a task to fail, get feedback, and retry with corrected approach**. A failed task blocks its dependents. Period.

In the Anthropic harness, the eval → feedback → retry loop is the core value proposition. Our system loses this entirely.

**Impact**: Workers that produce incorrect output have no path to self-correction. The only "retry" is human intervention or a completely new run.

### Gap 2: Planning Overhead for Simple Tasks

The 4-phase planning workflow (analyze → explore → synthesize → plan_tasks) is **mandatory overhead** regardless of task complexity. A simple "fix this typo" still runs 4 LLM calls before any work begins.

The Anthropic harness starts working immediately.

### Gap 3: Remaining Polling Loops

Despite the event-driven core, two critical consumers still poll:
- `ci_e2e_harness.py` — `time.sleep()` polling for run completion
- `sweevo_adapter/orchestration.py` — `asyncio.sleep()` polling for benchmark progress

(Already addressed in `coordination-loop-migration-plan.md`)

### Gap 4: Over-Abstraction

The system has significant abstraction overhead:
- `RunContext` threads ~15 callables through every module
- `AsyncBridge` manages thread↔loop crossing that may not always be needed
- `PhaseRunner` has a 2-step (work + posthook) pattern even when a single step would suffice
- `CoordinationCIAdapter` does file prediction from task descriptions — often inaccurate and rarely used

### Gap 5: Configuration Explosion

Teams, workspace contracts, stage configs, planner definitions, specialist definitions, model queue limits — the configuration surface area is large relative to the number of actual deployment configurations used.

---

## Part 5: Optimization Proposals

### Proposal 1: Add Lightweight Eval-Retry to Workers (HIGH PRIORITY)

**Goal**: Bring the Anthropic harness's self-correction capability into the event-driven model.

```
Current:    worker fails → task FAILED → dependents BLOCKED → done
Proposed:   worker fails → eval(result) → retry with feedback (up to N times) → then FAILED
```

**Implementation sketch**:
```python
# In worker_hooks.py or a new retry_policy.py
@dataclass
class RetryPolicy:
    max_retries: int = 2
    eval_fn: str = "default"  # or "test_pass", "lint_clean", etc.

async def _run_worker_with_retry(task, plan, policy, dispatch_ctx):
    for attempt in range(1 + policy.max_retries):
        result = await run_worker(task, plan, dispatch_ctx)
        if result.status == "completed":
            return result
        # Eval feedback becomes part of next attempt's context
        feedback = eval_result(result, policy.eval_fn)
        task.context.append(f"Attempt {attempt+1} feedback: {feedback}")
    return result  # final failure
```

**Key design choices**:
- Retry is per-task, not per-run (unlike Anthropic's full-loop retry)
- Feedback is structured and appended to worker context
- Configurable per-task via `retry_policy` field on `TeamTask`
- Default: 0 retries (current behavior preserved)
- Eval functions are pluggable (test execution, lint, custom)

**Estimated complexity**: Medium. Touches `worker_hooks.py`, `dispatch.py`, `models.py`.

---

### Proposal 2: Fast-Path for Simple Tasks (HIGH PRIORITY)

**Goal**: Skip 4-phase planning when unnecessary, matching Anthropic harness startup speed.

```
Current:    goal → 4 phases → task graph → dispatch → execute
Proposed:   goal → complexity_check → simple? → single-task direct dispatch
                                    → complex? → full 4-phase pipeline
```

**Complexity heuristic** (configurable):
- Single file mentioned → simple
- Explicit "fix", "typo", "rename" keywords → simple
- User provides pre-built task list → skip planning entirely
- Benchmark with known structure → use cached plan template

**Implementation**:
```python
async def smart_plan_or_execute(goal: str, context: dict) -> CoordinationPlan:
    if is_simple_task(goal, context):
        # Create single-task plan directly, skip 4 phases
        plan = CoordinationPlan(
            tasks={"t1": TeamTask(description=goal, agent_name="default-worker")},
            goal=goal,
        )
        await persist_plan(plan)
        return plan
    else:
        return await run_planning_workflow(goal, context)
```

**Estimated complexity**: Low. New entry point function, no changes to existing code.

---

### Proposal 3: Simplify RunContext (MEDIUM PRIORITY)

**Goal**: Reduce the number of callables threaded through RunContext.

**Current**: RunContext carries ~15 function references (`dispatch_task_fn`, `cascade_dispatch_fn`, `finalize_run_fn`, `make_hook_fn`, `handle_failure_fn`, `model_semaphore_fn`, `coordination_semaphore_fn`, etc.)

**Proposed**: Consolidate into 3 service interfaces:

```python
@dataclass
class RunContext:
    store: CoordinationStoreProtocol
    dispatcher: DispatchService      # dispatch, cascade, semaphores
    lifecycle: LifecycleService      # finalize, hooks, failure handling
    event_bus: EventBus
    ci_adapter: CoordinationCIAdapter | None
```

Where `DispatchService` and `LifecycleService` are thin wrappers that hold the engine reference and delegate. This preserves testability (can mock interfaces) while reducing the parameter threading.

**Estimated complexity**: Medium. Refactor across dispatch.py, lifecycle.py, worker_hooks.py, expansion.py.

---

### Proposal 4: Deprecate CI Adapter File Prediction (LOW PRIORITY)

**Goal**: Remove the regex-based file prediction from task descriptions.

The `CoordinationCIAdapter._predict_touched_files()` mines file paths from task description text using regex. This is inherently unreliable and adds complexity. The actual files touched are only known after worker execution.

**Proposed**: Keep artifact building (post-execution), remove prediction (pre-execution). Workers already have access to the full workspace — let them discover files organically.

**Estimated complexity**: Low. Remove `_predict_touched_files()` and `_prewarm_for_predictions()`.

---

### Proposal 5: Converge Polling Consumers (ALREADY PLANNED)

The `coordination-loop-migration-plan.md` already covers this comprehensively. Key actions:
1. Implement `RunNotifier` with async awaitables
2. Add `/wait` and `/events` SSE endpoints
3. Refactor `ci_e2e_harness.py` and `sweevo_adapter`

**Status**: Detailed plan exists, ready for execution.

---

### Proposal 6: Adaptive Planning Depth (MEDIUM PRIORITY)

**Goal**: Allow the planning workflow to short-circuit phases when earlier phases provide sufficient clarity.

```
Current:    always run all 4 phases (analyze → explore → synthesize → plan_tasks)
Proposed:   analyze → sufficient? → skip to plan_tasks
                    → need exploration? → explore → synthesize → plan_tasks
```

**Implementation**: Add `skip_if` conditions to phase config:
```json
{
  "name": "explore",
  "skip_if": {"phase": "analyze", "field": "complexity", "operator": "eq", "value": "low"}
}
```

This is already partially supported by the declarative phase config system — just needs the `skip_if` evaluator.

**Estimated complexity**: Low-Medium. Add evaluator to `phase_runner.py`.

---

## Part 6: Recommended Priority Order

| # | Proposal | Impact | Effort | Priority |
|---|----------|--------|--------|----------|
| 1 | Eval-Retry for Workers | Closes biggest gap vs Anthropic harness | Medium | **P0** |
| 2 | Fast-Path for Simple Tasks | Eliminates unnecessary planning overhead | Low | **P0** |
| 5 | Converge Polling Consumers | Plan already exists, ready to execute | Medium | **P1** |
| 6 | Adaptive Planning Depth | Reduces latency for medium-complexity tasks | Low-Med | **P1** |
| 3 | Simplify RunContext | Code quality, maintainability | Medium | **P2** |
| 4 | Deprecate CI File Prediction | Remove dead complexity | Low | **P2** |

---

## Part 7: Target Architecture (Post-Optimization)

```
Goal Input
    ↓
Complexity Router ←──── [NEW: Proposal 2]
    ├── Simple: single-task direct dispatch
    └── Complex: planning workflow
                    ↓
            Adaptive 2-4 Phase Pipeline ←──── [NEW: Proposal 6]
            (skip phases when sufficient)
                    ↓
            Task Graph (persisted)
                    ↓
            Event-Driven Dispatch
                    ↓
            ┌─── Worker (with eval-retry) ←──── [NEW: Proposal 1]
            │       ↓ fail?
            │       eval → feedback → retry (up to N)
            │       ↓ final fail
            │       block dependents
            │
            └─── TaskPlanner (expansion)
                    ↓
                    Child plan → recursive dispatch
                    ↓
            Reactive Notification ←──── [NEW: Proposal 5]
            (SSE + awaitables, no polling)
                    ↓
            Run Finalized
```

### What This Achieves

1. **Best of both worlds**: Anthropic harness's simplicity for simple tasks + Ephemeral OS's power for complex ones
2. **Self-correction**: Workers can retry with eval feedback, closing the biggest gap
3. **Reduced latency**: Skip unnecessary planning phases, fast-path simple tasks
4. **Clean infrastructure**: No polling, simplified RunContext, no speculative file prediction
5. **Preserved strengths**: Parallelism, crash resilience, hierarchical expansion, observability all remain intact

### What We DON'T Change

- Core event-driven dispatch model (proven, working)
- DB-backed persistence and CAS transitions (crash resilience)
- Per-model semaphore concurrency control (resource management)
- Dependency graph model with transitive blocking (correctness)
- Team/workspace configuration system (deployment flexibility)
