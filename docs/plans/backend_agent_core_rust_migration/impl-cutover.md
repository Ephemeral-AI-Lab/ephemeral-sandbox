# impl-cutover — Phase-7 compatibility boundary and retirement gates

> Phase-7 cutover plan. Conforms to ./spec-conventions.md.
> No new Rust crate: this spec owns the Python/Rust compatibility protocol,
> rollout switch, rollback, parity comparator, and retirement order.

## 1. Purpose & Responsibility

Phase 7 runs the Rust `agent-core` control plane beside the Python control plane
without introducing unsafe FFI or a second orchestration model. The compatibility
boundary is a **subprocess JSON-RPC adapter**: Python launches the Rust runtime
binary, exchanges newline-delimited JSON requests/responses, and keeps the old
Python path available behind a config/env switch. This doc does not define new
domain state; it defines how already-implemented crates are selected, compared,
rolled back, and retired.

## 2. Boundary Choice

- **Adapter:** subprocess JSON-RPC over stdio. Avoid PyO3/FFI so agent-core can
  keep `#![forbid(unsafe_code)]` and avoid Python ABI coupling.
- **Switch:** `EOS_AGENT_CORE_RUNTIME=python|rust|compare`, default `python`
  until all Phase-7 gates pass.
- **Compare mode:** run Python as source of truth, run Rust against cloned
  fixtures/state where side effects are isolated, then emit a structured parity
  report. Compare mode never publishes Rust mutations to the live Python run.
- **Rollback:** set `EOS_AGENT_CORE_RUNTIME=python` and restart the request entry
  service. No schema downgrade is required because Rust uses the same SQLite
  compatibility contract until package retirement.

## 3. Protocol

Messages are JSON objects with `version`, `kind`, `request_id`, and `payload`.
Supported request kinds:

- `start_request`: `{cwd, prompt, sandbox_id?, config_path?}`.
- `resume_handle`: `{request_id}` for a still-running Rust handle.
- `cancel_request`: `{request_id, reason}`.
- `health`: `{}`.

Responses are `{status: "ok"|"error", request_id?, payload, error?}`. Errors
include stable `kind`, lowercase `message`, and optional `details`. The adapter
never streams provider/tool internals directly; prompt reports, audit JSONL, DB
rows, and sandbox artifacts remain the comparison artifacts.

## 4. DB And Sandbox Invariants

- SQLite schema compatibility is preserved until the Python package owning that
  table is retired. Rust migrations may add forward-compatible columns only when
  Python ignores them.
- Rust and Python both use the existing sandbox daemon protocol. Rust
  `agent-core` uses Docker as the only sandbox provider; non-Docker Python
  provider paths are outside this Rust migration and are not parity targets.
- Compare mode uses isolated DB/sandbox fixtures or copied state. It must not
  publish OCC writes, terminal submissions, or workflow closure to the live run.

## 5. Verification Gates

- **AC-cutover-01:** `python` mode produces the same behavior as the current
  Python entry path. Test: adapter smoke with a mocked root request.
- **AC-cutover-02:** `rust` mode completes a mocked root request and delegated
  workflow using the same DB/sandbox fixtures as the Python run.
- **AC-cutover-03:** `compare` mode emits a parity report for root request,
  delegated workflow, sandbox tool, provider SSE, audit JSONL, prompt report, and
  SQLite row projection without mutating live state.
- **AC-cutover-04:** rollback is one config/env change plus restart; a failing
  Rust subprocess returns control to Python without corrupting request/task rows.
- **AC-cutover-05:** package retirement is blocked unless the matching parity
  report is green and the architecture page for that package has updated
  `data-last-reviewed-commit` and `data-evidence-paths`.
- **AC-cutover-06:** E2E latency comparison artifact is produced for root and
  delegated workflows before retiring Python runtime/engine/workflow packages.

## 6. Retirement Order

Retire Python packages only in dependency order: leaf DTO/provider/config pieces
first, then tools, then engine/workflow, then runtime entry. `test_runner` stays
out of this migration and is rebuilt separately after control-plane parity.

---
**On completion:** update the Progress Tracker in `./overview.md` row `cutover`
per spec-conventions.md §13. Do not edit crate rows.
