export const meta = {
  name: 'sandbox-trace-spec-review',
  description: 'Review sandbox event-tracing/response-contract implementation against its SPEC; find bugs, spec violations, dead/legacy code; adversarially verify each finding',
  phases: [
    { title: 'Review', detail: 'one reviewer per SPEC dimension produces structured findings' },
    { title: 'Verify', detail: 'one skeptic per finding reads the code and confirms or refutes' },
  ],
}

const REPO = '/Users/yifanxu/machine_learning/LoVC/EphemeralOS'
const SPEC = REPO + '/docs/plans/sandbox-event-tracing-and-response-contract_SPEC.md'
const SBX = REPO + '/sandbox'

const COMMON = [
  'You are reviewing the IMPLEMENTATION of a Rust sandbox event-tracing + response-contract system against its SPEC.',
  'SPEC file: ' + SPEC,
  'Implementation root: ' + SBX + ' (crates under ' + SBX + '/crates).',
  '',
  'CONTEXT: The SPEC progress tracker claims Phases 01-06 are COMPLETE under a DESTRUCTIVE, CLEAN-SLATE posture:',
  '"no technical debt is the explicit goal." Legacy surfaces (OpResponse, merge_runner_timings, V1FlatteningAdapter,',
  'is_success/error_kind, flat dotted-key timings maps, json! response envelopes, quirk serializers like',
  "'error: Option<()>' serialized as null and 'mutation_source: None' spliced post-hoc) are supposed to be GONE.",
  'DO NOT trust the progress notes -- verify against the actual code. The workspace currently compiles (cargo check passed).',
  '',
  'Your job: find REAL problems, not restate the SPEC. Specifically look for:',
  '- BUGS: logic errors, incorrect hashing/chaining, wrong field derivation, fail-open where spec says fail-closed,',
  '  off-by-one seq, races, panics on the hot path, swallowed errors, incorrect truncation.',
  '- SPEC-VIOLATIONS: behavior or contract shape that contradicts a normative SPEC rule.',
  '- DEAD-CODE / LEGACY: code the destructive posture said to delete that still exists (live or dead), unused pub items,',
  '  functions/modules/imports no longer reachable, flat timings leaking to the wire envelope.',
  '- MISSING: a normatively-required event/field/span/query/marker that is not emitted/implemented.',
  '',
  'Read the relevant SPEC sections AND the actual code (grep + read). Anchor every finding to a real file:line you verified.',
  'Be rigorous and skeptical: only report things you actually confirmed by reading code. Prefer fewer high-confidence',
  'findings over speculation. For dead-code claims, actually check call sites (rg the symbol across crates) before claiming unused.',
  '',
  'Return findings via the structured schema. Each finding MUST include a precise file path, line range, the SPEC anchor',
  '(section name or line range), concrete evidence (what the code does), and a concrete suggested fix. Set confidence honestly.',
].join('\n')

const FINDINGS_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['dimension', 'findings', 'coverage_notes'],
  properties: {
    dimension: { type: 'string' },
    coverage_notes: { type: 'string', description: 'what you reviewed, what you could not cover, and why' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['id', 'title', 'severity', 'category', 'file', 'lines', 'spec_anchor', 'evidence', 'suggested_fix', 'confidence'],
        properties: {
          id: { type: 'string', description: 'short stable id, e.g. trace-1' },
          title: { type: 'string' },
          severity: { type: 'string', enum: ['high', 'medium', 'low'] },
          category: { type: 'string', enum: ['bug', 'spec-violation', 'dead-code', 'legacy', 'missing', 'quality'] },
          file: { type: 'string', description: 'absolute or repo-relative path' },
          lines: { type: 'string', description: 'line range like 102-144 or single line' },
          spec_anchor: { type: 'string' },
          evidence: { type: 'string', description: 'what the code actually does that makes this a problem' },
          suggested_fix: { type: 'string' },
          confidence: { type: 'string', enum: ['high', 'medium', 'low'] },
        },
      },
    },
  },
}

const VERDICT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['verdict', 'reasoning', 'final_severity', 'fix_guidance'],
  properties: {
    verdict: { type: 'string', enum: ['confirmed', 'refuted', 'partial'], description: 'confirmed = real and as described; refuted = not a real problem; partial = real but mis-scoped/mis-severity' },
    reasoning: { type: 'string', description: 'what you read to verify, including file:line you checked' },
    final_severity: { type: 'string', enum: ['high', 'medium', 'low', 'none'] },
    fix_guidance: { type: 'string', description: 'concrete, minimal fix or "do not fix" with reason' },
  },
}

