---
name: task-decompose
description: Universal task decomposition skill for coordinators. Use at any depth level — root goal or mid-tree macro — to emit a mixed graph of expandable sub-tasks and atomic leaf tasks.
---

# Task Decomposition

You are a coordinator. Your job is to decompose your goal into a task graph that maximizes parallelism and minimizes depth.

This skill is **depth-agnostic** — the same process applies whether you are decomposing a root goal ("build an e-commerce platform") or a macro sub-goal ("build the catalog API spine"). Recursion terminates naturally when every task is atomic.

## Mandatory anti-patterns

- **Do not use broad domain buckets as root tasks when the codebase map exposes narrower owned slices.** Bad: `DataFrame deprecations`, `NumPy 2.0 compatibility`, `CI + docs`. Good: a concrete compat shim slice, a concrete behavior/deprecation slice, a concrete test-only slice, a concrete docs slice.
- **One worker type available does not change task boundaries.** If only one specialist agent name is available, still keep independent tasks separate so queueing, retry, and dependency tracking preserve parallel lanes.
- **Do not create umbrella foundations by default.** A task that would gate 3 or more siblings must be tiny and truly shared; otherwise split it by consumer subset or fold it into the dependent slices.

---

## The Decomposition Process

### Step 1 — Identify the independent deliverables

Break the goal into discrete units of work. A good deliverable:
- Has a clear success condition you can state in one sentence
- Belongs to one owner (one specialist agent)
- Does not require mid-task coordination with another concurrent agent
- When a synthesized codebase map is available, maps to one owned file/symbol cluster and one primary validation target rather than a changelog heading or technology label alone

### Step 2 — For each unit, ask: is it atomic?

**One agent can own and complete this end-to-end without coordinating with another agent mid-task?**

- **Yes → emit as atomic task** (`expandable: false` or omitted). Assign to the right specialist.
- **No → emit as expandable task** (`expandable: true`). Write an `expansion_hint` that describes the wave structure of the subtasks. The engine will call a sub-coordinator to decompose it further.

Recursion terminates when every task in the graph is atomic. There is no fixed depth limit.

**MULTI-DOMAIN RULE (mandatory):** If your goal involves 2 or more distinct implementation domains — e.g. backend API + frontend UI, multiple backend sub-systems, auth + business logic + tests — you MUST emit at least one `expandable: true` macro per domain. Do NOT collapse an entire backend or entire frontend into one atomic task. A task like "Implement the backend" or "Build the auth system" spanning more than one file cluster is always expandable, never atomic.

### Step 3 — Set dependencies sparingly

Add `depends_on` only when task B genuinely needs task A's output to begin. Do not add deps for:
- "might edit the same file" → OCC resolves concurrent writes
- "might be related" → parallel is almost always faster
- "good practice" → only real data dependencies count

But when task B consumes a concrete artifact, interface, or verified behavior produced by task A, add the dependency explicitly. Missing a real dependency is worse than one extra edge.

### Step 4 — Fan out maximally

Tasks with no dependency relationship should run in parallel. Default assumption: **parallel**. Override only with evidence.

### Step 5 — Minimize global bottlenecks

A root-level "foundation" task is valid only when it is both small and truly shared.

- If 3 or more siblings would depend on the same candidate foundation, prove they all need the exact same prerequisite to start.
- If the prerequisite only matters to one cluster, fold it into that cluster's task or split it by consumer subset.
- Do not create umbrella blockers just because the changelog mentions a shared theme such as "compatibility", "cleanup", or "follow-up work".

---

## Task Types

### Expandable (expandable: true)

Use for:
- Complex subsystems that will produce 3–8+ atomic tasks when decomposed (api-spine, ui-spine, db-layer, auth-service, data-pipeline-stage)
- Any work where you can name the wave structure but not the individual files
- Work where you want a sub-coordinator to apply domain-specific decomposition patterns

Required fields:
- `expandable: true`
- `expansion_hint`: wave-structure description (see below)
- `agent_name`: specialist who will own the resulting atomic subtasks

### Atomic (expandable: false or omitted)

Use for:
- Tasks one agent can complete end-to-end: implement a component, write a model, add an endpoint, install dependencies, write a test file
- Clearly scoped deliverables with a single output artifact
- Utility tasks: scaffold directory, create .env.example, write README

Required fields:
- `agent_name`: appropriate specialist

---

## Writing a Good expansion_hint

The hint is injected verbatim as context into the sub-coordinator's decomposition call. Write it as a **wave structure**, not a file list.

**Bad (file-per-layer, creates serial chain):**
```
"write models.py, then repository.py, then service.py, then routes.py"
```

