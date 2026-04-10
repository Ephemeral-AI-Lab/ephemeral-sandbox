# Task Planning and Decomposition Guide

Use this reference when the planner is ready to turn scout-backed ownership into a submitted DAG, or when you are unsure whether a slice should stay atomic or become an expandable child `team_planner` lane.

This guide is about graph shape after ownership is known. Use `exploration-script.md` for how to discover ownership in the first place.

---

## Primary Question

Ask first:

> Can one worker own and complete this slice end-to-end without coordinating with another concurrent worker mid-task?

| Answer | Action |
|---|---|
| Yes, clearly | Emit one atomic `developer` or `validator` lane |
| No, the slice contains multiple independent tracks or failure domains | Emit `kind: "expandable"` targeting `team_planner` |
| Unsure | Apply the tiebreakers below |

Default mapping:
- Production-code ownership -> `developer`
- Verification-only ownership -> `validator`
- Multi-slice or still-overbroad ownership -> expandable `team_planner`

---

## Atomic vs Expandable Tiebreakers

### 1. Naming test

Can you name the deliverable as one concrete owned slice?

- Good atomic examples:
  - `make import fallback work in pandas/io/parsers/readers.py`
  - `update one compatibility shim for ndarray alias handling`
  - `verify fail-to-pass target in tests/test_foo.py::test_bar`
- Usually expandable:
  - `fix the backend`
  - `handle compatibility`
  - `update the auth system`
  - `write all tests for X`

If the description naturally wants words like `system`, `layer`, `module`, `subsystem`, `all`, or `remaining`, it is usually too broad for one atomic worker.

### 2. Hidden parallelism test

If an atomic lane would throw away clear internal parallelism, keep it expandable.

Signals:
- multiple disjoint FAIL_TO_PASS clusters
- multiple independent source-owner files or directories
- several unrelated validation targets
- wording such as `all`, `each`, `every`, or a list of separate behaviors

### 3. File-cluster test

File count is only a hint, but it helps:

| Scope | Guidance |
|---|---|
| 1-3 tightly related files | Usually atomic |
| 4-6 files in one cohesive behavior slice | Atomic if one worker can still own it end-to-end |
| 7+ files across multiple layers or directories | Usually expandable |

Exception: a deterministic repo-config patch can stay atomic even if it touches several related config files.

Important: one monolith file is not automatically atomic. If that file fronts many explicit failing targets, multiple named behavior families, or a wide compatibility/protocol matrix, prefer an expandable child `team_planner` lane that shards by region or behavior family.

### 4. Retry and failure-domain test

If a failed attempt would waste a large worker budget or conflate unrelated causes, split it.

Keep separate:
- distinct implementation clusters
- unrelated validation surfaces
- independent FAIL_TO_PASS root causes

Collapse only when the same owner would inevitably perform the steps together anyway.

---

## Width Targets By Level

### Root submitted level

- Keep the submitted root within `1-10` total tasks.
- For benchmark roots, the first ready frontier may be narrow while the total graph is larger. Do not confuse frontier cap with total plan size.
- If the natural root plan exceeds 10 siblings, merge adjacent work into disjoint expandable `team_planner` branches.

Healthy root shapes usually look like:
- `2-4` implementation lanes plus one downstream verifier
- or `2` critical implementation lanes plus one downstream expandable planner branch plus one verifier when residual work is real but not yet execution-sized

### Child planner level

- Child planners should usually return `2-5` execution-sized items.
- If only one meaningful child slice remains, emit execution work instead of another planner wrapper.
- If a child still owns multiple independent tracks, split them into disjoint narrower branches rather than returning one broad omnibus child again.

---

## Dependency and Depth Rules

### Default to parallel

Use `depends_on` only for real producer/consumer flow.

Good reasons for a dependency:
- one lane creates or changes the interface another lane needs
- one lane is a true unlocker for another lane's execution
- one lane is a validator or integration consumer of another lane's artifact

Bad reasons for a dependency:
- `might edit the same file`
- `same changelog theme`
- `cleaner if sequential`

### Keep branch depth shallow

Target at most `3` serial layers inside one branch:

`unlocker -> implementation frontier -> verification/integration`

If a branch gets deeper than that:
- collapse adjacent same-owner steps, or
- push the remaining breadth into an expandable child planner

### Collapse trivial serial pairs

Typical collapsed pairs:
- model + migration
- repository + thin service
- router edit + registration
- tiny helper + its only caller when both implement the same behavior fix

Do not collapse:
- independent owner clusters
- separate FAIL_TO_PASS root causes
- unrelated validation or docs surfaces

---

## Bottlenecks and Shared Foundations

A shared foundation is valid only when it is both small and truly required by multiple downstream lanes.

Rules:
- If three or more siblings would depend on one candidate unlocker, prove they all need that exact prerequisite now.
- If only one subset needs it, fold it into that subset or split it by consumer.
- Heavy setup, broad scaffolding, or large verification should stay lane-local or downstream instead of becoming one global blocker.

Avoid umbrella blockers such as:
- `compatibility foundation`
- `backend setup`
- `remaining cleanup`
- `all tests`

unless the evidence shows a genuinely tiny shared unlocker.

---

## Verification, Integration, and Docs Placement

Keep these late unless they are strict unlockers:

- validators
- integration or wiring passes
- descriptive docs
- polish or cleanup

Guidance:
- Validators should usually depend on the `developer` lanes they exercise.
- A validator that only verifies one concrete developer lane should stay attached to that lane even when a disjoint expandable child planner sibling is also present.
- Validators must not depend directly on an expandable child planner. If a branch still needs child planning, keep the validation inside that branch or behind the concrete developer lanes the child planner emits.
- Do not add a dependency from a disjoint expandable child planner to an unrelated developer just to keep a root validator "behind" that child branch. Keep the child ready immediately and place residual validation inside the child branch.
- If verification spans multiple independent flows or domains, keep it downstream and expandable rather than as one giant validator lane.
- Integration should depend only on the lanes it truly consumes.
- Descriptive docs should follow the behavior they describe; bootstrap docs may run earlier only when they unblock work.

Never use a validator lane to absorb a known unowned FAIL_TO_PASS cluster. That cluster needs its own `developer` lane or child `team_planner` branch first.

---

## Writing Expandable Branches

An expandable lane should describe the next owned slices, not a serial file checklist.

Good:
- `split parser compatibility work into csv reader shim, dtype normalization path, and downstream validation`
- `parallel next slices: import fallback in module A; behavior guard in module B; then one validator lane for the named fail-to-pass targets`

Bad:
- `edit file_a.py, then file_b.py, then file_c.py`
- `investigate remaining issues`
- `fix backend and tests`

For each expandable branch:
- narrow to one owned slice or one coherent wave structure
- keep child branches disjoint
- do not reopen sibling branches outside that slice

---

## Common Anti-Patterns

Avoid these shapes:

- One omnibus `developer` lane for multiple known root causes
- One dominant `developer` lane that absorbs most known FAIL_TO_PASS evidence just because the failures all touch one large owner file
- `developer + developer + validator` at the root when additional owned FAIL_TO_PASS clusters are already known
- One global `all tests` validator lane in the first frontier
- Theme buckets such as `compatibility`, `cleanup`, or `CI + docs`
- Child planner chains that each return only one narrower planner child without producing execution work

Prefer graphs where:
- independent owned slices stay separate
- trivial same-owner serial work is collapsed
- residual breadth is parked behind explicit expandable `team_planner` branches
- validators consume owned implementation lanes instead of discovering them