const DIMENSIONS = [
  {
    key: 'trace-crate',
    prompt: COMMON + '\n' + [
      '',
      'DIMENSION: eos-trace crate contract (SPEC Phase 01).',
      'Read SPEC sections: "New crate sandbox/crates/eos-trace" (~lines 499-540), "Detail-capture principle" + ResourceStats payload contract (~676-739),',
      '"Resource event wrapping" (~1559-1585), "Extension Model / Vocabulary governance" (~2035-2086), Phase 01 checklist (~2105-2131).',
      'Review files: ' + SBX + '/crates/eos-trace/src/*.rs and ' + SBX + '/crates/eos-trace/proto/eos/trace/v1/trace.proto and any tests.',
      'Verify: (1) record.rs defines TraceId/RequestId/SpanUid/TraceRecord/SpanRecord/EventRecord/TraceResource/TraceLink/WorkspaceRoute(4 values)/',
      'TraceKind(OpRequest|CommandFinalize|ActiveCommandAdvance|IdleWorkspaceEvict|PluginService) and a CLOSED SpanKind enum with an EXHAUSTIVE',
      'subsystem() mapping (Wire|Dispatch|Op|LayerStack|Overlay|Command|Workspace|Plugin|Control). (2) bounded-detail helpers produce',
      '{truncated, sha256, original_len} on overflow, never silent drop; budgets match the capture-budget table. (3) spool.rs is bounded',
      '(default 4 MiB), drop-oldest, increments dropped_traces, non-blocking try_push. (4) proto defines TraceBatch, TraceSpan, TraceEvent,',
      'TraceResource, TraceLink, RequestStart, SandboxStatusSnapshot, ResponseTraceRef, AuditEntry; fields additive/optional (schema evolution rule C).',
      '(5) codec round-trips DTO<->protobuf; golden compatibility tests exist and old fixtures still decode. (6) resource_stats.rs matches the',
      'ResourceStats payload contract (meta/cgroup.cpu/memory/io/pressure/process/tree/mount_cost) with source_available/read_error markers and sampler_duration_us.',
      'Flag mismatches, missing messages/fields, non-exhaustive or non-closed enums, silent truncation, and any unused items.',
    ].join('\n'),
  },
  {
    key: 'envelope-contract',
    prompt: COMMON + '\n' + [
      '',
      'DIMENSION: eos-operation response/envelope contract (SPEC Part B + Phase 01/05/06).',
      'Read SPEC: "Part B -- Response Contract" (~1910-2033), Posture deletions table (~286-300), Populate table error.details/warnings (~794-799),',
      'Drop/dedupe table (~778-790), Phase 01 checklist (~2117-2123), Phase 06 checklist (~2362-2386).',
      'Review files: ' + SBX + '/crates/eos-operation/src/core/*.rs (envelope.rs, fault.rs, response.rs, catalog.rs) and per-family contract.rs files under',
      SBX + '/crates/eos-operation/src/*/contract.rs, plus grep the whole repo.',
      'Verify: (1) OperationEnvelope<T> is a serde tag="status" union with EXACTLY 6 arms: ok, running, cancelled, timed_out, rejected, error;',
      'result XOR error per arm (rejected may carry optional partial result); no extra/missing arms. (2) ResponseMeta carries protocol_version,',
      'op, request_id, trace(TraceRef), caller_id?, workspace_route, duration_ms, modules_touched, steps, resource_summary, warnings -- and meta is',
      'DERIVED from the span tree, not hand-inserted timings. (3) OperationFault.details is structured (source_chain[], io_kind, path, exit codes),',
      'serialized as {} never null; error_id only for internal errors. (4) per-family result DTOs exist and match the family result-shape table (~1971-1981).',
      '(5) DELETED: OpResponse, V1FlatteningAdapter/from_legacy_value/to_legacy_value, is_success/error_kind classifiers, quirk serializers',
      '(error: Option<()> -> null, mutation_source None -> ""). (6) NO raw serde_json::Value as the public response contract.',
      'Confirm via grep whether these deleted items truly are gone everywhere (not just renamed). Flag any survivor, any null-pair serialization,',
      'any Value-typed public contract, any missing family DTO.',
    ].join('\n'),
  },
  {
    key: 'host-store',
    prompt: COMMON + '\n' + [
      '',
      'DIMENSION: eos-sandbox-host trace store + fail-closed persistence (SPEC Phase 02; audit rules B & D).',
      'Read SPEC: "Host persistence" DDL + write sequencing + acceptance queries (~1381-1754), Non-negotiable audit rules B/D (~342-351),',
      'Phase 02 checklist (~2140-2168).',
      'Review files: ' + SBX + '/crates/eos-sandbox-host/src/trace_store.rs and trace_store/ submodules (ddl, writer, ingest, query, seal), host.rs, runtime.rs, trace_recorder.rs, and tests.',
      'Verify: (1) DDL matches the schema (audit_entries append-only with the listed columns; audit_segment_seals; trace_requests/spans/events/resources/links;',
      'sandbox_heartbeats; the listed indexes). (2) entry_sha256 = sha256(canonical_header || payload_sha256 || prev_global_sha256 || prev_sandbox_sha256) --',
      'verify the actual hash input composition in writer code; both a global chain and a per-sandbox chain exist. (3) audit_entries is append-only;',
      'projections rebuildable from audit_entries; segment seals are signed (ed25519) with key_id and verifiable; tamper detection works.',
      '(4) FAIL-CLOSED: a RequestStart audit entry + trace_requests row are inserted AND DURABLY COMMITTED before forwarding a MUTATING op; if the append',
      'fails the mutating op is NOT forwarded; read-only ops proceed with a trace_degraded marker. Mutability comes from catalog OpContract.mutates_state;',
      'dynamic plugin.* ops default to MUTATING (fail-closed). Verify this is actually how forward() decides. (5) startup appends host_boot, reconciles',
      "prior status IS NULL rows to 'uncertain' (append uncertain_outcome loss before update), refuses to open a NEWER user_version. (6) prune writes a",
      'hash-chained tombstone BEFORE deleting and only operates on whole SEALED segments; never deletes unsealed entries. (7) connection pragmas',
      'journal_mode=WAL, synchronous=FULL, foreign_keys=ON. (8) acceptance queries (1)-(8) implemented and hit an index (EXPLAIN QUERY PLAN SEARCH not SCAN).',
      'Flag any fail-open path, wrong hash input, mutable history, missing reconciliation, wrong prune ordering, missing seal verification, or query that scans.',
    ].join('\n'),
  },
  {
    key: 'transport',
    prompt: COMMON + '\n' + [
      '',
      'DIMENSION: gateway/host/daemon transport propagation + sidecar/spool/export (SPEC Phase 03; audit rule A).',
      'Read SPEC: "Transport connection lifecycle" tables (~463-498), "Span taxonomy" (~850-864), "Context propagation rules" (~866-908),',
      '"Hot-path ingestion contract" (~910-934), "Host / Container Boundary" (~936-963), "Transport" implementation shape (~965-1075),',
      'phase-event vocabulary host.protocol/gateway/host.transport/daemon.transport (~747-752), Phase 03 checklist (~2179-2225).',
      'Review files: ' + SBX + '/crates/eos-sandbox-gateway/src/gateway.rs (+trace.rs), ' + SBX + '/crates/eos-sandbox-host/src/host.rs + protocol.rs + runtime.rs,',
      SBX + '/crates/eos-daemon/src/transport/server.rs, ' + SBX + '/crates/eos-daemon/src/trace.rs, dispatch/dispatcher.rs, and their tests.',
      'Verify: (1) all transport events in the lifecycle tables are emitted with their required fields (gateway accepted/request_read/parse_failed/',
      'response_written/write_failed + route_selected/rejected/engine_forward_*; host connect/retry/fallback/exec_client/daemon_respawn/request_written/',
      'response_read/decode_failed/empty_response; daemon accepted/read/auth_checked/decoded/response_write_*/shutdown_finished). (2) SECURITY: auth tokens are',
      'NEVER recorded/hashed/length-recorded -- only auth_token_present/auth_required/auth_ok booleans. Grep for any place a token value or its length is captured.',
      '(3) the daemon root op_request span opens BEFORE read_request_line so wire failures (bad JSON/too-large/timeout/auth) still close a trace; a',
      'registry-aborted request yields error_kind="cancelled". (4) request-scoped records ride the response as a protobuf _trace_events sidecar; background',
      'roots use a bounded non-blocking spool drained by sandbox.trace.export; the host drainer is single-flight per sandbox and never runs on the',
      'request-forwarding caller thread. (5) crash-log fmt installed before listener bind; config_loaded + listen_bound boot events. (6) AUDIT RULE A:',
      'no daemon crate depends on rusqlite / host-store modules; no SQLite/fsync/host-RPC on dispatch/route/subsystem decision paths; meta derived from the',
      'closed request span. (7) host-only transport failures still write a trace outcome with no daemon sidecar. Flag missing events/fields, token leakage,',
      'root span opened too late, blocking on the hot path, drainer on caller thread, or sidecar not stripped of secrets.',
    ].join('\n'),
  },
  {
    key: 'subsystem-events',
    prompt: COMMON + '\n' + [
      '',
      'DIMENSION: subsystem phase events + resource stats (SPEC Phase 04).',
      'Read SPEC: phase-event vocabulary tables (~741-762), "Inventory-verified deltas / Add" table (~800-831), span taxonomy (~850-864),',
      'context propagation rules 2-4 (OCC worker / background / PPC reader) (~880-908), ResourceStats placement + payload (~699-739), Phase 04 checklist (~2236-2277).',
      'Review files across mechanism crates: ' + SBX + '/crates/eos-layerstack/src (commit/worker.rs, service.rs), ' + SBX + '/crates/eos-workspace/src',
      '(shared/capture.rs, isolated_workspace/manager/lifecycle.rs), ' + SBX + '/crates/eos-command/src, ' + SBX + '/crates/eos-operation/src/command + plugin,',
      SBX + '/crates/eos-overlay/src, and the daemon op_adapters (' + SBX + '/crates/eos-daemon/src/op_adapter/*.rs).',
      'Verify each REQUIRED event in the vocabulary tables is actually emitted with its required fields:',
      '- layer_stack: manifest_validated{manifest_version,manifest_depth,active_lease_count}, publish_layer_finished{version before/after, published_layer_count},',
      '  auto_squash_started/finished/skipped{reason,error?}, lease_release_failed, snapshot_acquired.',
      '- occ: commit_started, validate_groups_finished, worker_handoff, worker_batch_finished, conflict_detected{path,reason,observed_version?,observed_state?} PER FILE, commit_finished.',
      '- overlay: mount_started/finished{layer_count,fsconfig_calls,duration_us,upperdir_empty_bytes}, capture_started/finished{failing_path on error,bytes,file_count,dir_count,entry_count,truncated}, unmount_finished.',
      '- command: prepared, spawned, stdin_written{bytes,wait_ms,waited_for_output}, progress_read, cancelled, timed_out, exit_taken{kill_reason,signal}, finalized,',
      '  artifact_written/failed, final_persisted, final_persist_failed, transcript_failed, completion_buffer_evicted{command_id,seq,max_entries}, changed_paths_recorded, response_meta.',
      '  Plus ActiveCommand gains origin trace_id/request_id; background CommandFinalize roots exist.',
      '- isolated_workspace: enter/holder/network_configured{dns_fallback_applied,previous_first_nameserver?}/status_read/exit/teardown_phase_finished x4',
      '  (kill_holder{holder_was_alive,exit_status,signal?})/exited{mountinfo_scan_error?}/recovery_started/finished{manager_json_error?,orphan_cleanup_error?}.',
      '- plugin: setup_finished{exit_code?,output_tail?,spawn_error?}, service_started{stderr_path}, service_exited{exit_code?,signal?,status_raw?},',
      '  service_health_checked{state,restart_count,refresh_count,last_error}, ppc_reply_orphaned{message_id,direction,reason}, overlay_started/finished, callback_request/response.',
      '  Plus typed parent_message_id: Option<String> on PpcMessage (body re-parse deleted). PendingCalls::register captures Span::current().',
      '- checkpoint: worktree_mode_selected{mode}, git_command_finished{argv_summary,exit_code,stderr_tail}.',
      'RESOURCE STATS: before/after resource_stats pairs ONLY around command.process.wait and plugin.overlay.run (cheap kernel gauges); tree walks only on',
      'spans that already paid for a walk (capture/teardown), never always-on pairs; every paired sample records inflight_requests; per-source error markers',
      '(never silent absence); real tree-walk truncation with a named entry budget (50,000), NOT a hardcoded truncated=0.',
      'Flag any required event not emitted, missing required fields, fake/hardcoded truncation, tree walks in always-on pairs, or fabricated zero stats.',
    ].join('\n'),
  },
  {
    key: 'legacy-debt',
    prompt: COMMON + '\n' + [
      '',
      'DIMENSION: surviving legacy/debt in the daemon response path (SPEC Posture + Drop/dedupe + Phase 06). THIS IS THE HIGHEST-PRIORITY DIMENSION.',
      'Read SPEC: "Posture: Destructive, Clean-Slate" deletions table (~270-300), "Inventory-verified deltas: Drop/dedupe" (~778-790) and "Add"',
      'manifest_path_count row (~808), Phase 06 Debt Deletion checklist + greps (~2362-2386), Part B migration mechanics (~2027-2033).',
      'Review files: ' + SBX + '/crates/eos-daemon/src/runtime/response.rs, ' + SBX + '/crates/eos-daemon/src/op_adapter/plugin.rs, op_adapter/checkpoint.rs,',
      'op_adapter/files.rs, op_adapter/command.rs, dispatch/dispatcher.rs, dispatch/builtin.rs, and ' + SBX + '/crates/eos-operation/src/command (settle/contract),',
      'and grep the repo broadly.',
      'SEED LEADS (verify and expand, do not assume):',
      '- runtime/response.rs still defines and EXPORTS a flat dotted-key timings/resource map builder: resource_timings(), copy_runner_timings()',
      '  (a runner-timings merge helper), insert_cgroup_process_resource_timings(), insert_tree_resource_timings(), TreeResourceStats, and',
      '  plugin_overlay_changeset_response() which builds PluginOverlayMutationResponse with quirk serializers error: Option<()> (serializes to null) and a',
      '  post-hoc response[mutation_source]=json!(plugin_overlay) splice. The destructive posture said flat timings maps and quirk serializers are DELETED.',
      '- resource_timings() emits resource.layer_stack.manifest_path_count from manifest.layers.len() -- SPEC Add table (~808) explicitly calls',
      '  manifest_path_count a response-visible wrong stat to be replaced by a typed resource summary with a golden test.',
      'KEY QUESTIONS TO ANSWER WITH EVIDENCE:',
      '(1) Are these flat timings/resource maps LEAKING TO THE WIRE ENVELOPE, or built only to feed trace/resource events then stripped before',
      '   the OperationEnvelope is rendered? Trace the exact data flow in op_adapter/plugin.rs around lines 280-310 and 95-145: does the value returned to the',
      '   dispatcher / wrapped in the envelope still contain a timings map or resource.* keys or mutation_source or a null error? Read the envelope wrap site.',
      '(2) Is plugin_overlay_changeset_response producing the actual wire body, and does MutationCore serialize a timings field to the wire?',
      '(3) Is copy_runner_timings the renamed/surviving merge_runner_timings behavior the posture said to delete? Is it still needed?',
      '(4) Are checkpoint.rs / files.rs still threading flat timings to the wire, or internal-only?',
      '(5) builtin.rs error_envelope and gateway json! sites: legitimate fault/trace-detail payloads, or surviving ad-hoc success:bool/json! RESPONSE envelopes?',
      'For each, decide: DELETE (dead or posture-forbidden), KEEP (legitimately internal trace input), or FIX (still leaking to wire). Provide the exact data-flow',
      'evidence (file:line of build site -> file:line of wire/envelope site) that justifies the disposition. This dimension drives the cleanup, so be exhaustive and precise.',
    ].join('\n'),
  },
  {
    key: 'deadcode-vocab',
    prompt: COMMON + '\n' + [
      '',
      'DIMENSION: dead code sweep + vocabulary governance drift (SPEC Extension Model / Vocabulary governance; Phase 06).',
      'Read SPEC: "Vocabulary governance" (~2063-2086), phase-event vocabulary tables (~741-762), workspace route taxonomy (~655-674),',
      '"Span taxonomy" (~850-864), Phase 06 (~2362-2386).',
      'TASKS:',
      '(1) DEAD CODE: across the trace-touching crates (eos-trace, eos-operation, eos-daemon, eos-sandbox-host, eos-sandbox-gateway, and mechanism crates),',
      '   find unused pub items, unreferenced functions/structs/enums/modules, unused imports, and #[allow(dead_code)] suppressions that hide real dead code.',
      '   For each candidate, GREP the symbol across all crates to confirm zero non-definition, non-test references before reporting. Pay attention to leftover',
      '   modules from the abandoned tracing-subscriber Layer design (SPEC says Layer/subscriber modules were deleted -- confirm none survive).',
      '(2) VOCAB DRIFT: every event name emitted in code and every SpanKind/WorkspaceRoute/TraceKind/Subsystem value must have a corresponding row in the SPEC',
      '   registry tables. Find any emitted name with NO SPEC row (review defect per governance rule 1) and any SPEC-required name never emitted. Also confirm',
      '   closed sets (status 6 arms, WorkspaceRoute 4 values, Subsystem, SpanKind, TraceKind) are truly closed enums with exhaustive matching, not stringly-typed.',
      "(3) Confirm no stray TODO/FIXME/'temporary'/'migration' markers remain indicating unfinished destructive-posture cleanup in the trace/response surfaces.",
      'Flag concrete dead items (with proof of zero refs) and concrete vocab drift (emitted-but-unregistered or registered-but-unemitted names).',
    ].join('\n'),
  },
]

