# EphemeralOS Live E2E Control Room

Status: UI-first design draft. This document defines the proposed product model and
the contract the UI needs. It does not change test execution yet.

## 1. Product goal

Give an engineer one place to answer, before, during, and after a run:

1. Which runtime, manager, observability, and compound behaviors are covered?
2. Which feature tags does each family and test prove?
3. What is each test for?
4. Which named validations passed, failed, were skipped, or never ran?
5. Which exact host workspace was used, and can it be reused safely?

The UI is a test catalog first and a log viewer second. Raw pytest output remains
available as evidence, but it is not the primary information architecture.

## 2. Current-suite findings that shape the design

- Pytest currently expands to 373 cases: Runtime 226, Manager 103,
  Observability 2, Config/cross-surface 31, and Harness/preflight 11.
- The source contains 273 test functions. Only 153 currently have docstrings.
- The suite contains 1,174 raw assertions, 619 directly inside test functions.
  Pytest can report the first failing assertion, but it cannot stream every
  passing assertion as a named validation.
- Export, squash, git policy, reserved paths, and workspace/session already
  record useful evidence, but their verdict JSON formats are different.
- Current case IDs such as `MED-01` collide across families. UI identity cannot
  use the case ID alone.
- The 31 top-level `config/` cases cross manager, runtime, gateway, and
  observability boundaries. They are the best initial source for Compound.
- Core harness tests and gateway preflight checks are operational health, not
  product feature coverage.

## 3. Information architecture

```text
Catalog
├── Runtime
│   └── Family → optional group → test → named validations
├── Manager
│   └── Family → optional group → test → named validations
├── Observability
│   └── Family → optional group → test → named validations
└── Compound
    └── Simple | Medium | Complex → scenario → component tests + validations

Runs
└── Run → domain/family/test tree → validation evidence → artifacts

Workspaces
└── Retained workspace → lineage → runs → reuse eligibility
```

### Domain mapping

| UI domain | Initial families |
| --- | --- |
| Runtime | Command, daemon HTTP, file operations, network isolation, reserved paths, shell security, workspace/session, squash/remount |
| Manager | Lifecycle, export, squash |
| Observability | Snapshot, initially containing aggregate and sandbox-scoped groups |
| Compound | Configuration propagation plus new cross-domain lifecycle journeys |
| Preflight | Gateway, Docker, binaries, root safety; shown as runner health and excluded from feature coverage |

Folder nesting does not dictate product ownership. For example, a manager
squash case remains Manager when runtime calls merely prepare data and
observability calls merely collect evidence. A case becomes Compound only when
it intentionally validates a contract across at least two subject domains.

## 4. Feature model

Feature tags are centrally registered IDs, not arbitrary labels. This prevents
`workspace-session`, `workspace_session`, and `sessions` from becoming
three accidental features.

Each feature has:

- stable `id`
- user-facing title and description
- kind: `operation | behavior | quality | contract`
- optional owning domain

Every family, optional group, and test must declare at least one direct feature.
The UI can also show inherited/effective features, but direct and inherited tags
remain distinguishable. Each named validation maps to one or more effective test
features so the UI can show not only that a test passed, but which feature claim
was actually proven.

Initial behavior and quality tags include atomicity, concurrency, isolation,
security, attribution, recovery, teardown safety, and performance. Operation
features should be seeded from the existing operation catalog.

## 5. Test annotation contract

Use a thin pytest metadata decorator/marker plus a validation fixture. This keeps
metadata next to the code and avoids a second test framework.

```python
@e2e_test(
    id="runtime.workspace-session.ex04",
    title="Rider defers finalization",
    description=(
        "Proves an active rider keeps the implicit workspace session alive "
        "and finalization begins only after the rider exits."
    ),
    features=(
        "runtime.workspace-session",
        "behavior.concurrency",
        "contract.finalization",
    ),
    validations={
        "session-remains-active": "The session stays active while the rider runs.",
        "finalizes-after-rider": "The session finalizes after the last rider exits.",
        "teardown-clean": "No session or sandbox resource leaks after teardown.",
    },
)
def test_EX_04_rider_defers_finalization(..., validation):
    with validation(
        "session-remains-active",
        expected="active",
        actual=lambda: snapshot["state"],
        evidence=[snapshot_path],
    ):
        assert snapshot["state"] == "active"
```

