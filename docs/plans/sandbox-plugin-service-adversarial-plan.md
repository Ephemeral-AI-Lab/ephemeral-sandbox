# Sandbox Plugin Service Adversarial Implementation Plan

**Status:** In progress; contract/status/routing/PPC lifecycle, retained
snapshot-refresh freshness gating, `restart_service` fallback, self-managed OCC
callbacks including repeated callback frames before a final reply, one-shot
WRITE_ALLOWED overlay execution, and live generic Rust
plugin coverage landed. Long-lived services now start in a daemon-owned private
overlay namespace on Linux and stale remount strategies now perform daemon-owned
in-place namespace remount with per-service refresh singleflight; a generic
package-adapter harness and the reusable bundled Python PPC service bridge are
live, while
Pyright read-only `documentSymbol`/`workspace/symbol`/`completion`/
`completionItem/resolve`/`publishDiagnostics`/`codeAction`/`signatureHelp`/
`hover`/`typeDefinition`/`declaration`/`callHierarchy` incoming/outgoing/
`documentHighlight`/`prepareRename`/`definition`/`references` refresh,
Pyright-computed self-managed rename publish, and generic LSP
`apply_workspace_edit`/`apply_code_action`/`format_document`/`execute_command`
publish paths are
live. Pyright `document_formatting` and `workspace/executeCommand` are now
routed through PPC as structured unsupported responses based on the server's
advertised capability boundary, while a generic positive LSP formatting
and execute-command provider path is covered separately through the daemon OCC
callback. Live status health
probes, failed-health isolation plus same-service failed-health recovery,
service-crash fail-closed plus same-service crash recovery, hung-service
timeout fail-closed probes, timeout next-dispatch recovery, and next-dispatch
service recovery after a PPC/process failure are
also green. The isolated-workspace plugin-family gate is now live-gated:
`api.plugin.status` and manifest-declared `plugin.*` calls fail with
`forbidden_in_isolated_workspace` while the same agent has an active isolated
workspace. The latest live
artifact records two read-only services co-observing the same refreshed
manifest without restart, non-serialized same-service concurrent read-only
dispatch with message-id routed out-of-order replies on the connected PPC
client, daemon-coalesced stale-service remounts before requests enter the
multiplexed stream, reusable PPC-bridge concurrent read-only calls where a fast
second request replies before a slow first request on the same service connection,
concurrent mounted-workspace write callbacks with distinct parent operation
ids, mixed `api.v1.shell` overlay/OCC publish followed by long-lived plugin
refresh/readback, explicit daemon cleanup that removes PPC routes/services and
reaps plugin harness processes, and the current
Pyright negative capability boundary:
`document_formatting=false`, `document_range_formatting=false`, and
`executeCommandProvider.commands=[]`, so Pyright formatting/execute-command are
unsupported-route gates for this Pyright harness, not positive provider parity
targets themselves.
Broader AV-10 LSP parity and the broader crash-recovery matrix beyond the
covered health/crash/timeout/recovery paths remain open.
**Date:** 2026-06-02.
**Scope:** `/sandbox` Rust plugin implementation, with the Python sandbox plugin
path as the behavioral reference.

## Source Anchors

- Python plugin reference:
  `backend/src/sandbox/ephemeral_workspace/plugin/op_registry.py`,
  `overlay_dispatch.py`, `overlay_child.py`, `runtime_api.py`, `projection.py`.
- Python LSP reference:
  `backend/src/plugins/catalog/lsp/runtime/session_manager.py`,
  `pyright_session.py`, `namespace_remount.py`, `apply.py`.
- Workspace and watch reference:
  `backend/src/sandbox/ephemeral_workspace/pipeline.py`,
  `backend/src/sandbox/ephemeral_workspace/events.py`,
  `docs/architecture/sandbox/plugins.html`.
- Workspace materialization reference:
  `backend/src/sandbox/layer_stack/stack.py::LayerStack.commit_to_workspace`,
  `backend/src/sandbox/daemon/layer_stack_runtime.py::commit_to_workspace`,
  `backend/src/sandbox/daemon/builtin_operations.py::commit_to_workspace`,
  `backend/tests/unit_test/test_sandbox/test_layer_stack/test_commit_to_workspace.py`,
  `backend/tests/live_e2e_test/sandbox/workspace_base/test_commit_to_workspace_correctness_perf.py`.
- Rust migration state:
  `sandbox/crates/eos-plugin`, `sandbox/crates/eos-daemon`,
  `sandbox/docs/contract/06-crate-map-and-invariants.md`,
  `docs/plans/sandbox-rust-external-migration-PLAN.md`,
  `docs/plans/sandbox-rust-external-migration-PROGRESS.md`.

## Progress Update - 2026-06-02 12:57 CST

Landed:

- Refined the concurrency contract after live E2E found a remount race. Plugin
  operations still must not serialize on the shared service connection: the
  daemon PPC client remains a multiplexed pending-message table with only a
  short writer mutex. The narrow exception is the mutable service freshness
  gate: stale `workspace_snapshot_refresh` remount/restart work is now
  singleflight per service, and waiters recheck the active manifest after the
  refresh lock so duplicate namespace remounts are skipped.
- Applied the freshness gate uniformly to connected read-only and connected
  self-managed WRITE_ALLOWED services. This fixes the canonical importlib LSP
  bridge write path that previously could run against an old mounted snapshot
  if a self-managed service skipped refresh before dispatch.
- Added focused daemon regressions:
  `self_managed_service_refreshes_after_peer_publish_before_request` proves a
  self-managed WRITE_ALLOWED connected route receives
  `PrepareRefresh -> Quiesce -> SwapWorkspace -> Health` before its operation
  after a peer publish; `concurrent_read_only_refresh_is_singleflight_before_requests`
  proves two concurrent calls to one stale service produce exactly one refresh
  sequence before both plugin requests enter the same PPC stream.
- The first live rerun after removing operation serialization failed before the
  Rust report was written, with `plugin service remount failed ... failed to
  unmount old workspace overlay ... Invalid argument`; that was the duplicate
  namespace-remount race. After singleflight and artifact rebuild, live
  verification passed:
  `EOS_SANDBOX_PROVIDER=docker EOS_LIVE_E2E_IMAGE=xingyaoww/sweb.eval.x86_64.dask_s_dask-10042:latest EOS_PLUGIN_REFRESH_SAMPLES=1 EOS_PLUGIN_REFRESH_AUTO_SQUASH_WRITES=104 EOS_RUST_PLUGIN_BENCH_TIMEOUT_S=600 uv run pytest -q -x -rs --tb=short --durations=10 backend/tests/live_e2e_test/sandbox/plugin/test_plugin_refresh_strategies.py`
  (`1 passed in 50.52s`). The Rust plugin report
  `.omc/results/rust-daemon-plugin-generic-20260602T050316Z-60000.json` had
  `gate_pass=true` and artifact SHA
  `62e6d703964fb5525874629cb39c522e1aa96e25fd80f9619c3da205ef98b83f`.
- The same passing artifact proves the canonical Python importlib LSP bridge
  slice over the reusable PPC service: `lsp_bridge_rename` came from
  `plugins.catalog.lsp.runtime.server`, published
  `live_plugin_lsp_bridge.py` through the mounted-workspace callback, and read
  back `bridge_total`; `lsp_bridge_query_symbols` returned
  `symbol_names=["bridge_total"]` from protocol `lsp-python-importlib`.
- The same artifact preserved the non-serialized operation proof:
  `runtime_bridge_concurrent` returned `fast-second` in `0.0049s` while
  `slow-first` took `0.3587s` on the same service, and
  `runtime_bridge_concurrent_apply` committed both concurrent mounted-workspace
  callback writes. Cleanup ended with no connected routes/services, no plugin
  processes, `post_cleanup_active_leases=0`, and no orphan/missing layers.

Still open:

- Broader AV-10 LSP parity beyond the representative Pyright/generic LSP
  coverage, plus broader crash-recovery matrix coverage. Operation-level PPC
  multiplexing and duplicate remount races are no longer open for the current
  `workspace_snapshot_refresh` service path.

## Progress Update - 2026-06-02 12:08 CST

Landed:

- Enforced the plugin PPC transport rule that operation serialization is
  forbidden. `eos-daemon` now shares one `Arc<PpcClient>` per connected service,
  registers each in-flight operation in a pending table by message id, writes
  frames under only a short stream-write mutex, and uses a dedicated reader
  thread to route out-of-order replies back to the correct caller. The outer
  `Arc<Mutex<PpcClient>>` serialization point was removed from health probes,
  read-only dispatch, refresh sequencing, and self-managed callback dispatch.
- Made concurrent self-managed callbacks deterministic. Plugin callback request
  bodies can carry `parent_message_id`; the daemon routes each callback to the
  owning in-flight operation, falls back to the legacy `parent:suffix` callback
  id shape for older harnesses, and fails ambiguous callback frames instead of
  guessing.
- Updated the reusable bundled Python PPC bridge so arbitrary service runtimes
  can process concurrent request frames on one socket. The bridge now spawns a
  task per request, serializes only socket writes, tracks daemon callback replies
  by message id, and includes `parent_message_id` in mounted-workspace publish
  callbacks.
- Added unit and live gates for the concurrency contract. Rust PPC tests now
  prove out-of-order reply matching and concurrent parent-scoped callbacks over
  one service connection. The daemon op-table test proves two concurrent
  read-only plugin dispatches to one service receive the correct out-of-order
  replies. The live runtime bridge adds `plugin.generic.runtime_bridge_delay_ping`
  plus concurrent mounted-workspace apply/readback probes.
