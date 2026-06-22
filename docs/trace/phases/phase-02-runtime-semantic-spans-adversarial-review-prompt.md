# Phase 2 Runtime Semantic Spans: Multi-Agent Adversarial Review Prompt

Use this prompt to review the current Phase 2 runtime semantic spans
implementation in `ephemeral-os`.

## Role

You are the coordinator for a read-only adversarial review. Spawn three
independent review agents and synthesize their findings:

1. Correctness agent: prove whether the implementation preserves runtime
   behavior and emits the intended trace semantics at live boundaries.
2. Completeness agent: prove whether the implementation satisfies every Phase 2
   acceptance criterion, test requirement, and explicit guard.
3. Cleanness/privacy agent: prove whether the implementation stays clean,
   minimal, scoped, and free of sensitive telemetry leaks or forbidden
   infrastructure.

All agents must inspect the live checkout and current diff. Do not rely on docs,
checklists, or stated completion summaries without code evidence.

## Hard Rules

- Review only. Do not modify files.
- Findings first, ordered by severity.
- Every finding must cite exact file and line evidence.
- If evidence is weak, say so explicitly and classify it as a question or
  residual risk, not a finding.
- Prefer concrete call paths and tests over grep-only claims.
- Do not report a missing test if an existing test already proves the behavior.
- Do not suggest adding OTLP, metrics, dashboards, protocol metadata, gateway
  UX, runner context propagation, or custom runtime trace infrastructure.

## Required Context To Read

Start from repo root.

Read these docs:

- `docs/trace/phases/README.md`
- `docs/trace/phases/phase-02-runtime-semantic-spans.md`
- `docs/trace/README.md`, focusing on safe fields, span/event names, telemetry
  boundaries, and OCC telemetry stats.

Inspect the live diff:

```sh
git status --short
git diff -- crates/sandbox-runtime docs/trace Cargo.lock
git diff --name-only
```

Inspect these implementation areas:

- `crates/sandbox-runtime/operation/src/internal/workspace_session/service/impls/`
- `crates/sandbox-runtime/operation/src/internal/workspace_remount/service/impls/remount_workspace_session.rs`
- `crates/sandbox-runtime/operation/src/internal/layerstack/service/impls/publish_changes.rs`
- `crates/sandbox-runtime/operation/src/public/command/service/finalize.rs`
- `crates/sandbox-runtime/workspace/src/lifecycle/create.rs`
- `crates/sandbox-runtime/workspace/src/lifecycle/remount/result.rs`
- `crates/sandbox-runtime/workspace/src/namespace/cgroup_monitor.rs`
- `crates/sandbox-runtime/operation/src/public/cgroup_monitor/`
- all touched tests under `crates/sandbox-runtime/**/tests/`

## Phase 2 Scope To Enforce

Allowed runtime additions:

- Inline `tracing` spans/events in runtime crates only.
- `workspace.create_session`
- `workspace.destroy_session`
- `workspace.capture_changes`
- `workspace.remount`
- `workspace_create_phase_finished`
- remount allowlisted result facts from `RemountOverlayResult` and the live
  setns-runner boundary.
- `layerstack.publish_changes`
- publish route/result/rejection fields and OCC telemetry stats from
  operation-level wrappers.
- `cgroup_monitor.anomaly`
- `cgroup_monitor.final_summary`

Forbidden:

- `crates/sandbox-runtime-trace/`
- `crates/sandbox-runtime/operation/src/internal/telemetry.rs`
- `crates/sandbox-runtime/workspace/src/lifecycle/remount/report.rs`
- runtime subscriber/exporter setup
- OTLP implementation
- metrics or dashboards
- protocol request/response changes
- telemetry DTO/report objects
- `OccTraceEvent` or any replacement runtime trace object API
- public cgroup read-op tracing for `inspect_cgroup_monitor` or
  `read_cgroup_monitor_samples`
- spans named after private helpers such as `plan_publish`,
  `validate_source_paths`, or manifest commit internals

Sensitive fields must not appear in traces:

- raw host paths, workspace roots, layer paths, cgroup paths, upper/work dirs,
  transcript/artifact paths
- raw root hashes or path-derived IDs
- command text, stdin, stdout/stderr, env values, auth tokens, raw request args
- PIDs
- raw `Debug` structs, raw `Display` errors, raw response payloads
- `WorkspaceHandle`, `WorkspaceEntry`, `PublishChangesResult`, remount
  diagnostic JSON, cgroup sample DTOs

Safe fields include booleans, counts, statuses, bounded reasons, bounded error
classes, phase names, fingerprint kinds, root-hash match booleans, and explicit
`duration_ms`.

## Agent 1: Correctness Review

Focus on behavioral correctness and call-path alignment.

Check:

- Workspace spans wrap the actual session service operations and do not change
  create/capture/destroy semantics, lock behavior, rollback behavior, remount
  pending behavior, or cgroup finalization behavior.
- `workspace_create_phase_finished` is emitted from the existing
  `WorkspaceModeManager::initialize_handle` phase timings without changing
  `WorkspaceHandle` or setup behavior.
- Remount telemetry uses `RemountOverlayResult` behavior and preserves
  `failure_summary()` behavior.
- Layerstack publish still preserves invalid-base, route, no-op, rejection, and
  OCC behavior. Trace fields must not affect publish correctness.
- Cgroup events are emitted only at registry/session-final/command-final/
  cleanup/anomaly boundaries plus command finalization handoff.
- Periodic sampler behavior and public read behavior remain data-only APIs.
- Adding tracing dependencies did not introduce runtime subscriber/exporter
  ownership in runtime crates.