Collection fails if a product-facing case lacks:

- a globally unique stable ID
- explicit domain and family ownership
- a non-empty description
- one or more registered feature IDs
- one or more declared named validations
- stable parameter IDs for parametrized cases

Parameterized case dictionaries carry their own title, features, and
validations. The catalog is generated after pytest collection so the UI sees all
373 expanded cases rather than only 273 source functions.

## 6. Catalog contract

The collection plugin materializes a versioned `catalog.json`:

```json
{
  "schema_version": 1,
  "catalog_revision": "sha256:…",
  "generated_at": "2026-07-12T12:00:00+08:00",
  "domains": [],
  "families": [],
  "groups": [],
  "features": [],
  "tests": []
}
```

A test entry includes:

```json
{
  "id": "runtime.file.read.offset-limit",
  "nodeid": "runtime/file/smoke/test_read_smoke.py::test_sessionless_read_with_offset_and_limit_over_multiline_file",
  "domain": "runtime",
  "family_id": "runtime.file",
  "group_id": "runtime.file.smoke",
  "title": "Windowed sessionless read",
  "description": "Returns only the requested line window.",
  "feature_ids": ["runtime.file.read", "behavior.windowing"],
  "execution_labels": ["smoke"],
  "validations": [
    {
      "id": "window-matches",
      "title": "Requested window is exact",
      "description": "The response contains only the requested lines.",
      "phase": "verify",
      "feature_ids": ["runtime.file.read", "behavior.windowing"],
      "required": true
    }
  ],
  "source": {
    "path": "runtime/file/smoke/test_read_smoke.py",
    "line": 41
  }
}
```

Every run snapshots its selected catalog entries. Historical pages therefore
keep the descriptions and feature mappings that were true when that run
occurred, even if the source catalog later changes.

## 7. Named validation lifecycle

Validation states are:

`pending | running | passed | failed | skipped | not_run`

Test and run states are:

`queued | running | passed | failed | error | skipped | cancelled`

The runner writes an append-only `events.jsonl` stream with monotonic sequence
numbers. A small HTTP/SSE adapter can tail the file for the UI; the on-disk
format remains useful without a server.

```json
{
  "schema_version": 1,
  "seq": 42,
  "at": "2026-07-12T12:03:14.102+08:00",
  "run_id": "01J…",
  "type": "validation.finished",
  "test_id": "runtime.file.read.offset-limit",
  "attempt": 1,
  "validation_id": "window-matches",
  "status": "passed",
  "duration_ms": 12.4,
  "expected": "lines 3–5",
  "actual": "lines 3–5",
  "evidence": ["artifacts/read-response.json"]
}
```

The reporter also materializes `run.json` and one atomic `result.json` per
test attempt. If a test aborts, declared validations that never start become
`not_run`; they must never look passed by omission.

Existing rich recorders are adapted first:

| Existing evidence | Initial normalized validations |
| --- | --- |
| Git policy axes | correctness, attribution, isolation, teardown |
| Export axes | correctness, host safety, incremental behavior, runnable output, teardown |
| Squash axes | correctness, space, time, teardown |
| Reserved paths axes | correctness, data safety, isolation, teardown |
| Workspace/session axes | correctness, timing, teardown |

Raw assertions remain available in failure evidence. Important checks are
wrapped gradually; the UI exposes annotation coverage so incomplete migration
is visible.

## 8. Compound tests

Compound is a first-class shared-context scenario, not merely a filter that
runs unrelated pytest nodes.

Each compound case declares:

- complexity: exactly `simple | medium | complex`
- at least two unique subject domains
- ordered components, with role `subject | fixture | evidence`
- its own cross-domain validations
- a shared run workspace and explicit teardown contract

Complexity guidance:

| Level | Intended shape |
| --- | --- |
| Simple | One happy-path lifecycle across two or three domains, no concurrency |
| Medium | All three domains, negative paths or multiple lifecycle transitions |
| Complex | Concurrency, fault injection, multiple sandboxes, recovery, or long-running soak |

Existing `easy`, `medium`, `hard`, and `complex` pytest markers remain
family-specific execution tiers. They are not automatically converted to
Compound complexity.

Initial scenario:

`manager.create → runtime.exec/write → observability.snapshot → manager.destroy`

Its validations prove creation, runtime visibility, observability correlation,
and clean teardown against one shared sandbox and workspace.

## 9. Durable workspace store

All mutable run state moves outside the source repository:

```text
/Users/yifanxu/Ephemeral-AI-Lab/ephemeral-sandbox-test-workspace/e2e/
├── root.json
├── templates/
│   └── testbed/
├── cache/
├── runs/
│   └── <run-id>/
│       ├── manifest.json
│       ├── run.json
│       ├── events.jsonl
│       ├── tests/<test-id>/attempt-1/result.json
│       ├── workspaces/<test-id>/attempt-1/
│       │   ├── root/
│       │   └── workspace.json
│       └── artifacts/
└── retained/
    └── <workspace-id>/workspace.json
```

Rules:

1. A test always gets an isolated run-scoped root.
2. The exact root used is retained with lineage, before/after digests, disk
   size, image, git revision, test ID, and teardown result.
3. Reuse clones a retained root into a new run root. Previous evidence is never
   mutated in place.
4. A root becomes reusable only after a passing test and clean teardown.
5. Sandbox containers and sessions are still destroyed; retaining a host
   workspace does not retain live compute.
6. Artifact references are relative to the run root. Existing stale absolute
   paths are normalized by adapters.
7. No automatic deletion in the first version. Purge is an explicit action with
   size and lineage shown in the UI.

The runner uses one new root setting, `E2E_WORKSPACE_STORE`, defaulting to the
path above. Existing helpers that use `tmp_path` or `mkdtemp()` must migrate
to a store-backed fixture; changing `E2E_WORKSPACE_ROOT` alone cannot preserve
all current workspaces.

## 10. Proposed pages

### Catalog / coverage

- Four domain cards with family, test, feature, and last-run summaries
- feature coverage matrix, not only test counts
- metadata health: descriptions, tags, and named-validation coverage
- runner/preflight status separated from product coverage

### Domain and family

- family navigation and optional test-group tabs
- dense test list with description, direct feature tags, validation count,
  execution labels, and last result
- filter by feature, status, group, or text

### Test contract

- “What this proves” description
- feature tags and source location
- declared validation checklist before a run
- expected, actual, evidence, duration, and error after/during a run
- recent result history and workspace lineage

### Compound catalog

- Simple, Medium, and Complex lanes
- component-domain map and shared-state boundary
- cross-domain validation list

### Live/completed run

- domain/family/test tree with aggregate state
- selected test contract and live validation transitions
- setup/execute/verify/teardown phase trail
- structured expected/actual evidence first; raw logs secondary
- the same page remains useful after completion

### Workspaces

- fixed store root and safety state
- retained roots with source run, digest, disk use, last use, and reuse status
- “Run from copy” as the primary action
- explicit purge and lineage details

## 11. Scheduling rules the UI must expose

- Config/gateway replacement uses an exclusive serial lane.
- Docker, gateway, and image pulls are shared resources with visible readiness.
- A global ULID/UUID run ID is generated once and propagated to every recorder.
- Phase 1 uses one event writer. If pytest-xdist is added later, workers send
  events to the controller or produce streams that are merged deterministically.

## 12. Incremental delivery

1. Add global run IDs, catalog snapshots, normalized result adapters, and the
   event stream without changing test behavior.
2. Annotate families and expanded tests; import existing case titles and axes.
3. Add named validation wrappers to the highest-value checks and show migration
   coverage in the catalog.
4. Route all workspace creation through the external store and verify safe reuse.
5. Add Compound journeys and complexity.
6. Promote the approved prototype to a dedicated React/Mantine E2E web app,
   reusing the benchmark laboratory's SSE and evidence patterns.

## 13. Design acceptance criteria

- Every product-facing collected case has a stable unique ID, description,
  family, feature tags, and at least one named validation.
- Every visible validation maps to one or more feature tags.
- Passing validations are visible live; unstarted validations become
  `not_run`.
- Compound cases show complexity and component-domain provenance.
- The exact run workspace is retained under the external root and can only be
  reused by cloning.
- Historical runs render from their catalog snapshot and relative artifacts.
- Preflight/harness failures are visible but never inflate product coverage.