- Live verification passed:
  `EOS_SANDBOX_PROVIDER=docker EOS_LIVE_E2E_IMAGE=xingyaoww/sweb.eval.x86_64.dask_s_dask-10042:latest EOS_PLUGIN_REFRESH_SAMPLES=1 EOS_PLUGIN_REFRESH_AUTO_SQUASH_WRITES=104 EOS_RUST_PLUGIN_BENCH_TIMEOUT_S=600 uv run pytest -q -x -rs --tb=short --durations=10 backend/tests/live_e2e_test/sandbox/plugin/test_plugin_refresh_strategies.py`
  (`1 passed in 48.99s`). The Rust plugin report
  `.omc/results/rust-daemon-plugin-generic-20260602T040405Z-60405.json` had
  `gate_pass=true` and artifact SHA
  `6d58b54f40cdaa8af77a767983dda0b06c27ea0cb4221d781b2b4cce42c431c4`.
  `runtime_bridge_concurrent` sent slow-first delay `0.35s` and fast-second
  delay `0.0s` over the same reusable bridge service; both replies came from the
  runtime bridge/PPC bridge with `workspace_mounted=true`, and the fast second
  request finished at the service and client before the slow first request
  finished. `runtime_bridge_concurrent_apply` concurrently published
  `live_plugin_runtime_bridge_concurrent_a.txt` and
  `live_plugin_runtime_bridge_concurrent_b.txt` through mounted-workspace
  callbacks, with callback success at manifest versions `4` and `5`; LayerStack
  readbacks returned `from concurrent runtime bridge a\n` and
  `from concurrent runtime bridge b\n`. Final metrics were manifest version
  `25`, active service leases `9`, layer dirs/referenced layers `25`, no orphan
  or missing layers, and cleanup moved plugin process count from `12` to `0`
  with post-cleanup active leases `0`.
- The paired refresh-strategy report
  `.omc/results/plugin-refresh-strategies-20260602T040405Z-60405.json` still
  recommends `workspace_snapshot_refresh`; refresh p95 was `6.143 ms` versus
  `commit_to_workspace` p95 `4.914 ms`, raw workspace watch stayed stale without
  materialization, and auto-squash plus post-drain commit passed.

Still open:

- Broader AV-10 LSP parity beyond the representative Pyright and generic LSP
  coverage already listed below, and a broader crash-recovery matrix beyond the
  covered health probe, failed-health isolation/recovery, closed PPC stream
  fail-closed/recovery, PPC timeout fail-closed/recovery, and next-dispatch
  restart paths. Operation-level PPC multiplexing is no longer open: plugin op
  serialization is forbidden and the current daemon/live gates enforce
  message-id-routed concurrent operations over one service connection.

## Progress Update - 2026-06-02 07:34 CST

Landed:

- Added `eos-plugin` contract modules for generic plugin services:
  `manifest.rs`, `refresh.rs`, `service.rs`, and `service_registry.rs`.
  These define `PluginServiceKey`, `ServiceMode`,
  `RefreshStrategy`, manifest validation, the
  `workspace_snapshot_refresh` daemon-to-harness messages, and stale-manifest
  health checks.
- Added the daemon plugin module and registered `api.plugin.ensure` /
  `api.plugin.status` in `eos-daemon`. The Rust daemon now records logical
  plugin manifests/services, reports status, keeps the no-`eos-occ` plugin
  dependency edge, and applies the plugin-family isolated-workspace gate before
  ensure/status.
- Added exact registered-op resolution for manifest-declared
  `plugin.<plugin>.<op>` names. Registered ops now return a structured
  `plugin_dispatch_deferred` response instead of `unknown_op`; undeclared
  `plugin.*` names still return `unknown_op`, and digest reload replaces the
  previous route set.
- Added the first daemon PPC/process boundary slice:
  `sandbox/crates/eos-daemon/src/plugin/process.rs` derives per-service
  `/eos/plugin/ppc/*.sock` endpoints and harness environment from
  `PluginServiceKey`, `api.plugin.ensure/status` now expose `service_processes`,
  and `plugin/ppc_router.rs` performs message-id checked AF_UNIX request/reply.
  Connected read-only routes can now dispatch through PPC without holding the
  daemon plugin registry lock during I/O; same-service concurrent read-only
  requests share the connected client but must never serialize by operation.
  The daemon keeps a pending request table, writes frames under only a short
  stream-write mutex, routes replies by message id from a dedicated reader, and
  drops failed PPC I/O from connected-route status.
- Added the first self-managed callback transport slice: the daemon PPC client
  can now service plugin-originated request frames on the same AF_UNIX stream
  before the final operation reply arrives, validates callback reply direction
  and message id, and preserves the existing one-request `round_trip` behavior
  by failing unexpected callbacks with a typed PPC error. Follow-up coverage now
  proves multiple callback request frames can be serviced on the same plugin
  request before the final operation reply.
- Added the first OCC-backed self-managed callback slice:
  `sandbox/crates/eos-daemon/src/plugin/occ_callbacks.rs` parses the generic
  `daemon.occ.apply_changeset` callback payload, validates that the callback
  targets the service's `layer_stack_root`, converts callback changes into
  `LayerChange`s, and publishes through `dispatcher::apply_occ_changeset` so
  self-managed plugin callbacks use the same daemon-owned per-root OCC writer.
  A connected self-managed `plugin.*` route can now run over PPC, service the OCC
  callback, repeat that callback sequence for more than one daemon-managed
  write, and return the plugin's final reply.
- Added daemon-owned one-shot overlay execution for generic WRITE_ALLOWED plugin
  workers. Manifest services using `service_mode: "oneshot_overlay"` require a
  launch command but are not started as long-lived processes. At dispatch time,
  auto-overlay WRITE_ALLOWED routes acquire a LayerStack snapshot lease, allocate
  a fresh overlay upper/work dir, write a generic request JSON, run the worker in
  `RunMode::FreshNs` against the bound workspace root, read the optional worker
  result JSON, capture the upperdir, compute snapshot base hashes, and publish
  through the same OCC path as shell/write routes.
- Added opt-in service process lifecycle behind `api.plugin.ensure`:
  `start_services: true` spawns service commands with the PPC harness
  environment, reports `running_service_processes`, and tears processes down
  through the daemon registry/drop path. This proves daemon ownership of service
  lifetime without requiring Pyright in focused tests.
- Wired the daemon-side service PPC accept/connect handoff: the daemon binds the
  per-service `/eos/plugin/ppc/*.sock` endpoint, starts the service command,
  accepts the harness connection, restores the accepted stream to blocking mode,
  and registers the connected client by `PluginServiceKey` service instance.
  Focused coverage proves a `start_services: true` service can handle a
  registered read-only `plugin.*` request over that accepted PPC stream.
- Added Linux private-overlay launch for long-lived read-only plugin services.
  `api.plugin.ensure start_services=true` now acquires a service snapshot,
  allocates daemon-owned `/eos/mount/runtime/plugin-service/*` upper/work dirs,
  starts the service through the existing single-threaded `eosd ns-runner`
  boundary with a new `plugin_service` runner verb, mounts the leased LayerStack
  snapshot at the bound workspace root, then executes the vanilla service
  command with the normal PPC environment plus
  `EOS_PLUGIN_WORKSPACE_MOUNTED=1`. Non-Linux/test builds keep the direct-spawn
  path so host unit tests stay portable.
- Added daemon-owned in-place workspace remount for stale long-lived services.
  `eos-overlay` now exposes lazy overlay unmount, `eosd ns-runner
  --remount-overlay` replaces the workspace mount inside the caller's current
  namespace, and `eos-daemon` drives `nsenter -t <service-wrapper-pid> -U -m`
  while the service is quiesced. The refresh order is now
  `PrepareRefresh -> Quiesce -> daemon remount -> SwapWorkspace ->
  NotifyRefresh -> Resume -> Health`, so a package harness receives a normal
  generic refresh protocol while the daemon owns the actual LayerStack snapshot
  mount.
- Added the first `workspace_snapshot_refresh` freshness gate. Started
  long-lived services now retain a daemon LayerStack snapshot lease and record a
  manifest key in status. Before each connected read-only request, the daemon
  compares that service manifest key with the active manifest key. If stale, it
  runs the generic PPC refresh sequence for remount strategies, performs the
  daemon remount after `Quiesce` and before `SwapWorkspace`, swaps the retained
  lease only after the harness acknowledges the new manifest, increments
  `refresh_count`, and only then sends the plugin operation. The focused test
  publishes a peer write, verifies the refresh frames are sent before the next
  read-only request, and confirms status returns `state=ready` with
  `refresh_count=1`.
- Added the generic `restart_service` fallback for stale read-only services.
  When a `workspace_snapshot_refresh` route uses `restart_service`, the daemon
  tears down the old service process/client/snapshot, starts the same declared
  service command against a fresh LayerStack snapshot, accepts a new PPC
  connection, marks the service ready on the active manifest, and increments
  `restart_count` instead of sending refresh frames. Focused coverage publishes
  a peer write before the next read-only request and verifies the restarted
  service answers with `restart_count=1` and `refresh_count=0`.
- Added the first service-crash fail-closed hardening. Before connected
  read-only or self-managed dispatch, the daemon checks any tracked service
  process, removes dead process/client/snapshot state, marks the service
  stopped, releases the retained lease, and returns a structured plugin error
  instead of letting a stale PPC route answer. Focused coverage proves an exited
  tracked process is reaped before dispatch.
- Added live hung-service timeout fail-closed coverage. A separate
  `hang_harness` long-lived service intentionally sleeps past the manifest
  operation timeout; the daemon PPC read timeout tears down that service,
  removes `plugin.generic.hang_probe` from connected routes, releases retained
  snapshot state, records the timeout in service status, and leaves unrelated
  plugin services ready.
- Added live timeout recovery for that same hung service.
  `plugin.generic.hang_recover_ping` restarts `hang_harness` on the next
  dispatch after the timeout, answers from the current daemon-owned snapshot
  with `from_timeout_recovered_service=true`, and restores both
  `plugin.generic.hang_probe` and `plugin.generic.hang_recover_ping`.
- Added the generic service health-probe hardening slice. `api.plugin.status`
  now reaps dead service processes before building `loaded_plugins`; when called
  with `probe_services: true`, it sends the same
  `daemon.workspace_snapshot_refresh` `Health { manifest_key }` frame to every
  connected long-lived service with a retained daemon snapshot, reports
  per-service `service_health`, and fails closed by tearing down only the service
  that cannot acknowledge its retained manifest. Focused coverage proves both
  successful health reporting and failed-health route/snapshot cleanup.