phase('Review')
const results = await pipeline(
  DIMENSIONS,
  (d) => agent(d.prompt, { label: 'review:' + d.key, phase: 'Review', schema: FINDINGS_SCHEMA }),
  (review, d) => {
    if (!review || !review.findings || review.findings.length === 0) {
      return { dimension: d.key, coverage_notes: (review && review.coverage_notes) || 'no review', findings: [] }
    }
    return parallel(
      review.findings.map((f) => () =>
        agent(
          COMMON + '\n' + [
            '',
            'You are an adversarial VERIFIER. A reviewer filed this finding about the sandbox trace/response implementation. Independently',
            'confirm or REFUTE it by reading the actual code at the cited location and its surroundings. Default to skepticism: if the cited',
            'file:line does not actually show the claimed problem, or the claim misreads the data flow, mark it refuted. If real but mis-scoped',
            'or wrong severity, mark partial and correct it. Verify dead-code claims by grepping for references yourself.',
            '',
            'FINDING (dimension ' + d.key + '):',
            '- id: ' + f.id,
            '- title: ' + f.title,
            '- category: ' + f.category + '  severity: ' + f.severity + '  confidence: ' + f.confidence,
            '- file: ' + f.file + '  lines: ' + f.lines,
            '- spec_anchor: ' + f.spec_anchor,
            '- evidence: ' + f.evidence,
            '- suggested_fix: ' + f.suggested_fix,
            '',
            'Read the file/lines (and SPEC anchor) and return your verdict. In reasoning, cite the exact file:line you inspected and what you saw.',
          ].join('\n'),
          { label: 'verify:' + d.key + ':' + f.id, phase: 'Verify', schema: VERDICT_SCHEMA },
        ).then((v) => ({ dimension: d.key, finding: f, verdict: v }))
      )
    )
  },
)

