# Decomposition Decision Rubric

Use this reference when you are unsure whether a unit of work should be **expandable** or **atomic**, or when you need to determine the right grain size.

---

## Primary Test: The Atomicity Question

> **Can one agent own and complete this end-to-end without coordinating with another concurrent agent mid-task?**

| Answer | Action |
|---|---|
| Yes, clearly | Atomic task — assign to specialist, set `expandable: false` |
| Yes, but it touches 5+ files across 3+ layers | Still atomic if one agent can reason about all of it — just write a clear description |
| No, it needs multiple independent parallel tracks | Expandable — write expansion_hint describing the tracks |
| Unsure | Apply the tiebreakers below |

---

## Tiebreaker 1: The Naming Test

Can you name the deliverable in a single noun phrase?

- "The ProductCard React component" → atomic
- "The catalog API implementation" → too broad → expandable
- "The GET /products endpoint with pagination" → atomic
- "The user authentication system" → too broad → expandable
- "The Alembic migration for the Product table" → atomic

If your description uses the word "system", "layer", "module", or "subsystem", it is almost certainly expandable.

---

## Tiebreaker 2: The File Count Test

How many files will this task create or significantly modify?

| Files touched | Guidance |
|---|---|
| 1–3 files | Almost always atomic |
| 4–6 files | Atomic if they form one cohesive deliverable (e.g. component + its hook + its test) |
| 7–10 files | Expandable unless they are trivially related (e.g. bulk renaming) |
| 10+ files | Always expandable |

Exception: DevOps setup tasks often touch many config files but are still atomic because they follow a deterministic procedure a single agent can execute top-to-bottom.

---

## Tiebreaker 3: The Retry Test

If this task fails, what is the cost of retrying it from scratch?

| Retry cost | Guidance |
|---|---|
| Low (< 5 min, no external side effects) | Atomic is fine |
| Medium (5–30 min, runs migrations or installs) | Consider splitting at natural checkpoints |
| High (> 30 min, many LLM calls, external API calls) | Split into smaller expandable stages |

---

## Tiebreaker 4: The Parallelism Test

Does this task contain internal parallelism you are throwing away by making it atomic?

Example: "Implement all CRUD endpoints for the catalog domain" contains 5 independent endpoints that could run in parallel. Making this atomic forces one agent to do sequential work that 5 agents could do concurrently.

Rule: If an atomic task description contains the words "all", "each", "for every", or lists multiple independent items, it may be a covert expandable task.

---

## Common Expandable Patterns

These descriptions should almost always be expandable:

| Description | Why |
|---|---|
| "Implement the X API" | Multiple endpoints, models, and services |
| "Build the X UI" | Multiple components and hooks |
| "Set up the X domain" | Spans models, services, and routes |
| "Write tests for X" | Multiple test files and fixture setups — split by domain subset to avoid blowing the worker tool budget |
| "Implement the auth system" | Multiple endpoints, middleware, and client code |
| "Build the data pipeline" | Multiple extract/transform/load stages |

---

## Common Atomic Patterns

These descriptions should almost always be atomic:

| Description | Agent |
|---|---|
| "Implement the ProductCard component" | frontend-developer |
| "Write the GET /products endpoint with filtering" | backend-developer |
| "Add the Product ORM model and migration" | backend-developer |
| "Write pytest fixtures for DB session" | test-engineer |
| "Write the Dockerfile for the backend service" | devops-engineer |
| "Implement the useCart hook" | frontend-developer |
| "Add the catalog router to main.py" | backend-developer |
| "Write integration tests for the orders API" | test-engineer |
| "Create .env.example with all required variables" | devops-engineer |

---

## Grain Size by Depth

| Depth | Typical grain | Expandable? | Example |
|---|---|---|---|
| 0 (root) | Entire application | Almost always → expandable | "Build e-commerce platform" |
| 1 | Major subsystem | Usually expandable | "Build catalog API spine" |
| 2 | Domain component | Sometimes expandable | "Implement catalog service layer" |
| 3 | Single deliverable | Usually atomic | "Write GET /products endpoint" |
| 4+ | Micro task | Always atomic | "Add pagination to product list query" |

Recursion stops when everything at a given level is atomic. Most real projects reach a stable atomic state at depth 2–3.

---

## Dependency Rubric

Only add `depends_on` when **all three** are true:
1. Task B needs a file or symbol that Task A will create or significantly modify
2. Task B cannot make a reasonable assumption about Task A's output shape
3. Running B before A would cause an import error, missing table, or undefined symbol

If only 1 or 2 are true: make a reasonable assumption, add a comment in the description, run in parallel.

**Never add deps for:**
- "Might edit the same file" → OCC (optimistic concurrency control) handles this
- "Could be related" → default to parallel, override with evidence
- "Cleaner if sequential" → cleaner does not equal faster