- Added next-dispatch recovery for previously ready read-only services. If a
  `workspace_snapshot_refresh` service had already reached a retained manifest
  and a PPC/process failure later tears down its client, the next dispatch for
  that same route restarts the declared service command against the current
  daemon-owned snapshot instead of leaving the route indefinitely deferred.
  Focused coverage proves the first request fails closed, status reports no
  connected route and `state=stopped`, and the second request returns from the
  restarted service with `restart_count=1`.
- Added focused Rust coverage: `cargo test -p eos-plugin` (`18 passed`) and
  `cargo test -p eos-daemon plugin -- --test-threads=1` (`30 passed`).
- Added live plugin refresh coverage at
  `backend/tests/live_e2e_test/sandbox/plugin/test_plugin_refresh_strategies.py`,
  backed by `backend/scripts/bench_plugin_refresh_strategies.py`, with
  iteration notes in
  `backend/tests/live_e2e_test/sandbox/plugin/ITERATION-REPORT.md`.
- Added live Rust-runtime generic plugin coverage:
  `backend/scripts/bench_rust_daemon_plugin.py` reuses the existing Docker
  sandbox benchmark helpers, uploads the current packaged Rust `eosd`, installs
  a small generic PPC harness, one-shot worker, and vanilla JSON-lines package
  adapter subprocess plus a Pyright setup script under `/eos/plugin/*`, starts
  only the long-lived PPC services through `api.plugin.ensure start_services=true`,
  verifies `api.plugin.status probe_services=true` can health-check the generic
  harness, restart harness, package adapter, Pyright adapter, crash-probe
  service, hang-probe service, and recover-probe service on their retained
  daemon snapshot manifest while failing closed on a deliberately rejecting
  `health_fail_harness`,
  verifies a read-only `plugin.generic.ping`, verifies a self-managed
  `plugin.generic.apply` publish through `daemon.occ.apply_changeset`, verifies
  `plugin.generic.apply_multi` can issue two daemon-owned
  `daemon.occ.apply_changeset` callbacks on the same PPC request before the
  plugin's final reply and then reads both committed files from LayerStack,
  verifies
  the next read-only `plugin.generic.ping` triggers
  `workspace_snapshot_refresh` and advances the harness service to the new
  manifest key, verifies the same refreshed service reads the post-write file
  through its daemon-remounted `EOS_PLUGIN_WORKSPACE_ROOT`, verifies
  `plugin.generic.adapter_query` reaches a package adapter behind
  `refresh_strategy: "remount_workspace"` and returns cached post-refresh file
  content from the adapter process, seeds a Python file through `api.v1.write_file`,
  verifies `plugin.generic.pyright_symbols` reaches a real
  `pyright-langserver --stdio` adapter behind
  `refresh_strategy: "remount_workspace_and_notify"` after daemon remount and
  returns the `live_value` document symbol, verifies
  `plugin.generic.pyright_workspace_symbols` asks that same Pyright service for
  a real LSP `workspace/symbol` response and returns `live_value` from the
  refreshed workspace-wide symbol index, verifies
  `plugin.generic.pyright_completion` asks that same Pyright service for a real
  LSP `textDocument/completion` response and returns a `live_value` completion
  label from a second seeded Python file, verifies
  `plugin.generic.pyright_completion_resolve` resolves the selected
  `live_value` completion item, verifies
  `plugin.generic.pyright_diagnostics` consumes a real
  `textDocument/publishDiagnostics` notification for a separately seeded
  undefined `List` type and normalizes the `reportUndefinedVariable` diagnostic,
  verifies
  `plugin.generic.pyright_code_actions` asks that same Pyright service for a
  real LSP `textDocument/codeAction` response using the advertised
  `source.organizeImports` kind and parses Pyright's empty action list for that
  seed, verifies
  `plugin.generic.pyright_signature_help` asks that same Pyright service for a
  real LSP `textDocument/signatureHelp` response and returns active-parameter
  evidence for a second argument in a typed function call, verifies
  `plugin.generic.pyright_hover` asks that same Pyright service for a real LSP
  `textDocument/hover` response and returns hover text for the call site,
  verifies
  `plugin.generic.pyright_type_definition` asks that same Pyright service for a
  real LSP `textDocument/typeDefinition` response and resolves an instance use
  back to its class definition in a separately seeded file, verifies
  `plugin.generic.pyright_declaration` asks that same Pyright service for a
  real LSP `textDocument/declaration` response and resolves the call site back
  to the seeded function declaration, verifies
  `plugin.generic.pyright_call_hierarchy` asks that same Pyright service for a
  real LSP `textDocument/prepareCallHierarchy` responses,
  `callHierarchy/incomingCalls` from `live_caller` to `live_callee`, and
  `callHierarchy/outgoingCalls` from `live_caller` back to `live_callee`,
  verifies
  `plugin.generic.pyright_document_highlight` asks that same Pyright service
  for a real LSP `textDocument/documentHighlight` response and returns symbol
  highlights for both the declaration and call site, verifies
  `plugin.generic.pyright_prepare_rename` asks that same Pyright service for a
  real LSP `textDocument/prepareRename` response and returns a call-site rename
  range, verifies
  `plugin.generic.pyright_definition` asks that same Pyright service for a real
  LSP `textDocument/definition` response and resolves the call site back to the
  seeded function definition inside the refreshed workspace, verifies
  `plugin.generic.pyright_references` asks that same Pyright service for a real
  LSP `textDocument/references` response and resolves both the declaration line
  and call-site line inside the refreshed workspace, verifies
  `plugin.generic.pyright_rename` asks that same Pyright service for a real LSP
  `textDocument/rename` WorkspaceEdit and publishes the resulting write through
  the daemon-owned `daemon.occ.apply_changeset` callback, verifies
  `plugin.generic.lsp_apply_workspace_edit` converts a generic LSP
  `WorkspaceEdit` into a daemon-owned OCC callback and publishes
  `live_plugin_apply_workspace_edit.py`, verifies
  `plugin.generic.lsp_apply_code_action` applies a CodeAction `edit` through
  the same daemon-owned OCC callback path and publishes
  `live_plugin_apply_code_action.py`, verifies
  `plugin.generic.lsp_format_document` applies generic positive LSP
  `textDocument/formatting` edits through the daemon-owned OCC callback path
  and publishes `live_plugin_format.py`, verifies
  `plugin.generic.lsp_execute_command` applies a positive generic
  `workspace/executeCommand` provider command through the daemon-owned OCC
  callback path and publishes `live_plugin_execute_command.py`, verifies a
  second read-only
  `plugin.generic.restart_ping`
  triggers the `restart_service` fallback for a separate generic service,
  verifies that restarted service reads the post-write file through its mounted
  `EOS_PLUGIN_WORKSPACE_ROOT`,
  verifies `plugin.generic.oneshot_write` through daemon-owned overlay/OCC
  execution, verifies `plugin.generic.crash_probe` fails closed by dropping the
  broken PPC route and marking only that service stopped, verifies
  `plugin.generic.crash_recover_ping` restarts that same crashed service on the
  next dispatch and restores the crash-service routes, verifies
  `plugin.generic.hang_probe` fails closed on PPC timeout by dropping only the
  hung-service routes and marking only that service stopped, verifies
  `plugin.generic.hang_recover_ping` restarts that timed-out service on the
  next dispatch and restores the hung-service routes, verifies
  `plugin.generic.health_fail_ping` is removed when its service rejects the
  daemon health probe while unrelated services stay connected, verifies
  `plugin.generic.health_fail_recover_ping` restarts that same failed-health
  service on the next dispatch and restores the health-fail service routes,
  verifies the bundled `sandbox.ephemeral_workspace.plugin.ppc_service` bridge
  can launch an installed `plugins.catalog.generic.runtime.server` module,
  serve `plugin.generic.runtime_bridge_ping`, and publish
  `plugin.generic.runtime_bridge_apply` through its mounted-workspace OCC
  callback,
  verifies
  `plugin.generic.recover_probe` first fails closed by dropping only the
  recover route and then succeeds on the next dispatch after the daemon restarts
  the previously ready service, verifies active isolated workspace mode blocks
  both `api.plugin.status` and manifest-declared `plugin.generic.ping` with
  `forbidden_in_isolated_workspace`, and verifies isolated exit closes the
  handle before plugin cleanup.
  The pytest wrapper now runs this Rust plugin benchmark after the
  refresh-strategy benchmark in the same integrated sandbox fixture.
