# Phase 03 - OCC Changeset API And Routing Implementation Report

Companion to
[`phase-03-occ-changeset-routing.md`](./phase-03-occ-changeset-routing.md).

This report records the current Phase 03 implementation after the OCC package
structure cleanup.

---

## 1. Verdict

**Phase 03 is implemented on the typed service path and the legacy OCC runtime
wire path has been removed from the OCC package.**

OCC now accepts typed mutation intent objects, routes normalized paths into
tracked/direct/drop/reject groups, infers tracked base hashes from leased layer
stack manifests, and returns `PreparedChangeset` objects for the Phase 04
commit transaction.

The `occ/` package now matches the Phase 03/04 target shape. It no longer
contains `wire.py`, `handlers/`, `orchestrator.py`, `direct/`, `gated/`,
`content/`, or `patching/` source modules.

---

## 2. File Inventory

### OCC Package

| File | Purpose |
| --- | --- |
| `backend/src/sandbox/occ/client.py` | Typed `OCCClient` plus sandbox-id to typed-service binding helpers |
| `backend/src/sandbox/occ/runtime_ops.py` | Byte hashing and leased-manifest base-hash inference helpers |
| `backend/src/sandbox/occ/service.py` | `OccService.prepare_changeset` and `apply_changeset` entrypoint |
| `backend/src/sandbox/occ/changeset/types.py` | `Change`, `WriteChange`, `EditChange`, `DeleteChange`, direct shell change types, and result objects |
| `backend/src/sandbox/occ/changeset/builders.py` | API and shell source builders |
| `backend/src/sandbox/occ/changeset/prepared.py` | `PreparedChangeset`, `PreparedPathGroup`, `RouteDecision`, and `CommitIntent` |
| `backend/src/sandbox/occ/routing/gitignore.py` | Cached gitignore oracle |
| `backend/src/sandbox/occ/routing/router.py` | Path normalization, routing, grouping, and concurrent preparation |

### Runtime Bridge

| File | Purpose |
| --- | --- |
| `backend/src/sandbox/runtime/overlay_shell/capture_to_changeset.py` | Converts Phase 02 upperdir captures into typed OCC changes outside the OCC package |

---

## 3. Behavior Delivered

### Typed Mutation Sources

`Change.source` identifies the mutation origin:

```text
api_write
api_edit
shell_capture
```

`WriteChange` stores bytes, `EditChange` stores one search/replace anchor, and
`DeleteChange` carries an optional base hash for tracked deletion checks.

### Routing And Preparation

`OccOrchestrator` normalizes paths and emits ordered `PreparedPathGroup` values:

- tracked workspace paths route to `tracked`
- gitignored paths route to `direct`
- `.git` and descendants route to `drop`
- absolute paths and parent traversal route to `reject`
- direct shell-only kinds route to `direct` without a gitignore lookup

Tracked write/delete changes with no caller-supplied base hash receive their
base hash from the leased manifest through `infer_manifest_base_hash(...)`.

### Client Path

`OCCClient` no longer serializes changes to a runtime `occ.apply_changeset`
operation. Callers bind a typed service directly or register one for a
sandbox id:

```text
register_occ_service(sandbox_id, OccService(...))
OCCClient(sandbox_id).apply_changeset(changes)
-> OccService.apply_changeset(...)
```

`sandbox.api.write` and `sandbox.api.edit` now use this typed service binding
instead of the removed runtime wire codec.

---

## 4. Deleted Legacy Source

The cleanup removed the old live-root OCC runtime path:

```text
backend/src/sandbox/occ/wire.py
backend/src/sandbox/occ/bootstrap.py
backend/src/sandbox/occ/setup.sh
backend/src/sandbox/occ/handlers/
backend/src/sandbox/occ/orchestrator.py
backend/src/sandbox/occ/direct/
backend/src/sandbox/occ/gated/
backend/src/sandbox/occ/content/
backend/src/sandbox/occ/patching/
```

The runtime server now loads the overlay-capture bootstrap only; it no longer
registers an in-sandbox `occ.apply_changeset` handler.

---

## 5. Verification

Focused OCC/API/runtime slice:

```bash
uv run pytest backend/tests/test_sandbox/test_occ backend/tests/test_sandbox/test_api/test_write.py backend/tests/test_sandbox/test_api/test_edit.py backend/tests/test_sandbox/test_api_contract.py backend/tests/test_sandbox/test_runtime/test_shell_pipeline.py backend/tests/test_sandbox/test_runtime/test_bundle_upload.py backend/tests/test_sandbox/test_runtime/test_setup_orchestrator.py -q
```

Result:

```text
72 passed in 1.67s
```

Full sandbox suite:

```bash
uv run pytest backend/tests/test_sandbox -q
```

Result:

```text
345 passed in 5.19s
```

Sandbox lint:

```bash
uv run ruff check backend/src/sandbox backend/tests/test_sandbox
```

Result:

```text
All checks passed!
```

---

## 6. Remaining Integration Boundary

The deleted runtime OCC handler is not replaced by another live-root
compatibility wrapper. Mutating shell execution now needs the Phase 06
layer-stack integration to pass a typed changeset applier into the shell
pipeline. Until that integration supplies the service binding, the pipeline
does not silently rebuild the removed orchestrator path.