**Good (wave-based, enables parallelism):**
```
"(1) ORM model + migration as one collapsed task; (2) repo + service collapsed (service is thin orchestration); (3) parallel: one task per endpoint group; (4) router wiring depending on all endpoints"
```

**Good (component-based):**
```
"parallel leaf tasks: one per independent component (ProductCard, useCatalog hook, catalogApi client); then container CatalogPage depending on all three"
```

---

## Specialist Mapping

These role labels are conceptual guidance only. In `plan_tasks()`, `agent_name` must always be one of the exact names returned by `list_available_agents()`. If your team uses custom names like `wwx` or `yifa`, map the role intent onto those names instead of emitting the generic labels below.

| Specialist | Use for |
|---|---|
| `backend-developer` | Python, FastAPI, SQLAlchemy, Alembic, REST/GraphQL endpoints, background tasks |
| `frontend-developer` | React 18+, TypeScript, Tailwind CSS, React Query, React Router, Vite |
| `test-engineer` | pytest, vitest, playwright, test fixtures, coverage reporting |
| `devops-engineer` | Shell, Docker, docker-compose, npm/pip/cargo install, .env, CI/CD pipelines |
| `fullstack-developer` | Tasks spanning both FE and BE: shared type contracts, auth wiring, API client generation |

---

## Parallelism Rules

**1. Collapse trivially sequential steps.**
If B always follows A and nothing else can run between them, merge A+B into one atomic task:
- ORM model + its migration → one task
- Repository + thin service → one task
- Router file + app wiring → one task

**2. Fan out at the entity level.**
Separate entities (Product, Cart, Order) are independent. One task per entity, run in parallel.

**3. Fan out endpoints off a shared service.**
Once a service exists, GET list, GET detail, POST create, DELETE can be separate parallel tasks.

**4. Target max chain depth of 3 within a single domain.**
`foundation → service layer → {endpoint-A ∥ endpoint-B ∥ wire}` is the standard shape.

---

## Sequential Ordering Rules

Use sequencing only to express real unlock order. Prefer broad waves, not micro-ordering.

**1. Emit ordered waves, not timestamps.**
If work must be sequential, model it as:
`foundation → implementation frontier → verification frontier → integration/polish`
Within each wave, tasks should still fan out in parallel.

**2. Producers before consumers.**
If a task creates the thing another task uses, the consumer depends on the producer:
- API/client generation depends on the API contract
- tests depend on the feature slice they exercise
- docs depend on the interface or workflow they describe

**3. Shared contracts unlock leaf work.**
Place schemas, shared types, DB foundations, auth/session primitives, and base layouts before the leaf tasks that import or extend them.

**4. Verification follows implementation and should be expandable.**
Do not emit standalone "write all tests" tasks in the same frontier as the features under test unless the tests are pure scaffolding. Test tasks should usually depend on the concrete backend/frontend slice they validate.
If a graph mixes implementation work and verification work, every verification task should depend on at least one non-foundation implementation or integration task. Examples: backend tests depend on backend API work, frontend tests depend on frontend UI work, and smoke/E2E verification depends on the bridge or final wiring tasks it exercises.

**Mark verification tasks as `expandable: true` with domain subsets.** A single "write all backend tests" atomic task risks timeout when the test surface is large. Instead, emit an expandable verification macro with an expansion_hint that splits tests by domain:
```
"parallel subsets: (1) auth endpoint tests (register, login, token refresh);
(2) product/catalog API tests (CRUD, search, filtering);
(3) cart + checkout flow tests (add/remove items, mock payment);
(4) order history tests"
```
Each subset becomes an atomic subtask that one agent can complete within a single worker tool budget. Treat the Agno hard ceiling as 100 tool calls and plan for each subset to finish well under that. The same pattern applies to frontend tests (split by page/feature) and E2E tests (split by user flow).

**5. Integration and wiring come last.**
Router wiring, page composition, cross-domain integration, release assembly, and final smoke/E2E checks should depend on the leaf tasks they aggregate.

**6. Documentation is late unless it is enabling work.**
Bootstrap docs (`.env.example`, setup notes, interface stub docs) can run early. Descriptive docs (README sections, API usage guides, runbooks) should depend on the implementation being stable enough to describe.

**7. When in doubt, collapse instead of chaining.**
If two steps are always performed by the same specialist with no useful parallel work between them, merge them into one atomic task rather than creating a fragile serial chain.

---

## Depth and Count Guidelines

The **2–8 subtask rule** applies at every level — root and child alike. The `plan_tasks()` tool rejects plans with more than 8 all-atomic tasks; plans with >8 tasks must include expandable children.

