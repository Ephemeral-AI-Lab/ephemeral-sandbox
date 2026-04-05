# Dependency Guidance

Add `depends_on` only when execution order is materially required.

Prefer parallelism by default:

- If two tasks can proceed independently, leave them parallel.
- Use dependencies to encode real execution constraints, not preferred sequencing.
- A good plan exposes as much safe concurrency as possible.

Use a dependency when:

- Task B edits code that assumes Task A's API or behavior change already exists.
- Task B is a validation or cleanup task for Task A's implementation.
- Both tasks touch the same hotspot and sequential execution reduces conflict.
- A shared foundation must land first before downstream tasks are safe.
- You can state the blocker in one short sentence such as "B needs the API from A" or "B validates A's behavior change".
- Task B is a docs, doctest, warning-expectation, or release-cleanup task whose expected output depends on Task A's behavior change.
- Task B is a focused test/docs follow-up for one implementation lane and updates that lane's own expectations, docs, or warnings.
- Two draft root tasks primarily edit the same file, same path prefix, or same symbol family and one clean split is not obvious.

Do not add a dependency when:

- The tasks are just thematically related.
- The tasks touch different subsystems and can run independently.
- The order is a preference rather than a requirement.
- The same agent will probably do both tasks.
- The tasks were listed in different planning phases.
- The only rationale is that both tasks are "foundational", "cross-cutting", or part of the same release note bucket.
- The tasks share a nearby utility or parent package name but do not directly overlap in owned files, symbols, or produced behavior.

Preferred dependency patterns:

- Foundation -> implementation
- Implementation -> focused test cleanup
- Implementation -> docs or doctest update for that behavior
- Implementation -> warning/expectation update for that behavior
- Shared hotspot task -> follow-up task on the same file

Root-task audit:

- A root task should represent either a safe parallel lane or a real blocker chain head.
- If two root tasks overlap on the same hotspot files or symbols, they are not independent just because their changelog bullets differ.
- If one root task exists only to validate, document, or clean up another root task's behavior, connect them with `depends_on`.
- If a draft task mixes implementation and follow-up validation or docs work, split it before deciding dependencies.
- When overlap is high and sequencing is ambiguous, prefer one `expandable: true` bucket over two falsely parallel roots.

Maintain sequential flexibility:

- Keep dependencies local to the tasks that truly need them.
- Allow later tasks to start as soon as their direct blockers are done.
- Prefer a few short dependency chains over one rigid serialized phase ladder.

Avoid:

- Long dependency chains created only from phase labels
- Artificial "docs last" or "CI last" edges unless the change truly depends on code landing first
- Making every test task depend on every implementation task
- Creating one release-wide `test-adjust`, `docs-update`, or `cleanup` root that depends on many unrelated implementation roots when the follow-up work can stay attached to each owning lane
- Creating a synthetic `core-infra` or similar foundation root just because several lanes import the same utility files, without a concrete shared blocker
- Using dependencies to create a neat review stack when the work itself is independent

Examples:

- Good:
  - `test-adjust-warning-expectations` depends on `add-warning-behavior`
  - `move-tests-to-new-module` depends on `refactor-core-behavior`
  - `docs-update` depends on the behavior change it documents when the text would otherwise be stale
  - `backend-cleanup` and `frontend-cleanup` run in parallel because they do not share code or APIs
  - `tokenize-highlevelgraph` and `dataframe-deprecations` run in parallel when they touch different files and neither task consumes the other's API or behavior
- Bad:
  - `docs-update` depends on all code tasks just because it appears later in the plan
  - `compatibility-fixes` depends on `api-cleanup` with no shared implementation edge
  - Every task depends on the previous one just to preserve a neat phase order
  - Two root tasks remain parallel even though both primarily edit the same hotspot files or symbol family
  - A validation-only task stays independent even though it mainly updates expectations for behavior introduced elsewhere
  - `dataframe-deprecations` depends on `tokenization-improvements` only because both are cross-cutting release work or both touch imported-by-many helpers