const flat = results.flat().filter(Boolean)
const confirmed = flat.filter((r) => r.verdict && (r.verdict.verdict === 'confirmed' || r.verdict.verdict === 'partial'))
const refuted = flat.filter((r) => r.verdict && r.verdict.verdict === 'refuted')

const rank = { high: 0, medium: 1, low: 2, none: 3 }
confirmed.sort((a, b) => (rank[a.verdict.final_severity] ?? 9) - (rank[b.verdict.final_severity] ?? 9))

return {
  summary: {
    dimensions: DIMENSIONS.length,
    total_findings: flat.length,
    confirmed: confirmed.length,
    refuted: refuted.length,
  },
  confirmed: confirmed.map((r) => ({
    dimension: r.dimension,
    id: r.finding.id,
    title: r.finding.title,
    category: r.finding.category,
    final_severity: r.verdict.final_severity,
    verdict: r.verdict.verdict,
    file: r.finding.file,
    lines: r.finding.lines,
    spec_anchor: r.finding.spec_anchor,
    evidence: r.finding.evidence,
    fix_guidance: r.verdict.fix_guidance,
    verify_reasoning: r.verdict.reasoning,
  })),
  refuted: refuted.map((r) => ({ dimension: r.dimension, id: r.finding.id, title: r.finding.title, why: r.verdict.reasoning })),
}
