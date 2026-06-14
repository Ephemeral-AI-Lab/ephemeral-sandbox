# Sandbox docs index

| Document | What it is |
|---|---|
| `SPEC.md` | The target architecture spec the sandbox system implements (components, wire protocol, op catalog, lifecycle, recovery, conformance). |
| `sandbox-architecture-7-to-9_SPEC.md` | Draft remediation spec for moving the live sandbox architecture from the review baseline toward a 9/10 target. |
| `API.md` | The public op reference. **Generated** from `../crates/daemon/operation/ops.json` via `cargo run -p xtask -- gen-docs`; `check-contract` fails when stale. |
| `contract/01-wire-protocol.md` | FROZEN: the full daemon wire contract (framing, wire messages, auth, limits, error catalog) with source citations. |
| `contract/02-cas-byte-identity.md` | FROZEN: the two CAS content hashes, byte-for-byte, plus the 18 golden cases' law. |
| `contract/03-audit-and-metrics.md` | FROZEN: audit ring buffer + isolated-workspace JSONL channels and `layer_metrics`. Both audit channels were removed from the Rust runtime on 2026-06-11; only `layer_metrics` remains live. |
| `contract/04-shared-models.md` | FROZEN: request/response data-type contract for the verb surface. |
| `contract/06-crate-map-and-invariants.md` | FROZEN: historical crate map and invariants from the migration. |
| `sandbox-event-tracing-response-plan.md` | HISTORICAL: original tracing/response proposal, superseded by the live host trace store and operator trace ops. |
| `sandbox-crates-code-review.md` | HISTORICAL: June 13 review snapshot, useful as prior findings but not current implementation truth. |
| `sandbox-crates-refactor-plan.md` | HISTORICAL: remediation task plan for the June 13 review snapshot. |
| `../improvement.spec.md` | HISTORICAL: earlier 8.5/10 improvement plan, superseded by `sandbox-architecture-7-to-9_SPEC.md`. |

The live, binding artifacts between the host and box sides are
`../crates/daemon/operation/ops.json`,
`../crates/shared/protocol/PROTOCOL.md`, and owner-local fixtures under
`../crates/shared/protocol/fixtures/`, `../crates/daemon/layerstack/tests/fixtures/`,
and `../crates/daemon/operation/fixtures/`. The `docs/contract/` files above
are the frozen historical contracts they were distilled from; they do not
change.

The migration/review notes `SPEC-operation-*.md`, `command-naming-update-note.md`,
`sandbox-bridge-*.md`, `sandbox-event-tracing-response-plan.md`,
`sandbox-crates-*.md`, `../improvement.spec.md`, and
`sandbox-response-observability-findings.md` are historical context. They may
mention retired crate names or Python-era paths.
Current architecture work should cite `SPEC.md`, `API.md`, `ops.json`,
`crates/shared/protocol/PROTOCOL.md`, or
`sandbox-architecture-7-to-9_SPEC.md` instead.
