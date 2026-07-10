# manager · management — test spec

Test plan for the `management` family. Two parts per operation:

- **(a) Features to be tested**
- **(b) Test files to be created**

## Conventions

- **CLI-driven**: every action goes through `sandbox-cli manager <op>`; results
  are verified from the operation's **JSON** (`json.loads`), never from logs.
- **Lifecycle via fixtures**: the `sandbox` fixture in `conftest.py`
  creates-and-teardown a ready sandbox so cleanup runs even on failure.
- **Cleanup is guaranteed**: per-test fixtures destroy what they create, and a
  session-wide safety net (`core/cleanup.py` + the `_session_sandbox_cleanup`
  finalizer) destroys any sandbox the suite created but a test leaked. Only
  suite-created ids are touched — never other clients' sandboxes. Inline-create
  tests still register automatically via `helpers.create_sandbox`.
- **Workspace variants**: `--workspace-root` is a **host path** the Docker
  backend bind-mounts into the sandbox. Variants live under `repo/` (one host
  dir each: `repo/testbed` default, `repo/special_case_b`, …), selected via
  `E2E_WORKSPACE_VARIANT` or `config.workspace_variant("name")`.
- **Helpers**: thin wrappers in `manager/management/helpers.py`
  (`create_sandbox`, `inspect_sandbox`, `list_sandboxes`, `destroy_sandbox`,
  `get_observability_tree`).
- **Error shape**: failures return `{"error": {"kind", "message", "details"}}`;
  missing required CLI flags are a usage error (exit 2), not a request.

## Operation → file → focus (quick map)

| Operation                | Test file                          | Status   |
|--------------------------|------------------------------------|----------|
| `create_sandbox`         | `test_create_sandbox.py`           | active   |
| `inspect_sandbox`        | `test_inspect_sandbox.py`          | active   |
| `list_sandboxes`         | `test_list_sandboxes.py`           | active   |
| `destroy_sandbox`        | `test_destroy_sandbox.py`          | active   |
| `get_observability_tree` | `test_get_observability_tree.py`   | deferred |
| (cross-operation)        | `test_management.py` (existing)    | active   |

`test_management.py` stays as a single end-to-end lifecycle/integration test
(create → inspect → list → destroy). The per-operation files below add focused
feature coverage.

---

## 1. create_sandbox

`--image` (req), `--workspace-root` (req) → `{id, workspace_root, state, daemon:{host,port}}`.
On any failure the daemon, runtime sandbox, and record are rolled back.

### (a) Features
- **F1 happy path** — returns a non-empty `id`, `state == "ready"`, and a
  `daemon` with `host` + integer `port`.
- **F2 workspace_root round-trips** — returned `workspace_root` equals the host
  path passed (the bind-mounted variant).
- **F3 workspace variant selection** — create against `repo/testbed` and an
  alternate variant (e.g. `repo/special_case_b`); each record reflects its path.
- **F4 custom image honored** — non-default image (e.g. `debian:12`) succeeds
  (mark `slow`; pulls if absent).
- **F5 invalid/empty image rolls back** — empty `--image` →
  `error.kind == "invalid_request"`, and the sandbox is absent from
  `list_sandboxes` afterward (rollback verified).
- **F6 missing required arg** — omitting `--image` or `--workspace-root` is a CLI
  usage error (no sandbox created).
- **F7 nonexistent workspace_root** — a host path that does not exist surfaces a
  backend error (mount failure); documents expected behavior.
- **F8 special / invalid files in workspace** — a workspace containing a
  non-regular file (FIFO/named pipe, socket, block/char device), an unreadable
  file, or a broken symlink → `create_sandbox` fails and rolls back. See
  *base-build error mechanism* below.
- **F9 workspace mutated during base hashing** — a file/dir removed while the
  base is being walked/copied (TOCTOU race) → `create_sandbox` fails and rolls
  back. Same error class as F8 (`unstable` instead of `special`). Inherently
  racy to trigger.

> **Base-build error mechanism (F8/F9).** At daemon startup `create_sandbox`
> builds the layerstack workspace base by walking the bind-mounted workspace
> (`layerstack/src/workspace_base/layer.rs`). Non-regular/unreadable entries are
> collected as `special`; entries that vanish mid-walk as `unstable`. A non-empty
> `special`/`unstable` set makes the base build return
> `Storage("workspace base must be a full copy; special=… unstable=…")`, which
> panics `Services::from_config`, kills the daemon, and surfaces to the client as
> `create_sandbox` error `internal_error` "sandbox daemon install failed: start
> daemon …". **Structured assertion is limited to failure + rollback** — the
> precise `special=/unstable=` reason is only in the daemon log, not the JSON.

### (b) Test files
- `test_create_sandbox.py`
  - `test_creates_ready_sandbox` (F1) — uses `sandbox` fixture or inline create+destroy.
  - `test_workspace_root_round_trips` (F2)
  - `test_workspace_variant_selection` (F3) — parametrized over variant names.
  - `test_custom_image` (F4) — `@pytest.mark.slow`.
  - `test_invalid_image_rolls_back` (F5)
  - `test_missing_required_args` (F6) — parametrized over the two flags.
  - `test_nonexistent_workspace_root` (F7) — may `xfail` until behavior is pinned.
  - `test_special_file_in_workspace_fails` (F8) — build a throwaway workspace dir
    with a FIFO (`os.mkfifo`) + a regular file, pass it as `workspace_root`,
    assert `create_sandbox` returns an error and the id is absent from
    `list_sandboxes` (rollback). Tear the temp dir down in `finally`.
  - `test_workspace_mutated_during_hash_fails` (F9) — best-effort: a large temp
    workspace plus a background thread deleting files while `create_sandbox`
    runs; assert failure + rollback. Mark `@pytest.mark.flaky`/`skip` by default
    since the race is timing-dependent; documents the `unstable` path.