| Scope | Expandable count | Total task count |
|---|---|---|
| Root goal (fullstack app) | 2–5 macros | 3–8 tasks |
| Mid-level macro (api-spine) | 0–2 sub-macros | 2–5 tasks |
| Leaf-level macro (single endpoint group) | 0 | 2–5 atomic tasks |

**Too few tasks at root (≤ 2):** you have lane-level macros, not domain macros. Split.
**Too many tasks at root (> 8):** you are flattening expansion work into the root. Group related domains into expandable macros to push breadth down.

---

## Fault Isolation

Every expandable task is a failure domain. If it fails, downstream tasks that depend on it are blocked.

Rules:
1. Foundation tasks must be minimal — they block everything that depends on them.
2. Do not put dependency installation, broad scaffolding, or long-running verification into one shared foundation task if downstream macros can start from lighter repo/config setup. Heavy setup belongs in lane-local or late-stage tasks, not the global bottleneck.
3. One expandable task per independent domain — catalog and cart are separate domains.
4. Never bundle two failure domains into one task.
5. Integration tasks should depend only on the macros they actually integrate, not on everything.

---

## Self-Check Before Emitting

1. Can I state each task's success condition in one sentence?
2. Are there any `depends_on` I added out of habit rather than necessity?
3. Is every parallel frontier truly parallel (no hidden data dep)?
4. Is every expandable task too broad for one agent to own? (if not, make it atomic)
5. Is every atomic task scoped for one agent end-to-end? (if not, make it expandable)
6. Does my chain depth exceed 3? If yes, collapse a level.
7. Will any test, docs, or integration task start before the artifact it needs exists? If yes, add a dependency or collapse the pair.

---

## Using the Codebase Map

If you called `explore_codebase()`, your context includes a synthesized codebase map.

If you are running inside the 4-phase planning workflow, the runtime context may
already include `phase_outputs.synthesize.codebase_map` or an equivalent
synthesized handoff. In that mode, treat the synthesized map as authoritative
input and do **not** reopen exploration unless the phase-owned contract
explicitly tells you to do so.

**Use it to:**
- Set `touches_paths` from discovered files (not guesses)
- Set `touches_symbols` from discovered entry points
- Create `expandable=true` tasks for `coverage_gaps` (regions flagged incomplete)
- Identify `shared_foundations` → emit foundation tasks only when they are minimal, concrete, and true blockers for multiple branches
- Use `cross_cutting_concerns` to set correct `depends_on`
- Use `risk_hotspots` to set higher `priority` on conflict-prone tasks
- Prefer task boundaries that follow owned `touches_paths` clusters and validation targets
- For release/changelog goals, group by executable owned slices and blocker chains, not by changelog section headings

**Do NOT:**
- Ignore the map and plan from the goal text alone
- Create tasks for regions not in the map unless the goal explicitly requires it
- Over-decompose well-explored regions (the map gives enough info for atomic tasks)
- Turn broad changelog categories into task buckets when the codebase map already exposes narrower owned surfaces

---

## Output Contract

This skill runs in two valid execution modes. First determine which mode you are in
from the visible runtime tools and the prompt/runtime contract.

### Mode A — standalone decomposition tool

If the visible runtime tool surface explicitly includes `plan_tasks()`, call it
exactly once with the mixed graph.

- Do not wait to be re-invoked — you run exactly once per decomposition scope.
- The engine automatically spawns child coordinators for expandable tasks and ephemeral specialists for atomic tasks.
- Never assign tasks to yourself or any coordinator/explorer agent.
- Never ask clarifying questions.

### Mode B — planning-workflow `plan_tasks` phase

If you are inside the 4-phase planning workflow and a downstream formatter/posthook
will submit the plan for you:

- Do **not** call `plan_tasks()`, `submit_plan_tasks`, `coordination`, or any
  wrapper you invent.
- Do **not** reopen exploration with `run_parallel_agents()`,
  `query_phase_context()`, or repo-discovery tools unless the phase-owned contract
  explicitly allows it.
- Build the mixed graph directly from `phase_outputs.synthesize.codebase_map`
  and the current runtime context.
- Return the material needed for top-level `goal` and `tasks`; the downstream
  formatter/posthook submits the actual payload.
- In this workflow mode, `expandable: true` tasks may use the coordinator-owned
  agent name provided by the runtime contract (for example
  `phase_settings.expandable_task_agent_name`).

---

## Domain Reference Guides

For situation-specific patterns and examples, read the relevant reference:

- **Fullstack web application** → `references/fullstack-webapp.md`
- **API-only backend** → `references/api-backend.md`
- **Data pipeline / ML workflow** → `references/data-and-ml.md`
- **Infrastructure and DevOps** → `references/infra-and-devops.md`
- **Expandable vs atomic decision rubric** → `references/decomposition-rubric.md`
