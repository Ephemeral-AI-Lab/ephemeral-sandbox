# Sandbox API + Runtime Refactor — Implementation Steps

Per-step implementation plans, ordered by merge sequence. Each step ends green: `make build`, `ruff check`, `make test` all pass. Old code paths coexist with new ones until the step that deletes them — no intermediate broken states.

Design context (motivation, end-state shape §1, layering invariants §1.5, result types §1.6, plugin integration §3, out-of-scope §4, risks §5) lives in [`../sandbox-api-runtime-refactor.md`](../sandbox-api-runtime-refactor.md). These step plans are not standalone — they assume that document.

The `step-XX` filename prefix is the implementation order. The slice ID remains in each title because the parent design still uses slice names to describe architectural dependencies.

## Implementation Order

| Step | Slice | Plan | Goal |
|---|---|---|---|
| 1 | 5a | [Overlay/OCC responsibility split](./step-01-slice-5a-overlay-occ-responsibility-split.md) | Overlay → pure upperdir capture; OCC → sole merge-policy decider (ledger gitinclude, direct-merge gitignore/external). In place. |
| 2 | 1 | [Provider seam](./step-02-slice-1-provider-seam.md) | Add `ProviderAdapter` Protocol + Daytona adapter; no caller changes. |
| 3 | 2 | [`raw_exec` primitive](./step-03-slice-2-raw-exec.md) | Public `sandbox.api.raw_exec` over the adapter; move host-side bundle upload to `runtime/bundle.py`; importer-allowlist test. |
| 4 | 3 | [Runtime scaffolding](./step-04-slice-3-runtime-scaffolding.md) | Replace `daemon/command.py` with `runtime/server.py`; add `setup_orchestrator.py`, pipeline stubs, and temporary legacy client compatibility. |
| 5 | 4 | [OCC peer relocation](./step-05-slice-4-occ-relocation.md) | `mutations/` → `sandbox/occ/`; add `client.py`, `setup.sh`, `edit_pipeline`, `write_pipeline`. |
| 6 | 5b | [Overlay peer relocation](./step-06-slice-5b-overlay-relocation.md) | `overlay/` → `sandbox/overlay/`; add `client.py`, `setup.sh`, `shell_pipeline`. |
| 7 | 6 | [Public verb API](./step-07-slice-6-public-api.md) | `sandbox.api.{shell,read,write,edit}`; §1.6 result hierarchy. |
| 8 | 7 | [Delete legacy](./step-08-slice-7-delete-legacy.md) | Remove `code_intelligence/`, old API modules, `SandboxTransport`. |
| 9 | 8 | [Tests + docs](./step-09-slice-8-tests-docs.md) | Relocate tests; add runtime/pipeline coverage; supersede prior docs. |

## Implementation Reports

- Step 1 / Slice 5a: [Overlay/OCC responsibility split report](./step-01-slice-5a-implementation-report.md)
- Step 2 / Slice 1: [Provider seam report](./step-02-slice-1-implementation-report.md)
- Step 3 / Slice 2: [`raw_exec` primitive report](./step-03-slice-2-implementation-report.md)
- Step 4 / Slice 3: [Runtime scaffolding report](./step-04-slice-3-implementation-report.md)
- Step 5 / Slice 4: [OCC peer relocation report](./step-05-slice-4-implementation-report.md)
- Step 6 / Slice 5b: [Overlay peer relocation report](./step-06-slice-5b-implementation-report.md)

## Ordering invariants

- Step 1 must land before the architecture chain starts and must remain independently revertible.
- Step 6 depends on both Step 1 and Step 5: overlay can move only after the seam is clean and OCC is at its peer location.
- Step 7 must land before Step 8. The public surface flips first; legacy deletes only after the new surface owns all callers.
- Each step keeps the old path alive until the step *after* the migration completes; no step both adds and deletes the same surface.
- Peer `client.py` modules are internal route points, not public APIs. Agent tools
  still import only `sandbox.api.{shell,read,write,edit}`.
- `runtime/server.py` is a generic OP_TABLE dispatcher. OCC/Overlay request
  behavior is registered by peer bootstrap/handler modules, not hardcoded in
  the server.
- The domain refactor has exactly two peer modules: `sandbox/occ/` and
  `sandbox/overlay/`. `sandbox/runtime/` is the shared daemon/server support
  layer, not a third peer module.