- Live verification passed:
  `EOS_SANDBOX_PROVIDER=docker EOS_LIVE_E2E_IMAGE=xingyaoww/sweb.eval.x86_64.dask_s_dask-10042:latest EOS_PLUGIN_REFRESH_SAMPLES=1 EOS_PLUGIN_REFRESH_AUTO_SQUASH_WRITES=104 EOS_RUST_PLUGIN_BENCH_TIMEOUT_S=600 uv run pytest -q -x -rs --tb=short --durations=10 backend/tests/live_e2e_test/sandbox/plugin/test_plugin_refresh_strategies.py`
  (`1 passed in 49.31s` on that previous rerun). The Rust plugin artifact report
  `.omc/results/rust-daemon-plugin-generic-20260602T033206Z-57542.json` used
  `eosd-linux-amd64` SHA
  `dfb9334466e2a32365e944e74e3a809acdb68d39808ba7d595e6e61d52dfc960` and
  proved registered routes `plugin.generic.adapter_query`,
  `plugin.generic.apply`, `plugin.generic.apply_multi`,
  `plugin.generic.crash_probe`, `plugin.generic.crash_recover_ping`,
  `plugin.generic.hang_probe`, `plugin.generic.hang_recover_ping`,
  `plugin.generic.health_fail_ping`,
  `plugin.generic.health_fail_recover_ping`,
  `plugin.generic.lsp_apply_code_action`,
  `plugin.generic.lsp_execute_command`,
  `plugin.generic.lsp_format_document`,
  `plugin.generic.lsp_apply_workspace_edit`,
  `plugin.generic.oneshot_write`, `plugin.generic.ping`, and
  `plugin.generic.pyright_call_hierarchy`,
  `plugin.generic.pyright_capabilities`,
  `plugin.generic.pyright_completion`,
  `plugin.generic.pyright_completion_resolve`,
  `plugin.generic.pyright_definition`,
  `plugin.generic.pyright_declaration`,
  `plugin.generic.pyright_diagnostics`,
  `plugin.generic.pyright_code_actions`,
  `plugin.generic.pyright_document_formatting`,
  `plugin.generic.pyright_document_highlight`, `plugin.generic.pyright_hover`,
  `plugin.generic.pyright_execute_command`,
  `plugin.generic.pyright_prepare_rename`,
  `plugin.generic.pyright_references`, `plugin.generic.pyright_rename`,
  `plugin.generic.pyright_signature_help`, `plugin.generic.pyright_symbols`,
  `plugin.generic.pyright_type_definition`,
  `plugin.generic.pyright_workspace_symbols`, `plugin.generic.recover_probe`,
  `plugin.generic.restart_ping`, `plugin.generic.runtime_bridge_apply`, and
  `plugin.generic.runtime_bridge_ping`;
  connected routes
  `plugin.generic.adapter_query`/`plugin.generic.apply`/
  `plugin.generic.apply_multi`/
  `plugin.generic.crash_probe`/
  `plugin.generic.crash_recover_ping`/
  `plugin.generic.hang_probe`/
  `plugin.generic.hang_recover_ping`/
  `plugin.generic.health_fail_ping`/
  `plugin.generic.health_fail_recover_ping`/
  `plugin.generic.lsp_apply_code_action`/
  `plugin.generic.lsp_execute_command`/
  `plugin.generic.lsp_format_document`/
  `plugin.generic.lsp_apply_workspace_edit`/
  `plugin.generic.ping`/`plugin.generic.pyright_call_hierarchy`/
  `plugin.generic.pyright_capabilities`/
  `plugin.generic.pyright_completion`/
  `plugin.generic.pyright_completion_resolve`/
  `plugin.generic.pyright_declaration`/
  `plugin.generic.pyright_definition`/
  `plugin.generic.pyright_diagnostics`/
  `plugin.generic.pyright_code_actions`/
  `plugin.generic.pyright_document_formatting`/
  `plugin.generic.pyright_document_highlight`/
  `plugin.generic.pyright_execute_command`/
  `plugin.generic.pyright_hover`/`plugin.generic.pyright_prepare_rename`/
  `plugin.generic.pyright_references`/`plugin.generic.pyright_rename`/
  `plugin.generic.pyright_signature_help`/
  `plugin.generic.pyright_symbols`/
  `plugin.generic.pyright_type_definition`/
  `plugin.generic.pyright_workspace_symbols`/
  `plugin.generic.recover_probe`/
  `plugin.generic.restart_ping`/
  `plugin.generic.runtime_bridge_apply`/
  `plugin.generic.runtime_bridge_ping`;
  `status_after_health_probe.service_health` proved successful
  `Health { manifest_key }` acknowledgements for `harness`, `restart_harness`,
  `adapter_harness`, `runtime_bridge`, `pyright_harness`, `crash_harness`,
  `hang_harness`, and `recover_harness` on retained manifest key
  `1:f071e2d096b352b67daeb0f2e2f6dc503335246a98e209fa9f199d06314b5cb5`,
  and failed-health isolation where `health_fail_harness` rejected the same
  probe with `intentional health failure`, was marked `state=stopped`, and
  had `plugin.generic.health_fail_ping` and
  `plugin.generic.health_fail_recover_ping` removed from
  `connected_ppc_routes` while unrelated routes stayed connected;
  same-service failed-health recovery evidence where
  `plugin.generic.health_fail_recover_ping` returned `success=true`,
  `from_health_recovered_service=true`, `from_ppc=true`,
  `workspace_mounted=true`, and `echo == "after-health-fail-recover"`,
  restored `plugin.generic.health_fail_ping` and
  `plugin.generic.health_fail_recover_ping` to connected routes, and left
  `health_fail_harness.state == "ready"` with `restart_count == 1` and
  `last_error == null`;
  initial service `state=ready` on manifest key
  `1:f071e2d096b352b67daeb0f2e2f6dc503335246a98e209fa9f199d06314b5cb5`;
  callback file status `committed`; self-managed readback
  `from live rust plugin\n`; reusable PPC bridge evidence where
  `plugin.generic.runtime_bridge_ping` returned
  `from_ppc_service_bridge=true`, `from_runtime_bridge=true`,
  `workspace_mounted=true`, and read `live_plugin_result.txt` as
  `from live rust plugin\n` after daemon refresh, while
  `runtime_bridge.refresh_count == 1`; `plugin.generic.runtime_bridge_apply`
  returned `from_mounted_workspace_callback=true`, published
  `live_plugin_runtime_bridge.txt` through `daemon.occ.apply_changeset` with
  callback `success=true` at published manifest version `3`, and LayerStack
  readback returned `from reusable ppc bridge\n`; repeated self-managed callback evidence where
  `plugin.generic.apply_multi.callback_count == 2`, callback index `0`
  committed `live_plugin_multi_a.txt` at published manifest version `3`,
  callback index `1` committed `live_plugin_multi_b.txt` at published manifest
  version `4`, and LayerStack readbacks returned
  `from live rust plugin multi a\n` / `from live rust plugin multi b\n`;
  post-write refresh `state=ready` with
  `refresh_count=1` on manifest key
  `5:4dbd00d1c820f1f6d06454fae748741d69628484b6ba921065be63c22e729629`;
  mixed non-plugin/plugin interleave evidence where `api.v1.shell` published
  `live_plugin_shell_result.txt` with `status="ok"`, `exit_code=0`,
  `mutation_source="overlay_capture"`, and one changed path, LayerStack
  readback returned `from live rust shell publish\n`, then a still-running
  `plugin.generic.ping` read the same file through its mounted
  `EOS_PLUGIN_WORKSPACE_ROOT` with `from_ppc=true`, `workspace_mounted=true`,
  manifest key
  `5:4dbd00d1c820f1f6d06454fae748741d69628484b6ba921065be63c22e729629`,
  `refresh_count=1`, and `restart_count=0`; the shell publish timing slice
  recorded `api.shell.total_s=0.05478475`,
  `command_exec.mount_workspace_s=0.001502042`,
  `command_exec.run_command_s=0.034188958`,
  `command_exec.capture_upperdir_s=0.001381875`,
  `command_exec.occ_apply_s=0.003363041`,
  `resource.command_exec.changed_path_count=1`, and zero
  workspace/run/upperdir tree bytes or retained tree entries;
  daemon-remount evidence from the original long-lived service
  `workspace_mounted=true` and
  `refresh_ping.workspace_read.content == "from live rust plugin\n"`;
  package-adapter evidence from `adapter_harness` with
  `refresh_strategy="remount_workspace"`, `state=ready`, `refresh_count=1`,
  `workspace_mounted=true`, and cached package response
  `{"protocol":"line-json-v1","cached":true,"content":"from live rust plugin\n"}`;
  Pyright adapter evidence from `pyright_harness` with
  `refresh_strategy="remount_workspace_and_notify"`, `state=ready`,
  `refresh_count=1`, `workspace_mounted=true`, and LSP response
  `{"protocol":"lsp-jsonrpc","server":"pyright-langserver","symbol_names":["live_value"]}`;
  Pyright workspace-symbol evidence where
  `plugin.generic.pyright_workspace_symbols` returned `symbol_count=1`,
  `symbol_names=["live_value"]`, and `symbol_paths=["live_plugin_pyright.py"]`;
  Pyright completion evidence where `plugin.generic.pyright_completion`
  returned `item_count=2`, `matching_labels=["live_value"]`, and position
  `live_plugin_completion.py` line `3`, character `14`;
  Pyright completion-resolve evidence where
  `plugin.generic.pyright_completion_resolve` advertised
  `completion_resolve=true`, returned `data_present=true`,
  `documentation_text == "def live_value() -> int"`, and resolved
  `request_label == resolved_label == "live_value"` at
  `live_plugin_completion.py` line `3`, character `14`;
  Pyright diagnostics evidence where
  `plugin.generic.pyright_diagnostics` consumed
  `textDocument/publishDiagnostics` for `live_plugin_diagnostics.py`, returned
  `diagnostic_count=1`, `diagnostic_codes=["reportUndefinedVariable"]`, and
  diagnostic message `"List" is not defined`;
  Pyright code-action evidence where
  `plugin.generic.pyright_code_actions` advertised `code_action=true`, confirmed
  `codeActionProvider.codeActionKinds` contains `source.organizeImports`, sent a
  real `textDocument/codeAction` request for `live_plugin_code_actions.py` at
  line `0`, character `0`, and parsed Pyright's empty action list
  (`action_count=0`) for that source-action seed;
  Pyright signature-help evidence where
  `plugin.generic.pyright_signature_help` returned `signature_count=1`,
  `active_parameter=1`, and label `(left: int, right: str) -> str` for
  `live_plugin_signature.py` line `3`, character `28`;
  Pyright hover evidence where `plugin.generic.pyright_hover` returned
  `hover_text == "(function) def live_value() -> int"` for the call at line
  `3`, character `12`;
  Pyright type-definition evidence where
  `plugin.generic.pyright_type_definition` returned `type_definition_count=1`
  and resolved the instance use in `live_plugin_type.py` at line `4`, character
  `11` back to the class definition range starting at line `0`, character `6`;
  Pyright declaration evidence where
  `plugin.generic.pyright_declaration` advertised `declaration=true`, returned
  `declaration_count=1`, and resolved the call at line `3`, character `12` to
  `live_plugin_pyright.py` range start line `0`, character `4`;
  Pyright call-hierarchy evidence where
  `plugin.generic.pyright_call_hierarchy` advertised
  `call_hierarchy=true`, returned `item_count=1` and
  `item_names=["live_callee"]` for `live_plugin_call_hierarchy.py` at line `0`,
  character `11`, returned `incoming_count=1` with
  `incoming_names=["live_caller"]`, then separately queried `live_caller` at
  line `3`, character `11` and returned `outgoing_count=1` with
  `outgoing_names=["live_callee"]`;
  Pyright document-highlight evidence where
  `plugin.generic.pyright_document_highlight` returned `highlight_count=2` with
  `live_plugin_pyright.py` range start lines `0` and `3`;
  Pyright prepare-rename evidence where
  `plugin.generic.pyright_prepare_rename` returned a call-site range at line
  `3`, characters `9..19`;
  Pyright definition evidence where `plugin.generic.pyright_definition`
  returned `definition_count=1` and resolved the call at line `3`, character
  `12` to `live_plugin_pyright.py` range start line `0`, character `4`;
  Pyright references evidence where `plugin.generic.pyright_references`
  returned `reference_count=2` with `include_declaration=true` and locations in
  `live_plugin_pyright.py` at range start lines `0` and `3`;
  Pyright self-managed write evidence where `plugin.generic.pyright_rename`
  returned a real LSP `documentChanges` WorkspaceEdit for
  `live_plugin_pyright.py`, converted it to one generic write changeset, and
  published through `daemon.occ.apply_changeset` with callback status
  `success=true`, file status `committed`, and published manifest version `16`;
  LayerStack readback was
  `def live_total() -> int:\n    return 42\n\nRESULT = live_total()\n`;
  generic LSP apply-workspace-edit evidence where
  `plugin.generic.lsp_apply_workspace_edit` converted a `WorkspaceEdit` for
  `file:///eos/plugin/rust-workspace/live_plugin_apply_workspace_edit.py`
  into one write changeset, published through
  `daemon.occ.apply_changeset` with callback status `success=true`, file
  status `committed`, published manifest version `14`, and LayerStack readback
  `alpha\nedited\n`;
  generic LSP apply-code-action evidence where
  `plugin.generic.lsp_apply_code_action` converted a CodeAction `edit` for
  `file:///eos/plugin/rust-workspace/live_plugin_apply_code_action.py`
  into one write changeset, published through
  `daemon.occ.apply_changeset` with callback status `success=true`, file
  status `committed`, published manifest version `15`, action kind `quickfix`,
  and LayerStack readback `after\nunchanged\n`;
  generic positive LSP formatting evidence where
  `plugin.generic.lsp_format_document` converted a `textDocument/formatting`
  TextEdit for `file:///eos/plugin/rust-workspace/live_plugin_format.py` into
  one write changeset, published through `daemon.occ.apply_changeset` with
  callback status `success=true`, file status `committed`, published manifest
  version `18`, method `textDocument/formatting`, `edit_count=1`, and
  LayerStack readback `def format_me() -> int:\n    return 1\n`;
  generic positive LSP execute-command evidence where
  `plugin.generic.lsp_execute_command` ran advertised command
  `generic.applyWorkspaceEdit`, converted its `workspace/executeCommand`
  argument into one write changeset for
  `file:///eos/plugin/rust-workspace/live_plugin_execute_command.py`,
  published through `daemon.occ.apply_changeset` with callback status
  `success=true`, file status `committed`, published manifest version `20`,
  `supported=true`, `unsupported=false`, and LayerStack readback
  `value = 'after'\n`;
  same-service concurrent dispatch evidence where two concurrent
  `plugin.generic.ping` calls against `harness` returned echoes
  `concurrent-a` and `concurrent-b`, both through PPC, both mounted, both
  `success=true`, and both on manifest key
  `1:f071e2d096b352b67daeb0f2e2f6dc503335246a98e209fa9f199d06314b5cb5`;
  co-shared read-only refresh evidence where `harness` and `adapter_harness`
  were both `ready`, both had `refresh_count=1`, both had `restart_count=0`,
  and both reported manifest key
  `5:4dbd00d1c820f1f6d06454fae748741d69628484b6ba921065be63c22e729629`;
  Pyright negative capability evidence where the reported supports map had
  `document_formatting=false`, `document_range_formatting=false`,
  `execute_command_provider=true`, `execute_command=false`, and raw
  `executeCommandProvider.commands == []`; routed unsupported-operation
  evidence where `plugin.generic.pyright_document_formatting` returned through
  PPC from `pyright_harness` with `success=false`, `unsupported=true`,
  method `textDocument/formatting`, capability
  `documentFormattingProvider`, path `live_plugin_pyright.py`, and
  `edit_count=0`, while `plugin.generic.pyright_execute_command` returned
  through PPC with `success=false`, `unsupported=true`, method
  `workspace/executeCommand`, capability `executeCommandProvider.commands`,
  and advertised `commands=[]`;
  restart fallback `state=ready` with `restart_count=1` and `refresh_count=0`
  on that same post-write manifest key; mounted workspace evidence
  `workspace_mounted=true` and `workspace_read.content == "from live rust plugin\n"`
  from inside the restarted service process;
  one-shot worker exit code `0`; one-shot readback
  `from live rust oneshot plugin\n`; crash probe evidence
  `expected_failure=true` with `ppc channel error: plugin PPC stream closed
  before reply`, `plugin.generic.crash_probe` and
  `plugin.generic.crash_recover_ping` removed from connected routes, and
  `crash_harness.state == "stopped"` with the same error recorded in
  `last_error`; same-service crash recovery evidence where
  `plugin.generic.crash_recover_ping` restarted `crash_harness` on the next
  dispatch, returned `from_crash_recovered_service=true`, `from_ppc=true`,
  `workspace_mounted=true`, and `echo == "after-crash-recover"`, restored both
  crash-service routes, and left `crash_harness.state == "ready"` with
  `restart_count == 1` and `last_error == null`; hung-service timeout evidence
  `expected_failure=true` with
  `daemon io error: Resource temporarily unavailable (os error 11)`,
  `plugin.generic.hang_probe` removed from connected routes, and
  `hang_harness.state == "stopped"` with the same error recorded in
  `last_error`; timeout recovery evidence where
  `plugin.generic.hang_recover_ping` returned `success=true`,
  `from_timeout_recovered_service=true`, `from_ppc=true`,
  `workspace_mounted=true`, and `echo == "after-timeout-recover"`, restored
  `plugin.generic.hang_probe` and `plugin.generic.hang_recover_ping` to
  connected routes, and left `hang_harness.state == "ready"` with
  `restart_count == 1` and `last_error == null`; recovery probe evidence
  `expected_failure=true` on the first
  `plugin.generic.recover_probe` with `ppc channel error: plugin PPC stream
  closed before reply`, `plugin.generic.recover_probe` removed from connected
  routes, `recover_harness.state == "stopped"`, second
  `plugin.generic.recover_probe` returning `from_recovered_service=true` with
  `workspace_mounted=true`, restored connected route, and
  `recover_harness.restart_count == 1`; final manifest version `23`; final
  active service leases before cleanup `9`; post-cleanup active leases `0`; and
  zero post-cleanup orphan layers and missing layers; isolated-workspace gate
  evidence where the daemon enabled `/eos/plugin/iws-scratch`, entered isolated
  mode for the same `AGENT_ID`, rejected both `api.plugin.status` and
  `plugin.generic.ping` with `forbidden_in_isolated_workspace`, exited
  successfully, released the isolated lease, and reported
  `status_after_exit.open=false`; teardown evidence showed
  ten plugin service processes before cleanup, zero after cleanup, empty
  `connected_ppc_routes`, empty `connected_ppc_services`, and empty
  `running_service_processes` after cleanup. The paired
  refresh-strategy report
  `.omc/results/plugin-refresh-strategies-20260602T033206Z-57542.json` still
  recommends `workspace_snapshot_refresh`; refresh p95 was `6.087 ms` versus
  `commit_to_workspace` p95 `4.567 ms`.