---

## 2. inspect_sandbox

`--sandbox-id` (req) → single record; unknown id → not found.

### (a) Features
- **F1 inspect existing** — returns a record whose `id` matches, `state == "ready"`,
  with `workspace_root` and a populated `daemon` endpoint.
- **F2 consistency** — the inspected record matches what `create_sandbox` /
  `list_sandboxes` report for the same id.
- **F3 unknown id** — `error.kind == "invalid_request"`,
  message `"sandbox not found: <id>"`.
- **F4 missing arg** — omitting `--sandbox-id` is a usage error.

### (b) Test files
- `test_inspect_sandbox.py`
  - `test_inspect_returns_matching_record` (F1, F2) — uses `sandbox` fixture.
  - `test_inspect_unknown_id` (F3)
  - `test_inspect_missing_arg` (F4)

---

## 3. list_sandboxes

No args → `{sandboxes: [...]}` sorted by id; each record's `daemon` is `null`
until `ready`.

### (a) Features
- **F1 shape** — `sandboxes` is a list; each item has `id`, `workspace_root`,
  `state`, `daemon`.
- **F2 visibility** — a created sandbox appears in the list; after destroy it is
  gone.
- **F3 sorted by id** — results are ordered by `id` (needs ≥2 sandboxes).
- **F4 ready record shape** — for a `ready` sandbox, `daemon` is
  `{host, port}` (non-null).

### (b) Test files
- `test_list_sandboxes.py`
  - `test_returns_array` (F1)
  - `test_created_then_destroyed_visibility` (F2) — uses `sandbox` fixture for
    the appear half; destroys and re-lists for the disappear half.
  - `test_sorted_by_id` (F3) — creates two sandboxes (parametrized fixture).
  - `test_ready_record_shape` (F4)

> Note: an "empty list when none exist" assertion is intentionally **not**
> included — the suite runs against a shared gateway that may hold other
> sandboxes. Assertions target the suite's own ids, never the global count.

---

## 4. destroy_sandbox

`--sandbox-id` (req) → removed record, final `state == "stopped"`. Unknown id →
not found. A sandbox in `creating`/`stopping` is rejected. If runtime destroy
fails the record is left `failed`.

### (a) Features
- **F1 destroy ready** — `state == "stopped"`; the id is absent from a
  subsequent `list_sandboxes`.
- **F2 unknown id** — `error.kind == "invalid_request"`,
  message `"sandbox not found: <id>"`.
- **F3 double destroy** — destroying an already-removed id returns not found
  (idempotency expectation, no crash).
- **F4 reject in-flight state** — a sandbox in `creating`/`stopping` is rejected
  (hard to trigger deterministically; documented, automated later).
- **F5 missing arg** — omitting `--sandbox-id` is a usage error.

### (b) Test files
- `test_destroy_sandbox.py`
  - `test_destroy_ready_sandbox` (F1) — inline create (no `sandbox` fixture, to
    avoid double-destroy in teardown).
  - `test_destroy_unknown_id` (F2)
  - `test_double_destroy` (F3)
  - `test_destroy_missing_arg` (F5)
  - (F4 documented; not automated in the first pass.)

---

## 5. get_observability_tree — DEFERRED

`--sandbox-id` (opt), `--resource-window-ms` (opt) → `{sandboxes: [node, …]}`.
`availability` ∈ `available | partial | unavailable`; unreachable/non-ready
sandboxes become `unavailable` nodes (with `errors`) instead of failing the call.

This is the **manager-side observability aggregate**. Per the project decision,
observability is verified **through runtime** later, so these tests are deferred
(mirroring `observability/`).

### (a) Features (planned)
- **F1** — no id aggregates all ready sandboxes; one node each.
- **F2** — `--sandbox-id` returns a single node for that sandbox.
- **F3** — node shape: `lifecycle_state`, `availability`, `daemon`,
  `resources{latest, history}`, `workspaces[]`.
- **F4** — a non-ready/unreachable sandbox yields an `unavailable` node with
  `errors[]`, and the overall call still succeeds.
- **F5** — `--resource-window-ms` bounds the returned resource history.

### (b) Test files (planned)
- `test_get_observability_tree.py` — skipped placeholder (`@pytest.mark.skip`)
  until the observability phase; then it lands here and/or under
  `observability/` per the runtime-verified plan.

---

## Implementation notes

- New per-operation files import family helpers
  (`from manager.management import helpers as mgmt`) and use the shared
  `sandbox` fixture where a ready sandbox is a precondition. Destroy-focused
  tests create inline to avoid fixture-teardown double-destroy.
- Add markers to `pytest.ini` as needed: `slow` (image pulls), and reuse
  `skip` for deferred coverage.
- New workspace variants: create `repo/<name>/` and reference via
  `config.workspace_variant("<name>")`.