Useful commands:

```sh
rg -n "workspace\\.create_session|workspace\\.destroy_session|workspace\\.capture_changes|workspace\\.remount|layerstack\\.publish_changes|workspace_create_phase_finished|workspace_remount_overlay_result|cgroup_monitor\\.anomaly|cgroup_monitor\\.final_summary" crates/sandbox-runtime -g '*.rs'
rg -n "inspect_cgroup_monitor|read_cgroup_monitor_samples|tracing::|info_span!|#\\[instrument" crates/sandbox-runtime/operation/src/public/cgroup_monitor -g '*.rs'
```

## Agent 2: Completeness Review

Focus on acceptance criteria and test/guard coverage.

Check:

- All required span/event names exist and are tested.
- Workspace create/destroy/capture/remount call paths have positive tests.
- Workspace phase timing has a test proving existing `Instant` timings are
  emitted.
- Remount tests prove behavior preservation and allowlisted trace facts only.
- Publish/OCC tests prove structured route/result/rejection/OCC telemetry stats
  without custom trace objects.
- Cgroup tests prove healthy periodic samples emit no trace events and
  final/anomaly events are bounded.
- Negative tests prove public cgroup read ops are not span names or
  instrumentation boundaries.
- Privacy sentinel tests cover raw paths, root hashes, command I/O, env/auth,
  raw DTOs, response payloads, and raw error strings.
- Forbidden path/module and no-protocol-change guards are present or were run.

Required verification commands:

```sh
cargo fmt --check
cargo test -p sandbox-runtime
cargo test -p sandbox-runtime-workspace
cargo test -p sandbox-runtime-layerstack
cargo test -p sandbox-runtime-namespace-process
git diff --check -- docs/trace crates/sandbox-runtime
test ! -e crates/sandbox-runtime-trace
test ! -e crates/sandbox-runtime/operation/src/internal/telemetry.rs
test ! -e crates/sandbox-runtime/workspace/src/lifecycle/remount/report.rs
git diff --exit-code -- crates/sandbox-protocol
if rg -n "OccTraceEvent" docs/trace crates/sandbox-runtime; then exit 1; else exit 0; fi
if rg -n "info_span!|span!|event!|#\\[instrument|tracing::" crates/sandbox-runtime/operation/src/public/cgroup_monitor -g '*.rs'; then exit 1; else exit 0; fi
```

If you do not run a required command, say why and treat the missing execution as
residual risk.

## Agent 3: Cleanness And Privacy Review

Focus on scope control, maintainability, and safe telemetry shape.

Check:

- No new production trace helper module, telemetry DTO, report object,
  subscriber, exporter, metrics path, or dashboard code was introduced.
- Test-only trace capture helpers are confined to tests and do not become
  runtime infrastructure.
- Error fields use bounded classes, not raw `Display` strings.
- Span/event fields are explicit and do not auto-capture large structs.
- No raw path/root/hash/command/env/auth/error/DTO values can appear through
  `Debug`, `Display`, `?`, `%`, `field::debug`, or whole-object records.
- OCC is represented as telemetry stats/fields on the normal publish tracing
  path, not as `OccTraceEvent` or a replacement trace object API.
- New helper methods are small, local, and consistent with existing code style.
- Docs changed only as needed for Phase 2/OCC telemetry wording.
- Cargo changes are justified by actual runtime crate tracing usage.

Useful commands:

```sh
rg -n "Debug|Display|field::debug|\\?[^a-zA-Z]|%[^a-zA-Z]|WorkspaceHandle|WorkspaceEntry|PublishChangesResult|CgroupMonitorSample|RemountOverlayResult" crates/sandbox-runtime -g '*.rs'
rg -n "sandbox-runtime-trace|internal/telemetry|remount/report|subscriber|exporter|otlp|metrics|dashboard|OccTraceEvent" crates/sandbox-runtime docs/trace -g '*.rs' -g '*.md'
rg -n "PATH_SECRET|ROOT_HASH_SECRET|CONTENT_SECRET|RAW_.*SECRET|AUTH_ENV_SECRET|STDIN_SECRET|STDOUT_SECRET" crates/sandbox-runtime -g '*.rs'
```

For sentinel hits, distinguish test input strings from actual telemetry output
expectations.

## Coordinator Output Format

Return one synthesized review, not three separate reports.

Use this structure:

```md
## Findings

- [P0/P1/P2/P3] Title
  File: `path:line`
  Evidence: concrete code/test/call-path evidence.
  Impact: why this matters.
  Recommendation: smallest scoped fix.

## Open Questions Or Residual Risk

- Question/risk with exact evidence and what would resolve it.

## Agent Coverage

- Correctness: what was inspected and whether any commands were run.
- Completeness: what was inspected and whether required verification ran.
- Cleanness/privacy: what was inspected and any scope/privacy concerns.

## No-Finding Confirmation

If no findings exist, say:
"No correctness, completeness, or cleanness findings were found in the reviewed
scope." Then list any commands not run as residual risk.
```

Severity guide:

- P0: breaks protocol/runtime correctness, leaks secrets, or violates hard
  forbidden infrastructure constraints.
- P1: likely runtime behavior regression, missing required telemetry boundary,
  public cgroup read-op tracing, raw sensitive telemetry field, or missing guard
  that could hide a serious violation.
- P2: incomplete acceptance criterion, weak test coverage for a risky boundary,
  bounded-field mistake, or maintainability issue with meaningful future risk.
- P3: minor cleanup, wording, duplication, or local test ergonomics issue.