Still open:

- Broader crash-recovery matrix beyond the now-covered health probe,
  failed-health isolation, same-service failed-health recovery, closed PPC
  stream fail-closed, same-service crash recovery, PPC timeout fail-closed,
  timeout restart recovery, and next-dispatch restart paths,
  and broader AV-10 LSP parity beyond the representative Pyright
  `documentSymbol` + `workspace/symbol` + `completion` +
  `completionItem/resolve` + `publishDiagnostics` + `codeAction` +
  `signatureHelp` + `hover` + `typeDefinition` + `declaration` +
  `callHierarchy` incoming/outgoing +
  `documentHighlight` + `prepareRename` + `definition` + `references` +
  `rename` + `apply_workspace_edit` + `apply_code_action` +
  `format_document` + `execute_command` path. Current Pyright live artifacts
  route document formatting and execute-command as structured unsupported
  operations because this server does not advertise document/range formatting
  or executable commands; positive generic provider coverage for both operation
  shapes is live separately.
- Generic non-LSP PPC, package-adapter coverage, read-only Pyright refresh, and
  representative Pyright and generic LSP WRITE_ALLOWED/self-managed publish
  paths are live; canonical Python-importlib vs Rust-PPC LSP parity still needs
  broader operation coverage before claiming full AV-10.

## Success Criteria

1. Plugin services are generic. The Rust implementation cannot assume Pyright,
   LSP, Python importlib, Node, or any package-specific lifecycle.
2. Plugin tools are shared-ephemeral only. If the caller has an active
   `isolated_workspace` handle, `api.plugin.*` and `plugin.*` operations fail
   with `forbidden_in_isolated_workspace`.
3. A long-running plugin service never serves a stale workspace silently. Each
   tool call is either against the active LayerStack manifest generation or
   fails with a retryable stale-projection error.
4. The generic read-only service path is daemon-managed
   `workspace_snapshot_refresh`. Arbitrary packages run behind a small service
   harness that speaks the daemon refresh protocol; package-native file watching
   may be used as an internal optimization, but not as the correctness source.
5. The read-only service path never publishes. Write-capable plugin tools, when
   a plugin also exposes them, publish only through the daemon-owned
   LayerStack/OCC path. Self-managed plugin callbacks must use the same
   per-`layer_stack_root` single OCC writer and storage lock as primary
   publishes.
6. O(1) overlay behavior remains the default for read-only services and
   one-shot write workers. A materialized filesystem-watch projection is not the
   target architecture. Do not reintroduce a 110-layer runtime overlay guard;
   keep the kernel `OVL_MAX_STACK = 500` ceiling and operational squash
   telemetry model.

## Current State

The Python path already has the right behavioral split but not the right generic
runtime boundary:

- `Intent.READ_ONLY` handlers run in process and must query a long-lived service.
- `Intent.WRITE_ALLOWED` handlers default to a per-operation overlay plus OCC
  publish.
- `auto_workspace_overlay=False` lets LSP apply/rename/format manage its own
  overlay and publish path.
- Pyright gets a private overlay namespace, remounts when the manifest key
  changes, and receives `workspace/didChangeWatchedFiles` plus open-document
  sync events.
- The daemon dispatcher blocks plugin-family operations while isolated mode is
  open for the agent.

The Rust path is deliberately incomplete:

- `eos-plugin` has the registry/public-op helpers, PPC envelope framing,
  generic service manifests, refresh messages, service keys, and logical
  service status. The old standalone warm-server/dispatch/context scaffold has
  been removed; daemon-connected read-only routes and connected self-managed
  routes execute through the daemon plugin module.
- `eos-daemon` registers `api.plugin.ensure` and `api.plugin.status`, records
  manifest-declared services and operation routes, and resolves exact
  `plugin.<plugin>.<op>` names. Read-only routes with a connected PPC client
  perform a message-id checked AF_UNIX round trip; otherwise registered routes
  still return a structured `plugin_dispatch_deferred` response. With
  `start_services: true`, service commands are spawned and reported as daemon
  owned processes, and the daemon binds/accepts the per-service PPC socket before
  dispatching registered read-only routes through the accepted stream. Concurrent
  read-only calls to the same service now share the connected client without
  operation serialization: in-flight requests are keyed by message id, replies
  may arrive out of order, and the registry mutex is not held during PPC I/O.
  Broken PPC streams are removed from the connected-service map. The PPC client
  can also service plugin-originated callback request frames before the final
  operation reply. For connected self-managed routes, the
  `daemon.occ.apply_changeset` callback now publishes through the same per-root
  daemon OCC writer after validating the callback's layer-stack root. For
  auto-overlay WRITE_ALLOWED routes using `service_mode: "oneshot_overlay"`, the
  daemon runs the service command per operation in a fresh overlay namespace,
  captures the upperdir, and publishes through OCC.
- The daemon-owned service registry uses `PluginServiceKey` so arbitrary plugin
  packages with distinct payload digests, runtimes, environment, and service
  modes do not share incompatible processes.

## Design Decision

Implement a generic `PluginServiceRegistry` in the daemon, backed by
`eos-plugin` contracts, with service instances keyed by:

```text
PluginServiceKey {
  layer_stack_root,
  workspace_root,
  plugin_id,
  plugin_digest,
  service_id,
  service_profile_digest,
  service_mode,
  refresh_strategy,
}
```

The registry owns process lifetime, PPC routing, projection freshness, event
subscriptions, and teardown. A service instance is not "the Pyright session"; it
is a daemon-managed read-only process behind the unified refresh protocol.
`service_profile_digest` covers launch command, environment, protocol version,
service mode, and refresh strategy so reuse cannot cross incompatible services.

Plugin manifests should describe:

- `plugin_id`, `plugin_version`, and content digest.
- `service_id` plus the service profile digest.
- Runtime launch command and payload requirements.
- PPC protocol version, using the existing newline-delimited daemon envelope
  framing.
- Service role:
  - `readonly_service` uses `workspace_snapshot_refresh`.
  - `write_worker` uses `oneshot_overlay` or self-managed daemon callbacks.
- Refresh strategy for read-only services:
  - `remount_workspace_and_notify`
  - `remount_workspace`
  - `restart_service`
- Operation list with `Intent`, `auto_workspace_overlay`, timeout, and whether
  the operation needs a long-lived service process or an operation worker.

The service mode names the daemon-owned freshness model. The strategy names are
mechanism names; `refresh_strategy` already supplies the refresh context, so the
enum values should not repeat it.

## Rust Crate Reuse and File Layout

There is no `sandbox/crates/eos-ephemeral` crate in the current Rust workspace.
Do not add one for plugin refresh. The implementation should reuse the existing
ephemeral workspace semantics as the behavioral contract, while sharing the
current Rust overlay, runner, LayerStack, OCC, and protocol crates directly.

The reuse boundary is:

- Python `backend/src/sandbox/ephemeral_workspace/**` remains the parity source
  for plugin overlay behavior and route semantics during the migration.
- `eos-overlay` owns overlay writable directory allocation and overlay helpers.
- `eos-runner` owns fresh namespace execution, including the long-lived
  read-only `plugin_service` verb.
- `eos-daemon` is the only impure owner that combines LayerStack leases,
  overlay dirs, runner requests, PPC sockets, process lifecycle, and OCC
  callbacks.
- `eos-plugin` stays pure: manifest, service, registry, refresh, and PPC
  contracts only. It must not depend on overlay, LayerStack, OCC, runner, nix,
  or tokio.

The resulting crate/module ownership is:

```text
sandbox/crates/eos-overlay/src/
  lib.rs                 # exports overlay helpers
  writable_dirs.rs       # /eos/mount allocation for upper/work dirs
  kernel_mount.rs        # Linux overlay mount helpers
  path_change.rs         # captured upperdir change classification

sandbox/crates/eos-plugin/src/
  lib.rs                 # exports plugin contracts
  error.rs               # plugin errors
  manifest.rs            # plugin/service manifest validation
  ppc.rs                 # PPC envelope over eos-protocol framing
  refresh.rs             # Prepare/Quiesce/Swap/Notify/Resume/Health types
  registry.rs            # op registration and public plugin.* names
  service.rs             # PluginServiceKey, ServiceMode, RefreshStrategy
  service_registry.rs    # logical registry contract, no daemon I/O

sandbox/crates/eos-runner/src/
  lib.rs                 # RunMode dispatch
  request.rs             # RunRequest, ToolCall, WorkspaceRoot
  fresh_ns.rs            # FreshNs mount + shell/search/plugin_service verbs
  mount.rs               # workspace overlay mount setup
  setns.rs               # existing namespace entry helpers

sandbox/crates/eos-daemon/src/
  plugin/mod.rs          # daemon plugin ensure/status + route dispatch
  plugin/process.rs      # PluginServiceKey -> /eos/plugin/ppc/*.sock process spec
  plugin/ppc_router.rs   # message-id checked PPC round trip + callbacks
  plugin/occ_callbacks.rs
                         # self-managed commit via the daemon OCC writer
  dispatcher.rs          # shared overlay runner, including plugin oneshot wrapper

sandbox/crates/eosd/src/
  main.rs                # ns-runner --mount-overlay / --remount-overlay entrypoint

backend/scripts/
  bench_plugin_refresh_strategies.py
                         # refresh/materialization/auto-squash strategy experiment
  bench_rust_daemon_plugin.py
                         # live generic plugin PPC/OCC/refresh/Pyright harness

backend/tests/live_e2e_test/sandbox/plugin/
  test_plugin_refresh_strategies.py
                         # focused live E2E wrapper for both benchmark scripts
  ITERATION-REPORT.md    # try-by-try findings and artifacts
```

Live runtime experiment files are staged under `/eos/plugin/*`; the plan does
not use the legacy tmp/plugin-refresh roots.

Potential later cleanup, if the daemon plugin module keeps growing:

```text
sandbox/crates/eos-daemon/src/

  plugin/snapshot_refresh.rs
                         # move leased snapshot remount/restart logic out of mod.rs
  plugin/overlay.rs      # move plugin-specific one-shot wrapper out of dispatcher
  plugin/telemetry.rs    # refresh, lease, queue, restart metrics
```

`eos-plugin` may depend on `eos-protocol` plus serde/JSON/error support. It
must not depend on `eos-layerstack`, `eos-occ`, `eos-overlay`, `eos-runner`,
`nix`, or `tokio`. The daemon is the impure owner that combines
`eos-layerstack`, `eos-overlay`, `eos-occ`, `eos-runner`, and `eos-plugin` into
a live service.

## Service Modes

### 1. `workspace_snapshot_refresh`

This is the unified mode for arbitrary read-only plugin services.

The contract is between the daemon and the plugin service harness, not between
the daemon and a package-specific protocol like LSP. The harness may wrap
Pyright, ripgrep-indexers, symbol servers, test discovery daemons, or other
package-specific processes. The daemon controls freshness through a standard
refresh protocol:

```text
PrepareRefresh { target_manifest_key }
Quiesce { request_id }
SwapWorkspace { layer_paths, workspace_root, manifest_key }
NotifyRefresh { changed_paths | full_resync }
Resume { request_id }
Restart { reason }
Health { manifest_key }
```

Flow:

1. Start the service in a private namespace backed by a leased read-only
   workspace overlay.
2. Track `manifest_key` on the service handle.
3. Subscribe to daemon workspace-change events.
4. Before every request, run `ensure_service_current(target_manifest_key)`.
5. If the active manifest changed, acquire a fresh snapshot and refresh the
   service according to its strategy:
   - `remount_workspace_and_notify`: quiesce, remount the service namespace,
     send the daemon refresh notification, then resume.
   - `remount_workspace`: quiesce, remount, invalidate daemon-side request caches,
     then resume. The service must read the filesystem on demand and not rely on
     stale internal indexes for correctness.
   - `restart_service`: terminate and restart the service on the new
     snapshot. This is the generic fallback for arbitrary packages with no safe
     refresh API.
6. If refresh fails, do not answer from stale state. Retry, restart, or return a
   retryable `plugin_projection_stale` error.

This keeps the correctness rule generic: the daemon owns the current manifest
generation and the service must prove it is on that generation before serving a
read.

Package-native file watching is optional. It may improve internal cache
latency, but the daemon refresh protocol is authoritative. A service that only
supports raw OS watches can still be supported through `restart_service`;
that is slower than an adapter-specific refresh hook, but it is generic and
does not require a materialized projection as the default.

### 2. `oneshot_overlay`

Use this for stateless tools and normal write-capable plugin tools.

Flow:

1. Acquire the latest LayerStack snapshot.
2. Mount a fresh per-operation overlay at `workspace_root`.
3. Run the plugin worker inside that namespace.
4. Capture upperdir changes for `WRITE_ALLOWED`.
5. Publish through the daemon's single OCC writer.
6. Release lease and scratch.

This is the generic equivalent of Python `overlay_dispatch.py`. It has the best
freshness story and no watch problem because each invocation starts from the
latest snapshot.

## Freshness Algorithm

Every service handle maintains:

```text
active_manifest_key
active_manifest_version
refresh_strategy
projection_state = current | refreshing | stale | restarting
last_refresh_error
queue_lag
```

Before dispatching a plugin operation:

1. Read the active LayerStack manifest key.
2. If the service key is current, dispatch.
3. If not current, run `ensure_service_current(target_manifest_key)`.
4. If refresh succeeds, dispatch and include the manifest key in telemetry.
5. If refresh fails, return a retryable `plugin_projection_stale` or restart the
   service, depending on operation policy.

Concurrency rules:

- One refresh/update lock per service instance.
- Requests may wait behind refresh up to a bounded timeout.
- Never hold a daemon-wide registry lock across service I/O.
- Use a latest-value channel for manifest targets and a bounded queue for path
  deltas. Queue overflow becomes `NotifyRefresh { full_resync }`, restart, or a
  retryable stale error; it is never silent event loss.

## Overlay and OCC Workflow

Read-only `workspace_snapshot_refresh` workflow:

1. The daemon acquires a LayerStack snapshot lease for the active shared
   ephemeral workspace.
2. The daemon starts or refreshes the service in a private namespace with a
   read-only overlay projection of that snapshot.
3. Before each request, the daemon compares the service manifest key with the
   active LayerStack manifest key.
4. If stale, the daemon refreshes the service by quiescing it, swapping or
   remounting the projection, notifying or restarting the harness, then
   resuming requests.
5. The service answers read-only requests only after reporting the target
   manifest key through `Health`.

This path does not go through OCC because it does not publish. It only consumes
leased snapshots and daemon-owned refresh events.

Write-capable plugin workflow:

1. The daemon acquires the latest LayerStack snapshot.
2. The daemon mounts a fresh per-operation overlay for the worker/apply step.
3. The worker writes into that upperdir.
4. The daemon captures the upperdir result and publishes through the existing
   per-root OCC writer.
5. The daemon releases the lease and scratch state.

So yes: for a write operation, mount first, then publish through OCC. The
long-lived read-only service may compute an edit plan, but it cannot own the
write mount or publish directly.

Sharing rule:

- Multiple operations from the same `PluginServiceKey` may share one
  `workspace_snapshot_refresh` process.
- Multiple plugin services on the same `layer_stack_root` may share daemon-side
  latest-manifest observation, event coalescing, and snapshot-acquire work.
- They must not share process memory, namespace state, PPC sessions, service
  caches, upperdirs, or OCC writers.

## `commit_to_workspace` as a Watcher Bridge

Candidate idea: have the daemon periodically call `api.commit_to_workspace` so a
plugin service watching the target workspace receives native filesystem events.

Assessment: do not use this as the default plugin refresh mechanism.

Current code behavior:

- `LayerStack.commit_to_workspace()` projects the active manifest into a fresh
  rendered tree, replaces the target workspace contents, clears layer-stack
  storage, then rebuilds a fresh base layer from the workspace bytes.
- It refuses to run while any snapshot lease is active with
  `RuntimeError("commit_to_workspace blocked by active leases")`.
- The daemon wrapper documents this as a privileged tear-down sync operation,
  not a steady-state refresh path.

Implications for plugin services:

- A correctly managed long-lived read-only service normally holds a leased
  snapshot/projection. That active lease blocks `commit_to_workspace`.
- Forcing periodic commits would either skip whenever useful work is active, or
  require dropping service leases and remounts on a timer. That turns a refresh
  protocol into repeated global workspace materialization.
- The operation is O(repository bytes) because it renders the merged view and
  rewrites the target workspace. That violates the desired steady-state O(1)
  overlay refresh model.
- Workspace watchers may receive events, but they would see whole-tree replace
  churn, not a precise semantic changed-path stream. This can cause unnecessary
  reindexing and event storms.
- Because commit resets layer storage and rebuilds base, running it while other
  tool calls, background shells, plugin services, or snapshot readers are active
  is intentionally disallowed by the active-lease guard.

Use `commit_to_workspace` only for explicit materialization boundaries, such as
SWE-EVO evaluation or final handoff where active leases have drained. Treat a
daemon timer that calls it every few seconds as a rejected default unless the
experiments below prove a narrowly bounded maintenance mode.

Experiment gates before any periodic-commit mode can be considered:

1. **Lease refusal gate:** hold a plugin-service snapshot lease and verify a
   periodic `api.commit_to_workspace` attempt fails or skips without killing the
   service, leaking a lease, or changing the manifest.
2. **Auto-squash gate:** drive manifest depth beyond the auto-squash threshold,
   trigger squash, then commit after leases drain. Verify raw workspace bytes,
   `.git` preservation, manifest depth, orphan count, and missing-layer count.
3. **Concurrent work gate:** run background shell, direct write/edit, read-only
   service calls, and self-managed plugin callbacks while the periodic committer
   wakes up. Expected behavior is skip/defer while leases or in-flight writes
   exist, not force commit.
4. **Watcher usefulness gate:** run a non-LSP watcher harness on the raw
   workspace and measure whether commit events actually refresh its cache
   correctly. Also measure event count and reindex time; whole-tree churn above a
   small threshold kills the approach.
5. **Throughput gate:** compare tool p95/p99 latency and storage bytes with and
   without a 2s commit timer on a large workspace. Any regression to foreground
   mount/read/write latency or storage lock wait kills the approach.

Expected outcome: this likely fails as a general plugin-service solution because
the active-lease guard and full projection semantics are working as designed.
It may remain useful as an explicit "materialize to target workspace now" API,
not as the freshness source for long-running read-only services.

### Experiment Result - 2026-06-01

Harness:

- Script: `backend/scripts/bench_plugin_refresh_strategies.py`
- Existing container: `2856103e0c53`
- Experiment paths: `/eos/plugin/workspace`, `/eos/plugin/layer-stack`, and
  watcher files under `/eos/plugin/*`
- Transport: daemon TCP endpoint, not Docker exec for measured daemon calls
- Artifacts:
  `bench/plugin-refresh-strategies-20260601.json`,
  `bench/plugin-refresh-strategies-20260601.md`

Results:

- `workspace_snapshot_refresh` refreshed through acquire/release/read in
  p95 `5.747 ms` and never served stale content.
- `commit_to_workspace_timer` materialized in p95 `11.419 ms` on this small
  workspace, and did produce native watcher events.
- `raw_workspace_fs_watch` without materialization stayed stale: daemon reads saw
  `watch-no-commit`, raw workspace still had `initial`, and the watcher saw
  zero target events.
- A synthetic held snapshot lease was not observed by the current
  `api.commit_to_workspace` path in this daemon run; commit succeeded and reset
  storage. That means a periodic materializer would need an explicit daemon
  plugin-service guard before it can be considered safe around long-lived
  plugin services.
- Auto-squash plus post-drain commit passed: after 104 writes, pre-commit
  manifest depth was `10` at version `111`; post-commit manifest depth was `1`,
  raw bytes matched the daemon view, and orphan/missing layer counts were `0`.

Conclusion:

Use `workspace_snapshot_refresh` as the default. It is faster on measured
refresh, does not require raw workspace materialization, and gives the daemon a
generic place to enforce freshness before reads. `commit_to_workspace` remains
an explicit materialization boundary, not a timer. `raw_workspace_fs_watch` is
not correct by itself because LayerStack publishes do not mutate the raw
workspace.

## Write Semantics

The `workspace_snapshot_refresh` service is read-only. It can answer queries or
return an edit plan, but it does not mutate workspace truth and does not publish.
Write-capable plugin tools must use a separate daemon-owned write path.

Allowed write paths:

1. `WRITE_ALLOWED` with `auto_workspace_overlay=true`: daemon acquires a fresh
   operation overlay, runs a worker/adapter, captures upperdir, and publishes
   through OCC.
2. Service-query-plus-daemon-apply: a long-lived service returns an edit plan,
   then the daemon applies it inside a fresh operation overlay and publishes.
3. `auto_workspace_overlay=false`: the service uses PPC callbacks for advanced
   self-managed apply, but those callbacks route into the same daemon-owned
   per-root OCC writer and storage lock.

Rejected paths:

- Capturing the long-lived read-only service overlay.
- Letting a plugin service write directly into LayerStack.
- Creating a second OCC service, commit queue, or storage writer for plugin
  callbacks.
- Allowing plugin operations while isolated workspace is active.

## Adversarial Review Loop

### Round 1 - Overfit Critic

Critique: The current PPC plan still reads as "Pyright in a wrapper." A generic
plugin service cannot depend on LSP notifications, Pyright remount behavior, or
Python importlib compatibility.

Resolution:

- Promote `readonly_service` plus `workspace_snapshot_refresh` into the
  manifest contract.
- Require a non-LSP daemon-refresh probe before declaring generic read-only
  service support.
- Key service instances by plugin identity and digest, not just
  `layer_stack_root`.

### Round 2 - File-Watch Critic

Critique: Overlay remount plus synthetic LSP-style notifications does not
satisfy arbitrary packages that use inotify or similar filesystem watchers.
They may hold inode watches that do not map cleanly across remounts.

Resolution:

- Do not make package-native file watches the correctness contract.
- Define a daemon-to-harness refresh protocol that every read-only service must
  implement.
- Let package adapters choose `remount_workspace_and_notify`,
  `remount_workspace`, or `restart_service`.
- Validate with a non-LSP dummy service that caches file content and proves the
  daemon refresh protocol invalidates or restarts it before the next read.

### Round 3 - Space-Model Critic

Critique: A generic daemon-managed service could drift toward materializing a
full workspace projection to satisfy file watchers, breaking the sandbox O(1)
overlay promise.

Resolution:

- Keep `workspace_snapshot_refresh` on leased LayerStack lowerdirs plus a
  service-private read-only overlay/remount path.
- Treat materialized projections as a rejected default and a future escape hatch
  only if a separate plan proves bounded space.
- Report service lease count, layer path count, refresh count, remount count,
  restart count, and queue lag in plugin telemetry.
- Add a gate that repeated peer publishes do not grow service workspace bytes
  except bounded scratch metadata.

### Round 4 - Publish-Correctness Critic

Critique: Self-managed plugin callbacks create a second structural entry point
to OCC. If that callback constructs its own writer, parity tests can pass while
contention correctness is broken.

Resolution:

- Keep `eos-plugin` free of `eos-occ`.
- Have `eos-daemon` own the only concrete OCC service cache.
- Pass the same per-root OCC runtime services into both primary plugin writes and
  self-managed callback handling.
- Add concurrent interleave tests: self-managed plugin writes plus direct
  write/edit plus shell publishes.

### Round 5 - Isolation Critic

Critique: "Plugin tools only under ephemeral workspace mode" can be weakened if
`api.plugin.ensure` or `api.plugin.status` bypass the isolated gate.

Resolution:

- Treat every `api.plugin.*` and `plugin.*` op as plugin-family.
- Extract `agent_id` from the daemon envelope.
- If that agent has an active isolated handle, return
  `forbidden_in_isolated_workspace` before ensure, status, service start, or
  tool dispatch.
- Preserve no-agent legacy status only for daemon diagnostics that do not observe
  an agent workspace.

## Implementation Phases

### Phase 0 - Contract Tightening

- Extend `sandbox/docs/contract/01-wire-protocol.md` with `api.plugin.ensure`,
  `api.plugin.status`, dynamic `plugin.*`, and PPC callback response shapes.
- Extend `sandbox/docs/contract/03-audit-and-metrics.md` with generic plugin
  service telemetry, avoiding Pyright-specific event names.
- Extend `sandbox/docs/contract/06-crate-map-and-invariants.md` with
  `PluginServiceKey`, read-only refresh strategies, and the `eos-plugin`
  no-`eos-occ` guard.

Checks:

- `cargo tree -p eos-plugin --edges normal` has no `eos-occ`.
- Existing `eos-plugin` unit tests still pass.

### Phase 1 - Daemon Plugin Surface

- Register `api.plugin.ensure` and `api.plugin.status` in `eos-daemon`.
- Add dynamic registration for `plugin.<plugin>.<op>`.
- Add the plugin-family isolated gate in Rust before handler dispatch.
- Use `PluginServiceRegistry` keyed by `PluginServiceKey`; do not reintroduce
  the old layer-stack-root-only warm-server registry.

Checks:

- Unit tests for keying, registration conflict, status shape, exact-key service
  reuse, digest reload, and isolated blocking.

### Phase 2 - Process-Backed PPC

- Spawn plugin service processes as process groups. The focused
  `start_services: true` lifecycle and daemon-side socket handoff are landed;
  on-demand status health probes are landed; periodic heartbeat, broader crash
  recovery, and broader AV-10 LSP parity remain.
- Connect through AF_UNIX PPC using the existing envelope framing. The focused
  single-request route is landed for connected read-only services; process
  accept/connect handoff and same-service concurrent multiplexing are landed.
  Serialization of plugin operations is forbidden; lifecycle refresh/remount may
  briefly gate a stale service before operation dispatch, but active operation
  requests share one PPC stream with message-id matched, out-of-order replies.
  Plugin-to-daemon callback-frame servicing and OCC callback body handling are
  landed for connected self-managed services, including repeated callback
  frames before the final plugin reply.
- Support message-id matched request/reply and plugin-to-daemon callbacks. The
  transport loop and daemon OCC callback handling are landed for the current
  connected-service path.
- Add explicit teardown, timeout, on-demand health probing, and crash recovery.

Checks:

- PPC round trip with message-id matched reply.
- Mismatched message id rejection.
- Callback request before final reply with message-id checked callback response.
- Connected self-managed route publishes through the daemon OCC callback and
  returns the plugin's final reply.
- Same-service concurrent read-only requests share the connected service client.
- Broken PPC streams are removed from connected-route status.
- Service crash returns structured plugin error and reaps process group.
- No daemon registry lock is held during PPC I/O or service process
  spawn/accept handoff.

### Phase 3 - `oneshot_overlay` Writes

- Implement `dispatch_write_allowed` against daemon-owned overlay acquire,
  worker invocation, upperdir capture, and OCC publish.
- Preserve plugin result plus publish metadata.
- Keep service projection out of the publish path.

Checks:

- Python parity for `test_plugin_write_allowed_apply_workspace_edit_publishes`.
- Rust unit/integration test proving one publish through the existing OCC writer.

### Phase 4 - `workspace_snapshot_refresh` Service

- Implement `workspace_snapshot_refresh`.
- Implement the daemon-to-harness refresh protocol:
  `PrepareRefresh`, `Quiesce`, `SwapWorkspace`, `NotifyRefresh`, `Resume`,
  `Restart`, and `Health`.
- Support `remount_workspace_and_notify`, `remount_workspace`, and `restart_service`.
- Port Pyright/LSP as one adapter, not as the service model itself. The current
  live slice proves a representative read-only `pyright-langserver` operation
  set after daemon remount plus Pyright-computed `textDocument/rename` and
  generic `apply_workspace_edit`/`apply_code_action` publishes through the
  daemon OCC callback; broader LSP operation parity remains in AV-10.
- Add a non-LSP read-only dummy service that caches workspace content and only
  stays correct if the daemon refresh protocol works.

Checks:

- Read-only LSP refresh after peer publish, with no plugin publish timing.
- Peer publish plus service refresh without cold restart.
- Stale-key replacement and ensure starts a new service process.
- Non-LSP service reads the post-publish content and never serves its cached
  pre-publish value.

### Phase 5 - Read-Only Sharing and Refresh Coalescing

- Share the daemon event subscription, latest-manifest channel, and snapshot
  acquisition across all services for the same `layer_stack_root`.
- Coalesce concurrent refreshes targeting the same manifest key.
- Keep process, namespace, PPC connection, and service cache state isolated per
  `PluginServiceKey`.
- Allow multiple operations from the same plugin service to reuse one read-only
  service instance when plugin id, digest, service id, service profile digest,
  workspace root, and refresh strategy match.

Checks:

- Two services on one workspace observe the same manifest generation after a
  peer publish.
- A refresh failure in one service does not poison another service.
- Shared refresh metadata does not imply a shared upperdir or shared OCC writer.

### Phase 6 - Contention and Parity Gates

- Add AV-10 plugin parity for READ_ONLY, WRITE_ALLOWED, and self-managed modes.
- Add CP-4 interleave with direct writes, shell publishes,
  `workspace_snapshot_refresh` service calls, and self-managed callbacks.
- Add forward/back parity where Python publishes, Rust plugin reads, Rust plugin
  publishes, and Python reads.

Checks:

- Final workspace hash parity.
- Manifest root hash and layer digest parity for publish paths.
- No stale plugin response after peer publish.
- No `forbidden_in_isolated_workspace` bypass.
- No unbounded service workspace growth.

### Phase 7 - Periodic `commit_to_workspace` Kill-Switch Experiment

This is an experiment, not part of the recommended architecture.

- Add an opt-in daemon maintenance task that wakes on a short interval and tries
  to materialize only when no leases, no plugin refresh, and no in-flight
  workspace mutations exist.
- First implementation should be skip-only: if any guard is active, emit
  telemetry and do nothing.
- Never let the timer force-release leases, cancel work, or restart plugin
  services.
- Run the five `commit_to_workspace` gates from the watcher-bridge section.

Checks:

- Existing commit-to-workspace unit tests pass.
- Active plugin service lease blocks or skips periodic commit.
- Auto-squash plus post-drain commit preserves final bytes and layer metrics.
- Background operations complete with no lost writes, no deadlocks, no stale
  plugin response, and no unexpected service restart.
- Watcher event count and foreground tool p99 remain within explicit thresholds.

## Rejected Alternatives

### One-Shot Everything

Reject as the universal design. It is generic and fresh, but it makes Pyright and
other indexing services unusably expensive because every call pays cold start and
full index cost.

### Unmanaged Workspace Remount Long-Lived Services

Reject. A bare remount without the daemon refresh protocol can leave service
caches stale. Remount is allowed only as one step inside
`workspace_snapshot_refresh`.

### Materialized File-Watch Projection As The Default

Reject. It gives watch compatibility but throws away the O(1) overlay path for
read-only services, stateless tools, and normal write workers.

### Let Services Publish Their Own Writes

Reject. It breaks OCC ownership and makes isolated/shared workspace semantics
ambiguous.

### Periodic `commit_to_workspace` For Watch Refresh

Reject as the default. It is a global materialization/reset operation, not a
read-only refresh primitive. It refuses active leases, performs a full merged
projection, rewrites the target workspace, and rebuilds layer-stack base state.
That makes it appropriate for explicit end-of-run materialization, not for a
few-second daemon timer feeding plugin watchers.

## Final Recommendation

Ship one daemon-managed read-only service layer:

1. `workspace_snapshot_refresh` is the unified abstraction for arbitrary
   read-only plugin services.
2. The daemon owns manifest freshness, remount/restart, service health, and
   event coalescing.
3. Service packages run behind a small harness that implements the standard
   refresh protocol. Package-specific APIs such as LSP are adapters behind that
   harness, not daemon assumptions.
4. Write tools stay outside the read-only service path: fresh operation overlay,
   upperdir capture, OCC publish.
5. `commit_to_workspace` remains an explicit materialization boundary. Do not
   put it on a daemon timer for plugin freshness unless the kill-switch
   experiment proves it only skips under active work and has acceptable latency,
   watcher, and auto-squash behavior.

Do not claim full AV-10 until the representative LSP READ_ONLY and
WRITE_ALLOWED/self-managed paths are canonically compared against the Python
importlib path across the broader operation set. The generic non-LSP package
adapter, read-only Pyright remount, and Pyright self-managed rename paths are
now live evidence for the daemon-owned refresh and publish model, not the full
parity closeout.
