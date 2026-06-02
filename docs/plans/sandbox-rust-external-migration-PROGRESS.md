# Sandbox → Rust migration — PROGRESS

Living status tracker for `docs/plans/sandbox-rust-external-migration-PLAN.md`.
Spec = PLAN.md. Landed-status snapshot = PLAN §13. This file = done/next checklist.

**Last updated:** 2026-06-02 · **Phase:** 3 is closed at the structural core boundary; Phase 3T has closed CP-4t and the deferred non-plugin sidecar through CP-4/AV-4, CP-5, AV-7, and §7. Plugin work has advanced from a pure deferred edge to a Rust contract/status/routing/PPC/lifecycle/freshness slice: `eos-plugin` owns generic service manifest/refresh/key/status types, `eos-daemon` owns `api.plugin.ensure/status`, manifest-declared `plugin.*` routing, service process lifecycle, private-overlay service launch, retained snapshot leases, in-place namespace remount, stale-read refresh sequencing, per-service refresh/remount singleflight, restart fallback, connected self-managed OCC callbacks including repeated callback frames before a final reply, daemon-owned `oneshot_overlay` execution, status health probes over the generic `Health { manifest_key }` frame, next-dispatch recovery for previously ready read-only services after PPC/process failure, operation serialization forbidden with same-service concurrent read-only/self-managed multiplexing and out-of-order reply preservation on the connected PPC client, parent-message-id routed concurrent callbacks, mixed `api.v1.shell` overlay/OCC publish plus long-lived plugin refresh/readback, isolated-workspace plugin-family blocking for `api.plugin.*` and `plugin.*`, daemon cleanup that drops connected PPC routes/services and reaps plugin harness processes, a generic package-adapter harness over `remount_workspace`, a reusable bundled Python PPC service bridge over `remount_workspace_and_notify`, explicit co-shared read-only refresh evidence across two services on one workspace, real Pyright read-only LSP `documentSymbol`, `workspace/symbol`, `completion`, `completionItem/resolve`, `publishDiagnostics`, `codeAction`, `signatureHelp`, `hover`, `typeDefinition`, `declaration`, `callHierarchy` incoming/outgoing, `documentHighlight`, `prepareRename`, `definition`, and `references` adapters over `remount_workspace_and_notify`, Pyright-computed rename plus generic LSP `apply_workspace_edit`, `apply_code_action`, `format_document`, and `execute_command` self-managed publishes through the daemon OCC callback, canonical Python importlib LSP bridge rename/query over the reusable PPC service, routed structured unsupported Pyright `document_formatting` and `workspace/executeCommand` responses matching the server's advertised capability boundary, cleanup-aware retained-lease release evidence, failed-health isolation plus same-service failed-health recovery, live service-crash fail-closed plus same-service crash recovery, and hung-service timeout fail-closed plus timeout-restart probes. Live plugin refresh strategy coverage and live Rust-runtime generic PPC/OCC/repeated-callback/concurrent-read-dispatch/concurrent-refresh-singleflight/concurrent-runtime-bridge-delay/concurrent-runtime-bridge-apply/shell-publish-refresh/isolated-gate/cleanup-reap/refresh/status-health/failed-health/failed-health-recovery/recovery/remount-read/co-shared-refresh/package-adapter/runtime-bridge/Pyright-read/Pyright-workspaceSymbol/Pyright-completion/Pyright-completionResolve/Pyright-diagnostics/Pyright-codeActions/Pyright-unsupportedDocumentFormatting/Pyright-unsupportedExecuteCommand/Pyright-signatureHelp/Pyright-hover/Pyright-typeDefinition/Pyright-declaration/Pyright-callHierarchy-incoming-outgoing/Pyright-documentHighlight/Pyright-prepareRename/Pyright-definition/Pyright-references/Pyright-rename/LSP-applyWorkspaceEdit/LSP-applyCodeAction/LSP-formatDocument/LSP-executeCommand/canonical-importlib-LSP-bridge/restart/crash/crash-recovery/timeout/timeout-recovery/mounted-workspace/runtime-bridge/oneshot-overlay coverage are in the suite. Remaining Phase 3T plugin work is broader AV-10 LSP parity beyond the representative `documentSymbol` + `workspace/symbol` + `completion` + `completionItem/resolve` + `publishDiagnostics` + `codeAction` + routed unsupported `document_formatting`/`workspace.executeCommand` + `signatureHelp` + `hover` + `typeDefinition` + `declaration` + `callHierarchy` incoming/outgoing + `documentHighlight` + `prepareRename` + `definition` + `references` + `rename` + `apply_workspace_edit` + `apply_code_action` + `format_document` + `execute_command` + canonical importlib LSP bridge path and a broader crash-recovery matrix beyond the covered status health probe, failed-health isolation, same-service failed-health recovery, closed PPC stream fail-closed, same-service crash recovery, PPC timeout fail-closed, timeout restart recovery, and next-dispatch restart paths.

---

## Phase status at a glance

| Phase | Scope | Status |
|---|---|---|
| **0 — Bootstrap** | workspace, eos-protocol, put_archive, pins, CP-0/local upload | ✅ **local amd64+arm64 upload closeout complete; signing/full matrix deferred** |
| 1 — ns-runner (fresh-ns) | `eos-runner` unshare→mount→exec | ✅ **scoped direct `eosd ns-runner` closeout complete; host dispatch is Phase 2** |
| 2 — daemon + read paths | `eos-daemon` RPC, read verbs, readiness | ✅ **CP-3/AV-2 closed on local amd64 Docker/dask** |
| 3 — write/publish + shell/search + background control core | OCC/LayerStack publish, structural shell/search, PPC scaffolding | ✅ **closed at the structural boundary:** direct `write_file`/`edit_file` publish flows through routed `eos-occ`; `api.v1.shell`/`glob`/`grep` overlay paths, background registry/control ops, PPC framing/no-OCC plugin edge, LayerStack squash/GC, and CP-4s structural live evidence are in place |
| 3T — terminal sessions + deferred Phase 3 gates | non-login Bash shell/session tools, typed background/subagent controls, plugin PPC execution, CP-4/CP-5/AV gates | 🟡 **partial:** CP-4t and the deferred non-plugin sidecar are closed; plugin service manifest/refresh/status contracts plus `api.plugin.ensure/status`, manifest-declared `plugin.*` routing, service process specs, connected read-only AF_UNIX PPC round trips, opt-in service process lifecycle, daemon-side service socket accept/connect, private-overlay service launch on Linux, retained service snapshot leases, in-place namespace remount for stale remount strategies, stale-read refresh sequencing before dispatch, per-service refresh/remount singleflight, stale-service restart fallback, next-dispatch recovery for previously ready read-only services, status health probes, failed-health isolation plus same-service failed-health recovery, operation serialization forbidden with same-service concurrent multiplexing and out-of-order reply preservation, parent-message-id routed concurrent callbacks, mixed `api.v1.shell` overlay/OCC publish plus plugin refresh/readback, isolated-workspace plugin-family blocking, broken-client status cleanup, cleanup route/process reap proof, callback-frame servicing, connected self-managed OCC callbacks including repeated callback frames before a final reply, daemon-owned oneshot WRITE_ALLOWED overlay execution, cleanup-aware retained-lease release evidence, explicit co-shared refresh evidence, reusable bundled Python PPC service bridge proof, canonical importlib LSP bridge proof, explicit Pyright unsupported-surface gates for formatting/execute-command, and live Rust-runtime generic PPC/OCC/repeated-callback/concurrent-read-dispatch/concurrent-refresh-singleflight/concurrent-runtime-bridge-delay/concurrent-runtime-bridge-apply/shell-publish-refresh/isolated-gate/cleanup-reap/refresh/status-health/failed-health/failed-health-recovery/recovery/remount-read/co-shared-refresh/package-adapter/runtime-bridge/Pyright-read/Pyright-workspaceSymbol/Pyright-completion/Pyright-completionResolve/Pyright-diagnostics/Pyright-codeActions/Pyright-unsupportedDocumentFormatting/Pyright-unsupportedExecuteCommand/Pyright-signatureHelp/Pyright-hover/Pyright-typeDefinition/Pyright-declaration/Pyright-callHierarchy-incoming-outgoing/Pyright-documentHighlight/Pyright-prepareRename/Pyright-definition/Pyright-references/Pyright-rename/LSP-applyWorkspaceEdit/LSP-applyCodeAction/LSP-formatDocument/LSP-executeCommand/canonical-importlib-LSP-bridge/restart/crash/crash-recovery/timeout/timeout-recovery/mounted-workspace/runtime-bridge/oneshot-overlay coverage have landed; remaining skipped scope is broader AV-10 LSP parity beyond the representative `documentSymbol` + `workspace/symbol` + `completion` + `completionItem/resolve` + `publishDiagnostics` + `codeAction` + routed unsupported `document_formatting`/`workspace.executeCommand` + `signatureHelp` + `hover` + `typeDefinition` + `declaration` + `callHierarchy` incoming/outgoing + `documentHighlight` + `prepareRename` + `definition` + `references` + `rename` + `apply_workspace_edit` + `apply_code_action` + `format_document` + `execute_command` + canonical importlib LSP bridge path and a broader crash-recovery matrix beyond the covered status health probe, failed-health isolation, same-service failed-health recovery, closed PPC stream fail-closed, same-service crash recovery, PPC timeout fail-closed, timeout restart recovery, and next-dispatch restart paths |
| 3.5 — isolated workspace | ns-holder + setns + shell-free net | 🟡 broader later-phase scope: the Phase 3T command-routing/control-plane slice is ✅ closed with ns-holder/setns handoff, shell-free bridge/veth/nft setup, local amd64 Docker/dask live proof, exit inspection, PTY controls, and same-port `3000` isolation; broader isolated soak/cutover gates remain later-phase work |
| 5 — cutover | flip default, delete Python | ⬜ |

Legend: ✅ done · 🟡 partial · ⬜ not started.

---

## Latest Plugin Service Refresh (2026-06-02)

- ✅ `eos-plugin` now has the generic service contract surface for the plugin
  plan: `PluginServiceKey`, `ServiceMode`, `RefreshStrategy`,
  `PluginManifest`, refresh request/ack messages, and logical service status.
  Focused verification: `cargo test -p eos-plugin` (`18 passed`).
- ✅ The plugin service key constructor now takes a typed
  `PluginServiceKeyParts` field bag instead of an 8-argument positional
  constructor, keeping service identity explicit while satisfying
  the workspace-wide clippy `-D warnings` gate. The stale dead-code shim that
  only kept service enum imports linked was removed.
- ✅ The old standalone `eos-plugin` warm-server/dispatch/context scaffold no
  longer owns live execution now that connected read-only routes and
  self-managed OCC callbacks execute through `eos-daemon`. `eos-plugin` remains
  a pure contract/PPC crate: `cargo tree -p eos-plugin --edges normal --depth 1`
  shows only `eos-protocol` plus serde/JSON/error support, with no
  `eos-layerstack`, `eos-occ`, `eos-overlay`, `nix`, or `tokio` edge; the old
  `eos-ephemeral` Rust crate is no longer a workspace member.
- ✅ The `eos-plugin` contract crate now passes the focused pedantic/nursery
  cleanup without reintroducing runtime dependencies: public
  result-returning APIs document `# Errors`, pure helpers are `const` /
  `#[must_use]` where applicable, PPC protocol errors are mapped by reference,
  and contract docs satisfy markdown/paragraph lints. Focused verification:
  `cargo check -p eos-plugin`; `cargo clippy -p eos-plugin --all-targets
  --no-deps -- -W clippy::pedantic -W clippy::nursery`; `cargo test -p
  eos-plugin` (`18 passed`).
- ✅ `eos-daemon` now registers `api.plugin.ensure` and `api.plugin.status`.
  The daemon records logical plugin manifests/services, reports plugin status,
  and checks the plugin-family isolated-workspace gate before ensure/status.
  Manifest-declared `plugin.<plugin>.<op>` names now resolve through the daemon
  plugin registry and return a structured `plugin_dispatch_deferred` response;
  undeclared `plugin.*` names still return `unknown_op`, and digest reload
  replaces old route sets.
  `api.plugin.ensure/status` also expose per-service process specs with
  `/eos/plugin/ppc/*.sock` endpoints and the harness environment derived from
  `PluginServiceKey`.
- ✅ `api.plugin.ensure` now supports opt-in `start_services: true` process
  lifecycle for declared services. The daemon starts service commands with the
  PPC harness environment, reports `running_service_processes` through ensure
  and status, and tears processes down through the daemon registry/drop path.
- ✅ The daemon-side service PPC accept/connect handoff is now wired. For
  `start_services: true`, the daemon binds the per-service
  `/eos/plugin/ppc/*.sock`, starts the service command, accepts the harness
  stream, restores blocking mode on the accepted socket, registers the connected
  client by service instance id, and can dispatch a registered read-only
  `plugin.*` request through that accepted stream.
- ✅ Long-lived read-only service processes now launch inside a private overlay
  namespace on Linux. The daemon acquires the service snapshot, allocates
  `/eos/mount/runtime/plugin-service/*` upper/work dirs, starts the service
  through the existing single-threaded `eosd ns-runner` boundary with the new
  `plugin_service` runner verb, and sets `EOS_PLUGIN_WORKSPACE_MOUNTED=1` for
  the vanilla service command. Non-Linux/test builds keep direct process spawn
  so host-focused unit tests remain portable.
- ✅ Connected read-only plugin routes can now round-trip over the daemon PPC
  client using the existing newline-delimited envelope framing, with strict
  message-id matching and with a cloned per-service client handle so the daemon
  plugin registry lock is not held during AF_UNIX I/O. Same-service concurrent
  read-only calls share that client without operation serialization: requests
  enter a pending map by message id, a dedicated reader routes replies by
  message id, and out-of-order replies are valid. Failed PPC I/O removes the
  broken stream from connected-route status. Registered routes without a
  connected client still return the structured deferred response.
  Focused verification: `cargo test -p eos-daemon plugin -- --test-threads=1`
  (`30 passed`).
- ✅ The daemon PPC transport can now service plugin-originated callback request
  frames before the final operation reply. The focused tests prove callback
  request -> daemon handler -> callback reply -> final plugin reply sequencing,
  repeated callback frames before the same final plugin reply, and reject
  callback handler replies with the wrong message id. Latest focused
  verification: `cargo test -p eos-daemon
  ppc_client_services_multiple_callbacks_before_final_reply -- --test-threads=1`
  (`1 passed`).
- ✅ Connected self-managed plugin routes can now service the
  `daemon.occ.apply_changeset` callback through the same per-root daemon OCC
  writer used by direct write/edit paths. The callback handler validates the
  callback root, parses generic write/delete/symlink/opaque-dir changes, returns
  a PPC reply with publish status/timings, and then lets the plugin send its
  final operation reply. Focused and live coverage prove callback publishes
  bytes into LayerStack through the daemon route, including repeated callback
  publishes from one plugin request.
- ✅ The first `workspace_snapshot_refresh` freshness gate landed for
  connected read-only routes. Started services retain a daemon LayerStack
  snapshot lease and status manifest key. Before every connected read-only
  dispatch, the daemon compares the service manifest key with the active
  LayerStack manifest key; stale services run the generic PPC refresh sequence
  before the plugin op is sent, swap the retained lease only after the harness
  acknowledges the target manifest, and increment `refresh_count`. Focused
  coverage publishes a peer write, observes
  `PrepareRefresh -> Quiesce -> SwapWorkspace -> NotifyRefresh -> Resume -> Health`
  before the next read-only request, and verifies status is `ready` with
  `refresh_count=1`.
- ✅ In-place namespace remount now lands for stale remount strategies.
  `eos-overlay` exposes lazy workspace unmount, `eosd ns-runner
  --remount-overlay` mounts a fresh snapshot in the caller's current namespace,
  and `eos-daemon` enters the service wrapper process namespace with
  `nsenter -t <pid> -U -m --preserve-credentials` while the harness is
  quiesced. The effective order is now
  `PrepareRefresh -> Quiesce -> daemon remount -> SwapWorkspace ->
  NotifyRefresh -> Resume -> Health`, keeping package harnesses generic while
  the daemon owns the actual snapshot mount.
- ✅ Stale-service refresh/remount is now singleflight per service without
  serializing plugin operations. The shared PPC client still accepts many
  in-flight requests by message id, but callers that concurrently observe a
  stale `workspace_snapshot_refresh` service coalesce behind a daemon refresh
  lock, recheck the active manifest after the first refresh, and skip duplicate
  namespace remount/restart work. Focused coverage proves two concurrent
  read-only calls to one stale service emit one refresh sequence before both
  operation requests enter the multiplexed PPC stream; live coverage closed the
  duplicate-remount race found after operation serialization was removed.
- ✅ The generic `restart_service` fallback landed for stale read-only services.
  When a `workspace_snapshot_refresh` service chooses `restart_service`, the
  daemon drops the old service process/client/snapshot, reacquires the latest
  LayerStack snapshot, starts the same declared service command, accepts a fresh
  PPC stream, and records `restart_count` while leaving `refresh_count` at zero.
  Focused coverage publishes a peer write before the next read-only request and
  proves the restarted service answers from the post-write manifest.
- ✅ The first service-crash fail-closed path landed. Before connected
  read-only or self-managed dispatch, the daemon checks any tracked service
  process, removes dead process/client/snapshot state, marks the service
  stopped, releases the retained lease, and returns a structured plugin error
  instead of letting a stale PPC route answer. Focused coverage proves an exited
  tracked process is reaped before dispatch.
- ✅ Live hung-service timeout fail-closed coverage landed. A dedicated
  `hang_harness` long-lived service sleeps past its operation timeout; the
  daemon PPC timeout tears down the service, removes
  `plugin.generic.hang_probe` from connected routes, records the timeout in
  service status, releases retained state, and leaves unrelated plugin services
  ready.
- ✅ Live timeout recovery for that same hung service landed.
  `plugin.generic.hang_recover_ping` restarts `hang_harness` on the next
  dispatch after the timeout, answers from the current daemon-owned snapshot
  with `from_timeout_recovered_service=true`, and restores both
  `plugin.generic.hang_probe` and `plugin.generic.hang_recover_ping`.
- ✅ Status health probing landed for generic long-lived services.
  `api.plugin.status` now reaps dead service processes before it builds the
  returned service state; when called with `probe_services: true`, it sends
  `daemon.workspace_snapshot_refresh` `Health { manifest_key }` to each
  connected service with a retained daemon snapshot, reports per-service
  `service_health`, and tears down only a service that cannot acknowledge its
  retained manifest. Focused daemon tests prove the successful health path and
  the failed-health route/snapshot cleanup path.
- ✅ Next-dispatch recovery landed for previously ready read-only services. If a
  `workspace_snapshot_refresh` service had already reached a retained manifest
  and a PPC/process failure later tears down its client, the next dispatch for
  that route restarts the declared service command against the current daemon
  snapshot. Focused daemon coverage proves the first request fails closed,
  status removes the connected route and marks the service stopped, and the
  second request returns from the restarted service with `restart_count=1`.
- ✅ Daemon-owned oneshot WRITE_ALLOWED overlay execution landed for generic
  plugin workers. `service_mode: "oneshot_overlay"` services require a launch
  command but do not start as long-lived processes. Auto-overlay WRITE_ALLOWED
  routes acquire a LayerStack snapshot lease, allocate a fresh overlay upper/work
  dir, write a generic request JSON, run the worker with `RunMode::FreshNs`
  against the bound workspace root, read the optional worker result JSON, capture
  the upperdir, compute snapshot base hashes, and publish through the same OCC
  path as shell/write routes.
- ✅ Live plugin refresh strategy coverage landed under
  `backend/tests/live_e2e_test/sandbox/plugin/`. The test reuses the existing
  Docker sandbox fixture and `backend/scripts/bench_plugin_refresh_strategies.py`
  so it does not provision a separate benchmark container.
- ✅ Live Rust-runtime generic plugin coverage landed in the same pytest path.
  `backend/scripts/bench_rust_daemon_plugin.py` reuses the existing
  DockerBench/runtime-upload helpers, stages the harness through `/tmp` before
  copying it into `/eos/plugin/*` so it is visible in the live tmpfs mount,
  uploads the current `eosd-linux-amd64` artifact, stages a vanilla JSON-lines
  package-adapter subprocess and a Pyright setup script beside the PPC harness
  and one-shot worker, starts only the long-lived services through
  `api.plugin.ensure start_services=true`,
  verifies `api.plugin.status probe_services=true` health acknowledgements for
  the generic harness, restart harness, package adapter, Pyright adapter,
  crash-probe service, hang-probe service, and recover-probe service, plus
  fail-closed isolation for a deliberately rejecting `health_fail_harness`,
  verifies `plugin.generic.ping`, verifies a self-managed `plugin.generic.apply` write
  through `daemon.occ.apply_changeset`, verifies `plugin.generic.apply_multi`
  can issue two daemon-owned `daemon.occ.apply_changeset` callbacks on the same
  PPC request before the plugin's final reply and reads both committed files
  from LayerStack, verifies the next read-only ping
  refreshes the service to the post-write manifest key and reads the post-write
  file through the daemon-remounted `EOS_PLUGIN_WORKSPACE_ROOT`, verifies
  `plugin.generic.adapter_query` reaches the adapter service using
  `refresh_strategy: "remount_workspace"` and returns cached post-refresh
  package content from the adapter process, seeds a Python file through
  `api.v1.write_file`, verifies `plugin.generic.pyright_symbols` reaches a real
  `pyright-langserver --stdio` adapter using
  `refresh_strategy: "remount_workspace_and_notify"` and returns the
  `live_value` document symbol after daemon remount, verifies
  `plugin.generic.pyright_workspace_symbols` reaches that same Pyright adapter
  and returns `live_value` from the refreshed workspace-wide symbol index,
  verifies
  `plugin.generic.pyright_completion` reaches that same Pyright adapter and
  returns a `live_value` completion label from a second seeded Python file,
  verifies `plugin.generic.pyright_completion_resolve` reaches that same
  Pyright adapter and resolves a raw Pyright completion item with
  `request_label == resolved_label == "live_value"` plus documentation text,
  verifies `plugin.generic.pyright_diagnostics` consumes a real
  `textDocument/publishDiagnostics` notification from that same Pyright adapter
  with `diagnostic_codes=["reportUndefinedVariable"]` for an undefined `List`,
  verifies `plugin.generic.pyright_code_actions` reaches that same Pyright
  adapter, confirms `codeActionProvider.codeActionKinds` advertises
  `source.organizeImports`, and parses the real LSP `textDocument/codeAction`
  empty-list response for `live_plugin_code_actions.py`,
  verifies `plugin.generic.pyright_signature_help` reaches that same Pyright
  adapter and returns active-parameter evidence for a second argument in a typed
  function call, verifies `plugin.generic.pyright_hover` reaches that same
  Pyright adapter and returns hover text for the call site, verifies
  `plugin.generic.pyright_type_definition` reaches that same Pyright adapter
  and resolves an instance use back to its class definition in a separately
  seeded file, verifies
  `plugin.generic.pyright_declaration` reaches that same Pyright adapter and
  resolves the call site back to the seeded function declaration, verifies
  `plugin.generic.pyright_call_hierarchy` reaches that same Pyright adapter and
  proves `textDocument/prepareCallHierarchy` plus
  `callHierarchy/incomingCalls` from `live_caller` to `live_callee` and
  `callHierarchy/outgoingCalls` from `live_caller` back to `live_callee`,
  verifies
  `plugin.generic.pyright_document_highlight` reaches that same Pyright adapter
  and returns declaration plus call-site highlights, verifies
  `plugin.generic.pyright_prepare_rename` reaches that same Pyright adapter and
  returns the call-site rename range before the write path, verifies
  `plugin.generic.pyright_definition` reaches that same Pyright adapter and
  resolves the call site back to the seeded function definition, verifies
  `plugin.generic.pyright_references` reaches that same Pyright adapter and
  resolves both the declaration line and call-site line, verifies
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
  `plugin.generic.lsp_execute_command` applies advertised generic
  `workspace/executeCommand` provider commands through the daemon-owned OCC
  callback path and publishes `live_plugin_execute_command.py`, verifies
  `plugin.generic.restart_ping` restarts a separate stale read-only service to
  the same post-write manifest and reads the post-write file through its mounted
  `EOS_PLUGIN_WORKSPACE_ROOT`, verifies `plugin.generic.oneshot_write` through
  the daemon-owned overlay/OCC path, verifies `plugin.generic.crash_probe`
  fails closed by dropping the broken PPC route and marking only that service
  stopped, verifies `plugin.generic.crash_recover_ping` restarts that same
  crashed service on the next dispatch and restores the crash-service routes,
  verifies `plugin.generic.hang_probe` fails closed on PPC timeout by
  dropping only the hung-service routes and marking only that service stopped,
  verifies `plugin.generic.hang_recover_ping` restarts that timed-out service
  on the next dispatch and restores the hung-service routes, verifies
  `plugin.generic.health_fail_ping` is removed when its service rejects the
  daemon health probe while unrelated services stay connected, verifies
  `plugin.generic.health_fail_recover_ping` restarts that same failed-health
  service on the next dispatch and restores the health-fail service routes,
  verifies
  `plugin.generic.recover_probe` first fails closed by dropping only the
  recover route and then succeeds on the next dispatch after the daemon restarts
  the previously ready service, verifies `harness` and `adapter_harness`
  co-observe one refreshed manifest key without restart after a peer publish,
  verifies two concurrent `plugin.generic.ping` calls against the same
  connected `harness` service preserve distinct replies on one manifest key,
  verifies the bundled Python PPC service bridge can load an arbitrary
  installed plugin runtime module, serve `plugin.generic.runtime_bridge_ping`,
  serve concurrent `plugin.generic.runtime_bridge_delay_ping` calls with a fast
  second request completing before a slow first request on the same service
  connection, and publish concurrent `plugin.generic.runtime_bridge_apply`
  operations through parent-scoped mounted-workspace callbacks,
  verifies cleanup drops all connected PPC routes/services and reaps all
  plugin harness processes,
  verifies a mixed `api.v1.shell` overlay/OCC publish is visible to the
  long-lived plugin service after `workspace_snapshot_refresh`, and verifies
  active isolated workspace mode rejects plugin-family operations with
  `forbidden_in_isolated_workspace`, and verifies
  Pyright's current unsupported formatting/execute-command capability boundary
  through routed structured unsupported PPC responses.
  Latest artifact
  SHA:
  `6d58b54f40cdaa8af77a767983dda0b06c27ea0cb4221d781b2b4cce42c431c4`.
- ✅ Live verification:
  `EOS_SANDBOX_PROVIDER=docker EOS_LIVE_E2E_IMAGE=xingyaoww/sweb.eval.x86_64.dask_s_dask-10042:latest EOS_PLUGIN_REFRESH_SAMPLES=1 EOS_PLUGIN_REFRESH_AUTO_SQUASH_WRITES=104 EOS_RUST_PLUGIN_BENCH_TIMEOUT_S=600 uv run pytest -q -x -rs --tb=short --durations=10 backend/tests/live_e2e_test/sandbox/plugin/test_plugin_refresh_strategies.py`
  passed (`1 passed in 48.99s` on the latest rerun). The generated Rust plugin
  report `.omc/results/rust-daemon-plugin-generic-20260602T040405Z-60405.json`
  had `gate_pass=true`, registered routes `plugin.generic.adapter_query`,
  `plugin.generic.apply`, `plugin.generic.apply_multi`,
  `plugin.generic.crash_probe`, `plugin.generic.crash_recover_ping`,
  `plugin.generic.hang_probe`, `plugin.generic.hang_recover_ping`,
  `plugin.generic.health_fail_ping`,
  `plugin.generic.health_fail_recover_ping`,
  `plugin.generic.lsp_apply_code_action`,
  `plugin.generic.lsp_execute_command`,
  `plugin.generic.lsp_format_document`,
  `plugin.generic.lsp_apply_workspace_edit`, `plugin.generic.oneshot_write`,
  `plugin.generic.ping`, `plugin.generic.pyright_call_hierarchy`,
  `plugin.generic.pyright_capabilities`,
  `plugin.generic.pyright_completion`,
  `plugin.generic.pyright_completion_resolve`,
  `plugin.generic.pyright_declaration`,
  `plugin.generic.pyright_definition`,
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
  `plugin.generic.restart_ping`, `plugin.generic.runtime_bridge_apply`,
  `plugin.generic.runtime_bridge_delay_ping`, and
  `plugin.generic.runtime_bridge_ping`,
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
  `plugin.generic.runtime_bridge_delay_ping`/
  `plugin.generic.runtime_bridge_ping`,
  status health probe acknowledgements for `harness`, `restart_harness`,
  `adapter_harness`, `runtime_bridge`, `pyright_harness`, `crash_harness`,
  `hang_harness`, and `recover_harness` on retained manifest key
  `1:f071e2d096b352b67daeb0f2e2f6dc503335246a98e209fa9f199d06314b5cb5`,
  failed-health isolation where `health_fail_harness` rejected the same probe
  with `intentional health failure`, was marked `state=stopped`, and had
  `plugin.generic.health_fail_ping` and
  `plugin.generic.health_fail_recover_ping` removed from connected routes
  while unrelated routes stayed connected,
  same-service failed-health recovery evidence where
  `plugin.generic.health_fail_recover_ping` returned `success=true`,
  `from_health_recovered_service=true`, `from_ppc=true`,
  `workspace_mounted=true`, and `echo == "after-health-fail-recover"`,
  restored `plugin.generic.health_fail_ping` and
  `plugin.generic.health_fail_recover_ping` to connected routes, and left
  `health_fail_harness.state == "ready"` with `restart_count == 1` and
  `last_error == null`,
  callback file status `committed`, post-write `refresh_ping_from_ppc=true`,
  repeated self-managed callback evidence where
  `plugin.generic.apply_multi.callback_count == 2`, callback index `0`
  committed `live_plugin_multi_a.txt` at published manifest version `3`,
  callback index `1` committed `live_plugin_multi_b.txt` at published manifest
  version `4`, and LayerStack readbacks returned
  `from live rust plugin multi a\n` / `from live rust plugin multi b\n`,
  mixed shell/plugin interleave evidence where `api.v1.shell` published
  `live_plugin_shell_result.txt` with `status="ok"`, `exit_code=0`,
  `mutation_source="overlay_capture"`, and
  `changed_paths=["live_plugin_shell_result.txt"]`; daemon LayerStack readback
  returned `from live rust shell publish\n`; then a still-running
  `plugin.generic.ping` returned through PPC, mounted the refreshed workspace,
  and read the same file through `EOS_PLUGIN_WORKSPACE_ROOT` on manifest key
  `5:4dbd00d1c820f1f6d06454fae748741d69628484b6ba921065be63c22e729629`
  with `refresh_count=1` and `restart_count=0`; the shell publish timing slice
  recorded `api.shell.total_s=0.05478475`,
  `command_exec.mount_workspace_s=0.001502042`,
  `command_exec.run_command_s=0.034188958`,
  `command_exec.capture_upperdir_s=0.001381875`,
  `command_exec.occ_apply_s=0.003363041`,
  `resource.command_exec.changed_path_count=1`, and zero
  workspace/run/upperdir tree bytes or retained tree entries,
  remounted-service `workspace_read.content == "from live rust plugin\n"`,
  service `refresh_count=1`, adapter service `state=ready` with
  `refresh_count=1`, `workspace_mounted=true`, and cached package response
  `{"protocol":"line-json-v1","cached":true,"content":"from live rust plugin\n"}`,
  reusable runtime bridge evidence where
  `plugin.generic.runtime_bridge_ping` returned
  `from_ppc_service_bridge=true`, `from_runtime_bridge=true`,
  `workspace_mounted=true`, and `workspace_read.content == "from live rust plugin\n"`,
  `runtime_bridge.state == "ready"` with `refresh_count == 1`, and
  `plugin.generic.runtime_bridge_apply` returned
  `from_mounted_workspace_callback=true`, callback `success=true`, changed
  `live_plugin_runtime_bridge.txt`, published manifest version `3`, and read
  back `from reusable ppc bridge\n`,
  concurrent runtime bridge delay evidence where slow-first delay `0.35s` and
  fast-second delay `0.0s` both returned from the reusable PPC service bridge
  with `workspace_mounted=true`, and the fast second request finished at both
  service and client before the slow first request, concurrent runtime bridge
  apply evidence where parent-scoped mounted-workspace callbacks committed
  `live_plugin_runtime_bridge_concurrent_a.txt` and
  `live_plugin_runtime_bridge_concurrent_b.txt` at manifest versions `4` and
  `5` with LayerStack readbacks `from concurrent runtime bridge a\n` and
  `from concurrent runtime bridge b\n`,
  Pyright completion response `item_count=2` with
  `matching_labels=["live_value"]` for `live_plugin_completion.py` line `3`,
  character `14`,
  Pyright completion-resolve response with `completion_resolve=true`,
  `data_present=true`, `documentation_text == "def live_value() -> int"`, and
  `request_label == resolved_label == "live_value"` for
  `live_plugin_completion.py` line `3`, character `14`,
  Pyright diagnostics notification with `diagnostic_count=1`,
  `diagnostic_codes=["reportUndefinedVariable"]`, and message
  `"List" is not defined` for `live_plugin_diagnostics.py` line `0`,
  character `9`,
  Pyright code-action response where `codeActionProvider.codeActionKinds`
  included `source.organizeImports`, the request targeted
  `live_plugin_code_actions.py` line `0`, character `0`, and Pyright returned a
  parsed empty action list (`action_count=0`) for that source-action seed,
  Pyright signature-help response `signature_count=1`,
  `active_parameter=1`, and label `(left: int, right: str) -> str` for
  `live_plugin_signature.py` line `3`, character `28`,
  Pyright read-only service `state=ready` with `refresh_count=1`,
  `workspace_mounted=true`, and LSP response
  `{"protocol":"lsp-jsonrpc","server":"pyright-langserver","symbol_names":["live_value"]}`,
  Pyright workspace-symbol response `symbol_count=1`,
  `symbol_names=["live_value"]`, and `symbol_paths=["live_plugin_pyright.py"]`,
  Pyright hover response
  `hover_text == "(function) def live_value() -> int"` for the call at line
  `3`, character `12`,
  Pyright type-definition response `type_definition_count=1` resolving
  `live_plugin_type.py` line `4`, character `11` back to the class definition
  range start line `0`, character `6`,
  Pyright declaration response `declaration_count=1` resolving the call at line
  `3`, character `12` to `live_plugin_pyright.py` range start line `0`,
  character `4`,
  Pyright call-hierarchy response `item_count=1`,
  `item_names=["live_callee"]`, `incoming_count=1`, and
  `incoming_names=["live_caller"]` for `live_plugin_call_hierarchy.py` line
  `0`, character `11`, plus a second call-hierarchy response for
  `live_caller` at line `3`, character `11` with `outgoing_count=1` and
  `outgoing_names=["live_callee"]`,
  Pyright document-highlight response `highlight_count=2` with
  `live_plugin_pyright.py` range start lines `0` and `3`,
  Pyright prepare-rename response with call-site range line `3`, characters
  `9..19`,
  Pyright definition response `definition_count=1` resolving the call at line
  `3`, character `12` to `live_plugin_pyright.py` range start line `0`,
  character `4`,
  Pyright references response `reference_count=2` with
  `include_declaration=true` and locations in `live_plugin_pyright.py` at range
  start lines `0` and `3`,
  Pyright self-managed rename evidence where the same service returned a real
  LSP `documentChanges` WorkspaceEdit, changed only `live_plugin_pyright.py`,
  published through `daemon.occ.apply_changeset` with callback
  `success=true`, file status `committed`, and published manifest version `16`,
  then read back
  `def live_total() -> int:\n    return 42\n\nRESULT = live_total()\n`,
  generic LSP apply-workspace-edit evidence where
  `plugin.generic.lsp_apply_workspace_edit` converted a `WorkspaceEdit` for
  `file:///eos/plugin/rust-workspace/live_plugin_apply_workspace_edit.py`
  into one write changeset, published through
  `daemon.occ.apply_changeset` with callback `success=true`, file status
  `committed`, published manifest version `14`, and read back
  `alpha\nedited\n`,
  generic LSP apply-code-action evidence where
  `plugin.generic.lsp_apply_code_action` converted a CodeAction `edit` for
  `file:///eos/plugin/rust-workspace/live_plugin_apply_code_action.py`
  into one write changeset, published through
  `daemon.occ.apply_changeset` with callback `success=true`, file status
  `committed`, published manifest version `15`, action kind `quickfix`, and read
  back `after\nunchanged\n`,
  generic positive LSP formatting evidence where
  `plugin.generic.lsp_format_document` converted a `textDocument/formatting`
  TextEdit for `file:///eos/plugin/rust-workspace/live_plugin_format.py` into
  one write changeset, published through `daemon.occ.apply_changeset` with
  callback `success=true`, file status `committed`, published manifest version
  `18`, method `textDocument/formatting`, `edit_count=1`, and read back
  `def format_me() -> int:\n    return 1\n`,
  generic positive LSP execute-command evidence where
  `plugin.generic.lsp_execute_command` ran advertised command
  `generic.applyWorkspaceEdit`, converted its `workspace/executeCommand`
  argument into one write changeset for
  `file:///eos/plugin/rust-workspace/live_plugin_execute_command.py`,
  published through `daemon.occ.apply_changeset` with callback
  `success=true`, file status `committed`, published manifest version `20`,
  method `workspace/executeCommand`, `supported=true`, `unsupported=false`, and
  read back `value = 'after'\n`,
  same-service concurrent dispatch evidence where two concurrent
  `plugin.generic.ping` calls against `harness` returned echoes
  `concurrent-a` and `concurrent-b`, both through PPC, both mounted, both
  `success=true`, and both on manifest key
  `1:f071e2d096b352b67daeb0f2e2f6dc503335246a98e209fa9f199d06314b5cb5`,
  co-shared refresh evidence where `harness` and `adapter_harness` were both
  `ready`, both had `refresh_count=1`, both had `restart_count=0`, and both
  reported manifest key
  `5:4dbd00d1c820f1f6d06454fae748741d69628484b6ba921065be63c22e729629`,
  Pyright capability boundary evidence with `document_formatting=false`,
  `document_range_formatting=false`, `execute_command_provider=true`,
  `execute_command=false`, and raw `executeCommandProvider.commands == []`,
  routed unsupported-operation evidence where
  `plugin.generic.pyright_document_formatting` returned through PPC from
  `pyright_harness` with `success=false`, `unsupported=true`, method
  `textDocument/formatting`, capability `documentFormattingProvider`, path
  `live_plugin_pyright.py`, and `edit_count=0`, plus
  `plugin.generic.pyright_execute_command` returned through PPC with
  `success=false`, `unsupported=true`, method `workspace/executeCommand`,
  capability `executeCommandProvider.commands`, and advertised `commands=[]`,
  restart fallback `restart_count=1` with
  `refresh_count=0`, `workspace_mounted=true`, restart-service
  `workspace_read.content == "from live rust plugin\n"`, one-shot worker exit code `0`, readbacks
  `from live rust plugin\n` and `from live rust oneshot plugin\n`, crash probe
  `expected_failure=true` with `ppc channel error: plugin PPC stream closed
  before reply`, `plugin.generic.crash_probe` and
  `plugin.generic.crash_recover_ping` removed from connected routes,
  `crash_harness.state == "stopped"` with the same error recorded in
  `last_error`, same-service crash recovery evidence where
  `plugin.generic.crash_recover_ping` returned `success=true`,
  `from_crash_recovered_service=true`, `from_ppc=true`,
  `workspace_mounted=true`, and `echo == "after-crash-recover"`, restored
  `plugin.generic.crash_probe` and `plugin.generic.crash_recover_ping` to
  connected routes, and left `crash_harness.state == "ready"` with
  `restart_count == 1` and `last_error == null`, hung-service timeout
  `expected_failure=true` with
  `daemon io error: Resource temporarily unavailable (os error 11)`,
  `plugin.generic.hang_probe` removed from connected routes,
  `hang_harness.state == "stopped"` with the same error recorded in
  `last_error`, timeout recovery evidence where
  `plugin.generic.hang_recover_ping` returned `success=true`,
  `from_timeout_recovered_service=true`, `from_ppc=true`,
  `workspace_mounted=true`, and `echo == "after-timeout-recover"`, restored
  `plugin.generic.hang_probe` and `plugin.generic.hang_recover_ping` to
  connected routes, and left `hang_harness.state == "ready"` with
  `restart_count == 1` and `last_error == null`, recovery probe
  `expected_failure=true` on the first
  `plugin.generic.recover_probe` with `ppc channel error: plugin PPC stream
  closed before reply`, `plugin.generic.recover_probe` removed from connected
  routes, `recover_harness.state == "stopped"`, second
  `plugin.generic.recover_probe` returning `from_recovered_service=true` with
  `workspace_mounted=true`, restored connected route,
  `recover_harness.restart_count == 1`, final manifest version `25`, final
  active service leases before cleanup `9`, post-cleanup active leases `0`, and
  post-cleanup orphan/missing layer counts `0`, isolated-workspace gate
  evidence where `/eos/plugin/iws-scratch` was enabled, the same `AGENT_ID`
  entered isolated mode, both `api.plugin.status` and `plugin.generic.ping`
  raised `forbidden_in_isolated_workspace`, isolated exit released the lease,
  and `status_after_exit.open=false`, plus cleanup evidence where plugin
  harness process count went from `12` to `0`, connected PPC routes and services
  were empty, and `running_service_processes` was empty after cleanup.
- ✅ Durable benchmark refresh:
  `.omc/results/plugin-refresh-strategies-20260602T040405Z-60405.json` / `.md`
  recommend `workspace_snapshot_refresh`; p95 refresh `6.143 ms` vs
  `commit_to_workspace` p95 `4.914 ms`; raw workspace watch without
  materialization stayed stale; auto-squash plus post-drain commit passed with
  final active leases, orphan layers, and missing layers all `0`.
- 🟡 Remaining plugin scope: broader AV-10 LSP parity beyond the representative
  Pyright `documentSymbol` + `workspace/symbol` + `completion` +
  `completionItem/resolve` + `publishDiagnostics` + `codeAction` +
  routed unsupported `document_formatting`/`workspace.executeCommand` +
  `signatureHelp` + `hover` + `typeDefinition` +
  `declaration` + `callHierarchy` incoming/outgoing + `documentHighlight` + `prepareRename` +
  `definition` + `references` + self-managed `rename` + `apply_workspace_edit` +
  `apply_code_action` + `format_document` + `execute_command` path. Current Pyright live artifacts route document
  formatting and execute-command as structured unsupported operations because
  this server does not advertise document/range formatting or executable
  commands; positive generic provider coverage for both operation shapes is
  live separately. Remaining plugin work is broader AV-10 LSP parity and a
  broader crash-recovery matrix beyond the covered status health probe,
  failed-health isolation, same-service failed-health recovery, closed PPC
  stream fail-closed, same-service crash recovery, PPC timeout fail-closed,
  timeout restart recovery, and next-dispatch restart paths.

---

## Latest Phase 3T Non-Plugin Refresh (2026-06-02)

- ✅ Current Rust bench-script sweep passed on rebuilt amd64 artifact
  `94a9fa39fdb8744f2f2dd31a6b34393870eb3a5e15d0b7e06add2f60a9e896ea`.
  The sweep used the Rust migration bench scripts directly, not
  `task_center_runner`: `bench/bench-rust-all-phase2-20260602-current.json`
  (`run_id=local-fe928a05c04f`) passed CP-3/AV-2/readiness/TCP transport;
  `bench/bench-rust-all-phase3-20260602-current.json`
  (`run_id=local-7ad6bfb51cb0`) passed CP-4s plus the 1/3/5/10 load matrix;
  `bench/bench-rust-all-phase3t-pty-20260602-current.json`
  (`run_id=local-d7ecdcabb986`) passed finite command, PTY progress,
  PTY stdin, PTY cancel, isolated exit inspection, embedded mixed load, and
  cache churn; `bench/bench-rust-all-phase3t-mixed-non-plugin-20260602-current.json`
  (`run_id=local-037cdd9a4a6f`) passed CP-4/AV-4 with audit/performance
  artifacts; `bench/bench-rust-all-phase3t-av7-20260602-current.json`
  (`run_id=local-928c37d308c9`) passed AV-7 forward/back parity;
  `bench/bench-rust-all-phase3t-section7-20260602-current.json`
  (`run_id=local-d2a345847fb3`) passed the Section 7 non-plugin
  differential/property gate; and
  `bench/bench-rust-all-isolated-inspection-20260602-current.json`
  (`run_id=local-b5492a01bc80`, `--privileged`) passed 74/74 isolated
  scenario checks. The isolated run showed `cgroup_writable=true`, target
  image `ip=nft=`, isolated PTY stdin/progress/natural/timeout/cancel
  behavior, no OCC publication of isolated writes, clean force-exit cleanup,
  and two isolated agents both binding TCP port `3000` while each reached its
  own localhost server and cross-agent access was blocked.
- ✅ Plugin implementation remains explicitly skipped for the non-plugin
  closeout, but the current plugin bench smoke also passed on the same artifact:
  `bench/bench-rust-all-plugin-20260602-current.json`
  (`run_id=local-6c4bab5ca7a0`). This does not expand the non-plugin closure
  boundary to broader AV-10/plugin implementation work.
- ✅ Rust isolated inspection rerun passed after the crate-graph cleanup on the
  rebuilt amd64 artifact
  `ddb923eb0f1a3e6b1cd367ab978f7056088175532a26c6b262a94d3ff029b6e7`:
  `bench/phase3t-rust-isolated-inspection-docker-20260602-post-ephemeral-removal.json`
  (`run_id=local-d8e7bff8015a`) reported `gate_pass=true` with 74/74 scenario
  checks green. The added coverage includes isolated
  `write_pty_command_stdin`, `check_pty_command_progress`, natural completion
  notification, timeout notification, explicit cancel, cancel duplicate
  suppression, and two isolated agents both binding TCP port `3000` while
  cross-agent access is blocked. The target image still lacks `ip` and `nft`,
  so bridge/veth/nftables setup remains daemon-side netlink, not shelling out
  inside the target.
- ✅ Current post-cleanup Rust verification passed: `cargo check --workspace`,
  `cargo fmt --all --check`, `cargo test -p eos-runner -p eos-isolated -p
  eos-ns-holder -p eos-plugin`, `cargo test -p eos-daemon isolated_workspace
  --test phase2_read_paths`, `cargo test -p eos-daemon
  active_pty_records_block_exit_until_cleared`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo check --workspace --target
  x86_64-unknown-linux-musl`, and `cargo run -p xtask -- package --target
  x86_64-unknown-linux-musl`. `cargo tree -p eos-isolated --edges normal
  --depth 1` still has no `eos-occ`; `cargo tree -p eos-plugin --edges normal
  --depth 1` stays contract-only; `cargo tree -p eos-ephemeral` no longer
  matches a package.
- ✅ Follow-up daemon server cleanup verification passed after removing the
  stale `OccWriterQueue` path: `cargo check -p eos-daemon -p eosd`, `cargo test
  -p eos-daemon server_dispatches --test phase2_read_paths`, `cargo clippy -p
  eos-daemon -p eosd --all-targets -- -D warnings`, `cargo fmt --all --check`,
  `cargo check --workspace`, and `cargo clippy --workspace --all-targets --
  -D warnings`.
- ✅ Follow-up architecture/prod-Rust audit passed after syncing
  `docs/architecture/sandbox/workspaces.html` with the completed live Docker
  isolated rerun: `cargo check --workspace --all-targets`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo clippy --workspace --lib
  --bins -- -D warnings -D clippy::unwrap_used -D clippy::expect_used`,
  `cargo test --workspace --all-targets`, and `git diff --check`.
- ✅ Follow-up stale-surface cleanup corrected old workspace-count claims to
  the current `10 crates + xtask` / `11 packages` shape and aligned the daemon
  plugin module invariant with the live
  PPC/process/refresh/callback/oneshot-overlay ownership.
- ✅ Follow-up isolated-session cleanup removed the unused Rust
  `WorkspaceHandle::active_calls` placeholder and unreachable
  `exit_drain_timeout` branch from `eos-isolated`. Active command/PTY
  quiescence in the current Rust slice remains daemon-owned through the active
  PTY exit gate; broader AV-9 exit-drain parity stays listed under section F.
- ✅ Follow-up `exec_command` validation cleanup preserves the exact shell
  string after non-empty validation instead of trimming it before passing the
  command to non-login Bash. The trimmed string helper is now Linux-only because
  it is only needed by PTY/session ids and isolated command roots on Linux.
  Focused verification: `cargo test -p eos-daemon
  exec_command_preserves_shell_string_bytes_after_validation`; `cargo test -p
  eos-daemon exec_command_requires_string_wire_shape`; `cargo clippy -p
  eos-daemon --lib --bins --no-deps -- -D warnings -W clippy::pedantic -W
  clippy::nursery`; `cargo fmt --all --check`.
- ✅ Follow-up production Rust hygiene pass is clean: `RUSTFLAGS='-W
  unused-crate-dependencies' cargo check --workspace --lib --bins` found no
  production unused crate deps; `cargo clippy --workspace --lib --bins
  --no-deps -- -W clippy::pedantic -W clippy::nursery` is warning-free after
  tightening `xtask` packaging helpers, daemon shutdown-token construction,
  plugin service lock lifetimes, and LayerStack shared lease-registry lock
  lifetimes.
- ✅ Remaining production lint exceptions use checked `#[expect(...)]`
  attributes with call-site reasons and are scoped to dispatcher result ABI
  parity, cfg-mirrored non-Linux/test helper parity, kernel `repr(C)` field
  naming, or Serde predicate signatures.
- ✅ Shared-workspace PTY command rerun remains green on the prior CP-4t
  artifact:
  `bench/phase3t-pty-command-docker-20260601-current-eos-paths-post-notify.json`
  (`run_id=local-7b9deab71f9f`) reported `gate_pass=true`,
  `operation_samples_ok=true`, and `load.gate_pass=true`; all 50 operation
  samples were green across finite command, PTY progress, PTY stdin echo,
  PTY cancel, and PTY true/no-op cells. The correctness gates also proved
  `nohup ... 2>&1 &` descendant cleanup for both `tty=false` and `tty=true`.
- ✅ The PTY bench harness now waits for the child `ready` marker before
  measuring stdin echo latency, so `write_pty_command_stdin` samples measure
  stdin delivery/echo rather than child-startup timing.
- ✅ Non-plugin deferred Phase 3T items are closed: CP-4t, typed subagent
  surfaces, Rust isolated command/PTY/network behavior, CP-4/AV-4 mixed
  non-plugin load, CP-5 cache-lock churn, AV-7 forward/back parity, and the
  Section 7 non-plugin differential/property suite. No deferred Phase 3
  non-plugin item remains open after the current bench-script sweep. Remaining
  work is the intentionally skipped plugin implementation/AV-10 scope plus the
  broader later Phase 3.5/AV-9/BYO-image/cutover work.
- ✅ The isolated-workspace bullets below are closed as the Phase 3T command
  routing/control-plane slice: daemon RPC routing, daemon-owned session state,
  ns-holder/setns execution, shell-free bridge/veth/nft setup, no-OCC isolated
  command/PTY results, active-PTY exit blocking, and same-port network
  isolation all have current Docker/dask evidence. They do not by themselves
  close the later Phase 3.5/4 exit gate, which still requires the broader AV-9
  parity and CP-1b BYO-image matrix work listed under section F.
- ✅ Status note: if those same isolated-workspace implementation bullets still
  appear with a yellow marker elsewhere, treat that marker as stale for Phase
  3T. The only remaining yellow isolated-workspace scope is the later Phase 4
  exit gate, not this command-routing/control-plane slice.

---

## DONE (verified through 2026-06-02, all checks re-run independently)

**Rust workspace `/sandbox` — 10 crates + xtask**
- ✅ `eos-protocol` **fully implemented + tested**: version/envelope/cas/audit/models/canonical. **29 tests green incl 18 executed CAS golden fixtures** (the `ensure_ascii` Unicode trap reproduced). CAS ASCII escaping and digest hex encoding now use fixed lowercase hex tables instead of per-byte `format!` allocation while preserving byte-stable fixture output.
- ✅ Faithful `// PORT backend/…:line` anchors remain where they still map deferred work. The obsolete Rust `eos-ephemeral` runtime pipeline/registry skeleton, unused `eos-daemon::ports` injector skeleton, stale `DispatchContext::with_in_flight` compatibility alias, unused public dispatcher registration surface, and stale `eos-occ` placeholder/skeleton comments have been removed; deferred plugin PPC dispatch now returns typed `PluginError::Ensure` errors instead of `todo!()` panic stubs, builtin op registration rejects different-handler collisions instead of silently overwriting routes, and `LayerStack` snapshot acquisition now returns a typed invalid-lease-owner error instead of panicking on an empty owner id. The layer-stack storage writer lock now reports registry/root mutex poisoning through `LayerStackError::LockPoisoned` on fallible acquire/exclusive paths, while RAII drop paths remain non-panicking; the daemon in-flight registry now recovers poisoned best-effort control-state locks instead of panicking heartbeat/cancel/count/drop cleanup paths; the OCC commit queue now reports poisoned receiver/transaction slots as `OccError::QueueStatePoisoned` and avoids invariant `expect(...)` calls in worker close/combine paths; the daemon OCC service cache now reports poisoned cache locks through `DaemonError::StateLockPoisoned` while route validation reuses normalized `LayerPath` values instead of reparsing with `expect(...)`; daemon isolated-workspace lifecycle state now recovers poisoned best-effort control-state locks and maps layer-stack / holder-child lock poisoning to typed setup errors instead of panicking; `eos-runner` setns namespace ordering now carries the `CLONE_NEW*` type beside each namespace FD instead of string-remapping through an `unreachable!()` fallback; `eos-ns-holder` pipe read/write helpers now use checked syscall byte-count conversions instead of unchecked signed-to-unsigned casts; and the Phase 3T PTY command/session path now recovers poisoned output-ring, completion-mailbox, live-session, writer, and cancellation state locks instead of panicking progress/stdin/cancel/natural-exit cleanup paths.
- ✅ The stale daemon `OccWriterQueue` no-op path has been removed. Live shared
  workspace writes route through the dispatcher-owned per-root `OccService`
  cache, while isolated command/PTY results remain audit/result-only and do not
  OCC-publish.
- ✅ The high-risk cross-instance LayerStack lease path is now covered and
  fixed: snapshot leases are shared per storage root across reopened
  `LayerStack` managers, so PTY/plugin/isolated handles that retain a lease id
  and later reopen the stack still block squash/GC from removing their frozen
  lower layers. The new regression
  `cross_instance_lease_retains_squashed_layers_until_reopened_release`
  failed before the fix by squashing to depth 1 with no visible lease barrier;
  it now passes and proves reopened release returns true before GC removes the
  retained tail layers.
- ✅ The final-review status matrix has been reconciled with current execution
  evidence: MF-1's gate text is applied in the plan, SF-1/3/4/5/6 are folded,
  SF-4 active-call TTL semantics are implemented and tested in the Rust
  in-flight registry, SF-5 auto-squash now maps to the Rust
  `AutoSquashMaintenancePolicy` / `LayerStack::squash` / storage-writer guard
  boundary, and SF-6 isolated network/audit SoT references now align with
  `eos-isolated`. Broader AV-10 LSP parity remains the explicit skipped plugin
  tail after the representative Pyright `documentSymbol`/`workspace/symbol`/
  `completion`/`signatureHelp`/`hover`/`typeDefinition`/
  `documentHighlight`/`prepareRename`/`definition`/`references` refresh and
  self-managed Pyright rename smoke.
- ✅ The Rust contract/guidance docs now match the current Cargo graph after
  `eos-ephemeral` removal: the workspace is described as 10 runtime crates plus
  `xtask`, `eos-overlay` depends on protocol only, `eos-occ` depends on
  overlay/protocol with daemon-injected layer-stack ports, `eos-ns-holder` has
  no internal crate edge, and `eosd`'s direct subcommand deps include
  daemon/runner/ns-holder/overlay/protocol.
- ✅ The 2026-06-01 daemon idiom pass tightened non-plugin production code without changing the wire contract: in-flight and isolated poison-lock recovery now use `PoisonError::into_inner` directly, isolated PTY force-cancel helpers borrow PTY id lists by slice and branch on the positive cancellation case, dispatcher overlay/changeset error helpers borrow display-only errors/messages, and the daemon server/reaper `tokio::select!` unit futures use explicit `()` patterns.
- ✅ The 2026-06-02 focused daemon pedantic/nursery pass removed the next
  actionable non-contract warnings without changing public wire behavior:
  audit events now derive `Eq`, simple audit/dispatch/error helpers are `const`
  where possible, audit pull and OCC cache snapshot build response JSON after
  releasing mutex guards, in-flight cancellation clones the abort handle before
  aborting outside the registry lock, isolated snapshot/handle/reset paths
  tighten daemon-state guard lifetimes, and timeout conversion avoids direct
  `u64 as f64` casts. The follow-up complexity pass split shell/plugin overlay
  execution into lease-scoped run helpers plus response builders, split OCC
  commit revalidation into validation/drop/publish/maintenance result helpers,
  moved plugin manifest route/status derivation into small helpers, and added
  narrow `unnecessary_wraps` allowances only on fixed-ABI dispatcher op
  handlers. Focused daemon pedantic/nursery clippy is now warning-clean.
- ✅ The daemon audit/command parsing cleanup now avoids signed cursor casts and manual size casts in the audit ring (`api.audit.pull` keeps negative cursor semantics and filters after-seq with checked conversion), uses saturating wire-size helpers for boot epoch / encoded-size accounting, and parses signed timeout-like command fields through `u64::try_from` instead of an `as` cast. Focused tests cover the audit cursor/filter path, saturating helper behavior, and signed/unsigned timeout parsing.
- ✅ The dispatcher cleanup replaced legacy `map(...).unwrap_or(...)` / `map(...).unwrap_or(false)` patterns with `map_or(...)` / `is_ok_and(...)` in readiness metadata, audit floor-reset gating, auto-squash maintenance, and write-response status construction. Linux-only PTY/isolated command finalization callers now pass borrowed overlay errors to the shared daemon error helper, keeping the host and Linux cfg surfaces aligned. This keeps the existing fallback semantics while removing the remaining non-plugin `map_unwrap_or` reports in the touched dispatcher paths.
- ✅ The in-flight registry test cleanup removed the remaining local
  `panic!`/`expect`/`expect_err` patterns from
  `sandbox/crates/eos-daemon/src/invocation_registry.rs`: async cancellation
  assertions now go through a `Result` helper, and the poison-lock test uses an
  explicit unwind branch instead of panic-style assertion plumbing. Focused
  verification passed with the `invocation_registry` daemon lib tests, daemon
  production clippy with `unwrap_used`, `expect_used`, and
  `undocumented_unsafe_blocks` denied, `cargo fmt --all --check`, diff
  whitespace checks on the touched file, and a focused `panic!/expect/unwrap`
  scan.
  Broader `eos-daemon --all-targets` strict `expect_used` was kept as a
  separate test-surface cleanup at that point; the later daemon plugin cleanup
  below closes the remaining plugin test fixture debt.
- ✅ The Phase 3 daemon write-path integration fixture cleanup converted
  `phase3_write_paths.rs` setup helpers and tests to return `Result` and use
  `?` for filesystem/JSON fixture failures instead of `expect(...)`. Focused
  verification passed with the `phase3_write_paths` daemon integration test,
  strict clippy for that test target with `unwrap_used`, `expect_used`, and
  `undocumented_unsafe_blocks` denied, `cargo fmt --all --check`, and a
  focused `panic!/expect/unwrap` scan.
- ✅ The 3T daemon command/PTY unit-test cleanup converted shell-string and
  completed-PTY control assertions to `Result` tests with `?`/`ok_or(...)`
  propagation, and moved `/dev/null` writer setup behind a fallible helper.
  Focused verification passed with the host `command::tests` daemon lib test
  slice, daemon production clippy with `unwrap_used`, `expect_used`, and
  `undocumented_unsafe_blocks` denied, `x86_64-unknown-linux-musl` daemon
  lib/test `cargo check`, `cargo fmt --all --check`, diff whitespace checks on
  the touched file, and a focused command-source `panic!/expect/unwrap` scan.
- ✅ The non-plugin daemon all-target test cleanup removed panic-style fixture
  setup from `phase2_read_paths.rs`, daemon dispatcher unit tests, and daemon
  isolated unit tests. Those tests now return `Result` and use `?`/`ok_or(...)`
  propagation for filesystem, JSON, LayerStack/OCC, socket, timeout, and typed
  helper failures. Verification passed with the daemon non-plugin lib-test slice
  (`24 passed`), the `phase2_read_paths` integration test (`10 passed`), strict
  clippy for the `phase2_read_paths` target, daemon production clippy with
  `unwrap_used`, `expect_used`, and `undocumented_unsafe_blocks` denied,
  `cargo fmt --all --check`, diff whitespace checks, and focused non-plugin
  `panic!/expect/unwrap` source scans. At that point the broader daemon
  lib/tests strict `expect_used` probe failed only in plugin modules; the later
  daemon plugin cleanup below closes that remaining test-surface debt.
- ✅ The latest non-plugin daemon conversion pass replaced unchecked manifest-version, audit-limit, audit-pressure, edit-count, changed-path-count, route-count, cache-count, PTY spool-byte, timeout, and PTY exit-code `as` casts with checked, saturating, or explicit lossy conversions in the dispatcher/OCC/audit and command finalization paths. Rounded timing-to-integer audit fields now use a saturating helper instead of a direct float-to-int cast. Gated validation now accepts `Option<&str>` for base hashes, parent-absence probing uses an explicit `matches!` predicate, public constructors/response builders that return must-use state are annotated, and the `OccStatus` wire mapping no longer carries a duplicate failed/fallback arm.
- ✅ The 2026-06-02 Phase 3/3T/3.5 Rust cleanup extended the checked-conversion pass across the shared Rust surface: `eos-protocol` / `eos-layerstack` / `eos-daemon` hex encoders use typed byte/char conversions, `eos-overlay` xattr capture checks syscall byte counts, `eos-runner` glob/grep result counters and file-size comparisons avoid direct integer casts, `eos-isolated` quota/veth/nftables setup checks holder PID, capacity, nft message-type, nfnetlink family/subsystem, and message-length conversions, `eos-ns-holder` rtnetlink message construction checks libc family/flag/index/socket-length conversions, and daemon PTY/holder child PIDs plus OCC cache eviction counters use checked/saturating conversions. A targeted source scan now finds no direct `as` casts in the Phase 3/3T/3.5 Rust source set checked here (`eos-daemon`, `eos-protocol`, `eos-overlay`, `eos-occ`, `eos-layerstack`, `eos-runner`, `eos-isolated`, `eos-ns-holder`, `eosd`).
- ✅ The follow-up unsafe/FFI pass tightened `eos-ns-holder`: the best-effort
  `/proc` rbind now uses the native libc flag type without a numeric cast,
  netlink address and struct-byte views use `std::ptr::from_ref(...)`, and the
  PID-namespace init installs SIGTERM/SIGINT handlers through typed
  `nix::sigaction` instead of raw `libc::signal` function-pointer casts. The
  Linux holder/eosd target checks stayed green.
- ✅ The focused isolated-crate pedantic/nursery pass cleaned the Phase 3.5
  support crates without changing runtime behavior: `eos-runner` no longer
  wraps no-follow directory walks and regular-file probes in infallible
  `Result`s, runner request/result docs now satisfy markdown/`Eq` lints,
  `eos-isolated` public fallible ports document `# Errors` and annotate
  must-use constructors/state helpers, and `eos-ns-holder` handshake helpers
  now expose must-use/const state plus explicit error docs while preserving the
  shell-free netlink hooks. Focused verification: `cargo clippy -p
  eos-isolated -p eos-runner -p eos-ns-holder --all-targets --no-deps -- -W
  clippy::pedantic -W clippy::nursery`.
- ✅ The core Phase 3 substrate pedantic/nursery pass is now warning-clean for
  `eos-protocol`, `eos-overlay`, `eos-occ`, and `eos-layerstack` without
  changing wire/storage behavior: protocol CAS/envelope/model APIs document
  errors and must-use state, overlay capture/mount/writable-dir APIs document
  fallible surfaces, OCC queue/service/route APIs tighten lock/test lifetimes,
  and LayerStack public ports/squash/merged-view/workspace binding APIs now
  expose explicit error contracts while removing redundant clones, stack-sized
  read buffers, underscore-live fields, and needless helper `self` receivers.
  Focused verification: `cargo fmt --all --check`; `cargo clippy -p
  eos-protocol -p eos-overlay -p eos-occ -p eos-layerstack --all-targets
  --no-deps -- -W clippy::pedantic -W clippy::nursery`; `cargo check -p
  eos-protocol -p eos-overlay -p eos-occ -p eos-layerstack`; `cargo test -p
  eos-protocol -p eos-overlay -p eos-occ -p eos-layerstack` (`42 passed`
  across unit/integration fixtures plus doc-tests).
- ✅ Follow-up OCC strict-lint cleanup converted commit-queue and overlay
  conversion tests to `Result`/`?` propagation, replaced mock mutex
  lock-poisoning `expect(...)` calls with explicit recovery or typed test
  errors, and removed the remaining local `expect_err(...)` assertion. Focused
  verification passed with `cargo test -p eos-occ` (`6 passed`), `cargo clippy
  -p eos-occ --all-targets --no-deps -- -D warnings -D clippy::unwrap_used -D
  clippy::expect_used -D clippy::undocumented_unsafe_blocks`, `cargo fmt --all
  --check`, `git diff --check`, and a focused `panic!/expect/unwrap` scan on
  the touched OCC test files.
- ✅ Follow-up protocol strict-lint cleanup converted the shared wire/CAS/audit
  unit tests and golden fixture tests to `Result`/`?` propagation plus explicit
  fixture-shape errors. The CAS property strategy now constructs valid
  single-segment `LayerPath` values directly instead of relying on a panic-style
  helper. Focused verification passed with `cargo test -p eos-protocol` (`25`
  lib tests, `1` CAS fixture test, `3` envelope fixture tests), `cargo clippy
  -p eos-protocol --all-targets --no-deps -- -D warnings -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks`, and a focused `panic!/expect/unwrap`
  scan on `eos-protocol`.
- ✅ The shared non-plugin substrate all-target strict-lint gate is now clean:
  the remaining `eos-ns-holder` handshake tests, `eos-overlay` capture /
  writable-dir tests, and `eos-layerstack` lease/squash/delete tests now use
  fallible fixtures and explicit error matches instead of `expect(...)` /
  `expect_err(...)`. Verification passed with `cargo test -p eos-protocol -p
  eos-overlay -p eos-occ -p eos-layerstack -p eos-runner -p eos-isolated -p
  eos-ns-holder` (`59` unit/integration tests plus doc-tests), `cargo clippy
  -p eos-protocol -p eos-overlay -p eos-occ -p eos-layerstack -p eos-runner -p
  eos-isolated -p eos-ns-holder --all-targets --no-deps -- -D warnings -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks`, and a focused `panic!/expect/unwrap`
  scan across that crate set.
- ✅ The public Rust daemon shell boundary no longer carries the historical
  CP-4s raw-argv escape hatch: `api.v1.shell` requires a non-empty command
  string and rejects argv arrays. The last `cp4s_legacy_argv` runner-internal
  compatibility path was removed after plugin one-shot overlay workers were
  switched to the dedicated `plugin_service` runner verb for argv commands;
  `plugin_service` now also uses a process group for timeout cleanup so worker
  descendants are not left behind. Focused verification passed with `cargo test
  -p eos-runner --lib`, `cargo test -p eos-daemon shell_command --lib`, `cargo
  test -p eos-daemon plugin::tests -- --test-threads=1`, `cargo check -p
  eos-runner --target x86_64-unknown-linux-musl --lib --tests`, `cargo check
  -p eos-daemon --target x86_64-unknown-linux-musl --lib --tests`, `cargo
  clippy --workspace --all-targets --no-deps -- -D warnings -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks`, `cargo fmt --all --check`, and focused
  `git diff --check` / stale-legacy scans.
- ✅ Follow-up plan-contract cleanup refreshed `PLAN.md` so CP-4s is described
  as historical raw-argv structural evidence, while the current public
  `api.v1.shell` boundary and `bench_rust_daemon_phase3.py` reruns use
  shell-format command strings. Focused verification passed with a stale
  raw-argv naming scan, `git diff --check` for the touched plan/progress/contract
  docs and Phase 3 bench script, `cargo check -p eos-daemon -p eos-runner -p
  eos-isolated -p eos-ns-holder --lib --tests`, strict clippy for
  `eos-runner`/`eos-isolated`/`eos-ns-holder` all-targets, and strict daemon
  library/binary clippy.
- ✅ Latest focused Phase 3.5 recheck passed: `cargo fmt --check -p eos-protocol -p eos-overlay -p eos-occ -p eos-layerstack -p eos-runner -p eos-isolated -p eos-ns-holder`, host `cargo clippy -p eos-protocol -p eos-overlay -p eos-occ -p eos-layerstack -p eos-runner -p eos-isolated -p eos-ns-holder --all-targets --no-deps -- -D warnings`, `x86_64-unknown-linux-musl` `cargo clippy -p eos-protocol -p eos-overlay -p eos-occ -p eos-layerstack -p eos-runner -p eos-isolated -p eos-ns-holder --target x86_64-unknown-linux-musl --lib --bins --no-deps -- -D warnings`, `cargo test -p eos-protocol -p eos-overlay -p eos-occ -p eos-layerstack -p eos-runner -p eos-isolated -p eos-ns-holder` (`51 passed` across unit/integration fixtures plus doc-tests), and `git diff --check` on the touched Rust files.
- ✅ Latest focused daemon recheck passed: `cargo fmt --check -p eos-daemon`, host `cargo clippy -p eos-daemon --lib --bins --no-deps -- -D warnings`, `x86_64-unknown-linux-musl` `cargo clippy -p eos-daemon --target x86_64-unknown-linux-musl --lib --bins --no-deps -- -D warnings`, and the current host `cargo test -p eos-daemon` (`43 lib tests`, `10 phase2_read_paths`, `5 phase3_write_paths`, doc-tests `0`). This supersedes the narrower earlier daemon cast/panic/unwrap lint slice.
- ✅ Latest non-plugin Phase 3T/3.5 production-review pass found no remaining
  production `unwrap`/`expect` or unused direct-crate cleanup in the focused
  daemon/runner/isolated/holder slice. Verification: `cargo clippy -p
  eos-daemon -p eos-runner -p eos-isolated -p eos-ns-holder --lib --bins
  --no-deps -- -D warnings -W clippy::pedantic -W clippy::nursery -D
  clippy::unwrap_used -D clippy::expect_used`; `RUSTFLAGS='-D
  unused-crate-dependencies' cargo check -p eos-runner -p eos-isolated -p
  eos-ns-holder --lib --bins`. The PTY completion collector remains intentional
  because the Python background supervisor still uses it for natural-exit and
  timeout notifications; `progress`/`stdin`/`cancel` also claim the daemon
  completion mailbox directly.
- ✅ Latest core Phase 3 storage/protocol production-review pass found no
  remaining production `unwrap`/`expect` or unused direct-crate cleanup in
  `eos-protocol`, `eos-overlay`, `eos-occ`, `eos-layerstack`, or `eosd`.
  Verification: `cargo clippy -p eos-protocol -p eos-overlay -p eos-occ -p
  eos-layerstack -p eosd --lib --bins --no-deps -- -D warnings -W
  clippy::pedantic -W clippy::nursery -D clippy::unwrap_used -D
  clippy::expect_used`; `RUSTFLAGS='-D unused-crate-dependencies' cargo check
  -p eos-protocol -p eos-overlay -p eos-occ -p eos-layerstack --lib --bins`.
  The remaining text-scan hits in that slice are test assertions and the
  intentional non-Linux overlay syscall stubs.
- ✅ Latest runtime dependency hygiene pass found no unused direct dependencies
  after the `eos-ephemeral` removal and daemon ownership split. Verification:
  `RUSTFLAGS='-D unused-crate-dependencies' cargo check -p eos-protocol -p
  eos-overlay -p eos-occ -p eos-layerstack -p eos-runner -p eos-isolated -p
  eos-ns-holder -p eos-daemon -p eosd --lib --bins`;
  `RUSTFLAGS='-D unused-crate-dependencies' cargo check -p eos-plugin --lib`;
  `cargo tree -p eos-isolated --edges normal --depth 2`; `cargo tree -p
  eos-plugin --edges normal --depth 2`; `cargo tree -p eos-daemon --edges
  normal --depth 1`; and `cargo metadata --no-deps --format-version 1`
  showing the current package set is `eos-daemon`, `eos-isolated`,
  `eos-layerstack`, `eos-ns-holder`, `eos-occ`, `eos-overlay`, `eos-plugin`,
  `eos-protocol`, `eos-runner`, `eosd`, and `xtask` with no `eos-ephemeral`
  package. Direct `eos-plugin` production clippy is also clean with
  `unwrap_used`, `expect_used`, and `undocumented_unsafe_blocks` denied; the
  follow-up contract-test cleanup below removes the prior unit-test
  `expect(...)` assertions too.
- ✅ Follow-up pure `eos-plugin` contract cleanup converted the remaining
  manifest, service-key, service-registry, PPC-frame, and op-registry tests from
  `expect(...)` assertions to fallible `Result`/`?` propagation with explicit
  typed fixture-shape errors. `eos-plugin/src` now has no `todo!()`,
  `unimplemented!()`, `panic!()`, `unwrap()`, or `expect()` text hits, and
  verification passed with `cargo test -p eos-plugin` (`18 passed`) plus
  `cargo clippy -p eos-plugin --all-targets --no-deps -- -D warnings -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks`.
- ✅ Follow-up daemon plugin process-test cleanup converted
  `sandbox/crates/eos-daemon/src/plugin/process.rs` tests and the socket-wait
  helper from `expect(...)` / timeout `assert!` paths to fallible `Result` / `?`
  propagation with typed I/O errors. Verification passed with `cargo test -p
  eos-daemon plugin::process` (`5 passed`), production daemon clippy
  (`cargo clippy -p eos-daemon --lib --bins --no-deps -- -D warnings -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks`), and a focused scan showing the only
  remaining `expect` text in `plugin/process.rs` is the intentional
  `#[expect(...)]` lint annotation.
- ✅ Follow-up daemon PPC router test cleanup converted
  `sandbox/crates/eos-daemon/src/plugin/ppc_router.rs` tests from
  `expect(...)` / `expect_err(...)` assertions to fallible server-thread
  `Result` propagation, explicit negative-result matches, and typed thread-join
  errors. Verification passed with `cargo test -p eos-daemon ppc_router` (`4
  passed`), `cargo fmt --all --check`, `git diff --check`, production daemon
  clippy, and a filtered daemon all-target strict-clippy rerun showing no
  remaining `ppc_router.rs` or `process.rs` hits.
- ✅ Follow-up daemon OCC callback test cleanup converted
  `sandbox/crates/eos-daemon/src/plugin/occ_callbacks.rs` tests and fixture
  helpers from `expect(...)` / `expect_err(...)` assertions to fallible
  `Result` / `?` propagation with explicit negative-result matches and typed
  fixture setup errors. Verification passed with `cargo test -p eos-daemon
  plugin::occ_callbacks` (`4 passed`), `cargo fmt --all --check`,
  `git diff --check`, production daemon clippy, and a filtered daemon
  all-target strict-clippy rerun showing no remaining `occ_callbacks.rs`,
  `ppc_router.rs`, or `process.rs` hits.
- ✅ Follow-up daemon plugin module test cleanup converted the large
  `sandbox/crates/eos-daemon/src/plugin/mod.rs` test block and shared PPC /
  fixture helpers from `expect(...)` assertions to fallible `Result` / `?`
  propagation, typed JSON shape helpers, fallible thread joins, fallible socket
  polling/connection helpers, and RAII reset-on-drop test isolation. Verification
  passed with `cargo test -p eos-daemon plugin::tests -- --test-threads=1`
  (`17 passed`), `cargo fmt --all --check`, `git diff --check`, and strict
  daemon all-target clippy:
  `cargo clippy -p eos-daemon --all-targets --no-deps -- -D warnings -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks`. A focused plugin-family source scan now
  finds only intentional `#[expect(...)]` lint annotations in
  `plugin/mod.rs` and `plugin/process.rs`.
- ✅ Workspace-wide Rust hygiene verification now passes after the daemon plugin
  cleanup. `cargo clippy --workspace --all-targets --no-deps -- -D warnings -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks` is green across `eos-daemon`,
  `eos-isolated`, `eos-layerstack`, `eos-ns-holder`, `eos-occ`, `eos-overlay`,
  `eos-plugin`, `eos-protocol`, `eos-runner`, `eosd`, and `xtask`; `cargo test
  --workspace --all-targets` also passed before the final `cp4s_legacy_argv`
  removal, and focused post-removal daemon/runner tests plus Linux-target
  compile checks passed afterward. A focused panic-style source scan over those
  crates now finds no live `unwrap(...)`, `expect(...)`, `expect_err(...)`,
  `panic!(...)`, `todo!(...)`, or `unimplemented!(...)` calls; the remaining
  textual matches are intentional lint attributes (`#[expect(...)]` /
  `cfg_attr(..., expect(...))`). The legacy `cp4s_legacy_argv` shell escape
  hatch is no longer present in Rust code; plugin one-shot workers now route
  argv commands through the dedicated `plugin_service` runner verb.
- ✅ Current production-surface legacy/dependency audit is clean for the
  non-plugin Rust closeout slice. `rg` over `sandbox/crates` and
  `sandbox/xtask/src` now finds no live `legacy` / `compat` / `stub` /
  `todo` / `FIXME` / `dead_code` / `allow(...)` / `cp4s_legacy_argv` /
  `eos-ephemeral` implementation hits outside intentional no-fallback
  documentation and skipped plugin deferred responses. `cargo machete` is not
  installed in this environment, so the production dependency check used
  `RUSTFLAGS='-W unused-crate-dependencies' cargo check --workspace --lib
  --bins`, which passed without unused production dependency warnings. The
  non-Linux overlay comments now describe typed `Unsupported` cfg paths instead
  of "stubs".
- ✅ Latest Rust source-debt recheck remains clean: a current scan over
  `sandbox/crates` and `sandbox/xtask/src` finds no live `todo!()`,
  `unimplemented!()`, `panic!()`, `.unwrap()`, `.expect()`, `.expect_err()`,
  or `#[allow(...)]` usage. The remaining `#[expect(...)]` attributes are narrow
  and carry explicit `reason = ...` text. Verification: `RUSTFLAGS='-D
  unused-crate-dependencies' cargo check --workspace --lib --bins` and `cargo
  clippy --workspace --all-targets --no-deps -- -D warnings -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks -D clippy::allow_attributes`.
- ✅ Current non-plugin strict best-practice pass is clean after converting the
  remaining negative-result test matches in `eos-layerstack` and `eos-runner`
  to idiomatic `let Err(...) = ... else { ... };` assertions. Verification:
  `cargo clippy -p eos-protocol -p eos-overlay -p eos-occ -p eos-layerstack -p
  eos-runner -p eos-isolated -p eos-ns-holder --all-targets --no-deps -- -D
  warnings -W clippy::pedantic -W clippy::nursery` and `cargo test -p
  eos-layerstack -p eos-runner --lib`.
- ✅ Current daemon/eosd all-target best-practice pass is clean after tightening
  PPC router negative-result tests to `let Err(...) = ... else { ... };`,
  using typed thread-join error conversion without manual `match`, and dropping
  plugin registry guards immediately after last use in test-only helper paths.
  Verification: `cargo clippy -p eos-daemon -p eosd --all-targets --no-deps
  -- -D warnings -W clippy::pedantic -W clippy::nursery`, focused
  `cargo test -p eos-daemon plugin::ppc_router --lib -- --test-threads=1`,
  focused `cargo test -p eos-daemon
  plugin::tests::status_probe_failure_drops_connected_service --lib --
  --test-threads=1`, and the workspace strict clippy gate.
- ✅ Follow-up Phase 3T PTY startup cleanup removed the production
  `clippy::too_many_lines` exceptions from `start_pty_command` and
  `start_isolated_pty_command` by splitting PTY start metadata/request
  preparation from the shared `openpty`/`ns-runner` spawn path. The remaining
  `too_many_lines` expectations in the daemon surface are integration-test
  scenarios only. Verification: host `cargo fmt --all --check`, host focused
  `cargo test -p eos-daemon pty_ --lib -- --test-threads=1` (`4 passed`),
  `cargo test -p eos-daemon --lib -- --skip plugin --test-threads=1` (`23
  passed`),
  host `cargo clippy -p eos-daemon --lib --bins --no-deps -- -D warnings -W
  clippy::too_many_lines -D clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks -D clippy::allow_attributes`, host
  pedantic/nursery `cargo clippy -p eos-daemon -p eosd --all-targets --no-deps
  -- -D warnings -W clippy::pedantic -W clippy::nursery -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks -D clippy::allow_attributes`, Linux-target
  `cargo check -p eos-daemon --target x86_64-unknown-linux-musl --lib --bins`,
  and Linux-target `cargo clippy -p eos-daemon --target
  x86_64-unknown-linux-musl --lib --bins --no-deps -- -D warnings -W
  clippy::too_many_lines -W clippy::pedantic -W clippy::nursery -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks -D clippy::allow_attributes`.
- ✅ Follow-up Phase 3T PTY finalizer cleanup removed the production
  `clippy::needless_pass_by_value` exceptions from the command module by
  representing owned background cleanup as `PtyFinalizer` /
  `IsolatedPtyFinalizer` values with consuming `finish(...)` methods. Follow-up
  OCC cleanup applied the same pattern to the single-writer worker loop with an
  owned `CommitWorker`, so the sandbox Rust source set now has no remaining
  `clippy::needless_pass_by_value` expectations.
  Verification: host `cargo fmt --all --check`, host `cargo check -p
  eos-daemon --lib --bins`, host `cargo clippy -p eos-daemon --lib --bins
  --no-deps -- -D warnings -D clippy::needless_pass_by_value -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks -D clippy::allow_attributes`, Linux-target
  `cargo check -p eos-daemon --target x86_64-unknown-linux-musl --lib --bins`,
  Linux-target `cargo clippy -p eos-daemon --target
  x86_64-unknown-linux-musl --lib --bins --no-deps -- -D warnings -W
  clippy::pedantic -W clippy::nursery -D clippy::needless_pass_by_value -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks -D clippy::allow_attributes`, focused
  `cargo test -p eos-daemon pty_ --lib -- --test-threads=1` (`4 passed`), and
  `cargo test -p eos-daemon --lib -- --skip plugin --test-threads=1` (`23
  passed`), `cargo test -p eos-occ --lib -- --test-threads=1` (`6 passed`), and
  `cargo clippy -p eos-occ --all-targets --no-deps -- -D warnings -W
  clippy::pedantic -W clippy::nursery -D clippy::needless_pass_by_value -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks -D clippy::allow_attributes`. Full
  workspace recheck also passed after the daemon/OCC ownership refactors:
  `cargo clippy --workspace --all-targets --no-deps -- -D warnings -W
  clippy::pedantic -W clippy::nursery -D clippy::needless_pass_by_value -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks -D clippy::allow_attributes`, `cargo
  clippy --workspace --target x86_64-unknown-linux-musl --lib --bins --no-deps
  -- -D warnings -W clippy::pedantic -W clippy::nursery -D
  clippy::needless_pass_by_value -D clippy::unwrap_used -D
  clippy::expect_used -D clippy::undocumented_unsafe_blocks -D
  clippy::allow_attributes`, and `cargo test --workspace --all-targets`.
- ✅ Follow-up Linux namespace setup cleanup removed the production
  `clippy::similar_names` exceptions from `eos-runner` fresh namespace setup and
  `eos-ns-holder` namespace-stack setup by capturing the pre-`unshare` parent
  IDs in a small typed value instead of paired `caller_uid` / `caller_gid`
  locals. The sandbox Rust source set now has no remaining `similar_names`
  expectations. Verification: `cargo clippy -p eos-runner -p eos-ns-holder
  --target x86_64-unknown-linux-musl --lib --bins --no-deps -- -D warnings -D
  clippy::similar_names -D clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks -D clippy::allow_attributes`, `cargo test
  -p eos-runner -p eos-ns-holder --lib -- --test-threads=1` (`12 passed`),
  `cargo clippy --workspace --all-targets --no-deps -- -D warnings -W
  clippy::pedantic -W clippy::nursery -D clippy::similar_names -D
  clippy::needless_pass_by_value -D clippy::unwrap_used -D
  clippy::expect_used -D clippy::undocumented_unsafe_blocks -D
  clippy::allow_attributes`, `cargo clippy --workspace --target
  x86_64-unknown-linux-musl --lib --bins --no-deps -- -D warnings -W
  clippy::pedantic -W clippy::nursery -D clippy::similar_names -D
  clippy::needless_pass_by_value -D clippy::unwrap_used -D
  clippy::expect_used -D clippy::undocumented_unsafe_blocks -D
  clippy::allow_attributes`, and `cargo test --workspace --all-targets`.
- ✅ Follow-up const/significant-drop cleanup removed the remaining
  `clippy::missing_const_for_fn` and `clippy::significant_drop_tightening`
  expectations from the sandbox Rust source set. Daemon/plugin non-Linux parity
  helpers are now explicit `const fn` no-op / typed-unsupported cfg arms where
  the public fallible ABI must remain shared; `isolated::with_state` keeps the
  lock guard as an immediate temporary; and `eos-ns-holder` splits Linux syscall
  helpers from non-Linux const no-op helpers for `/proc` rbind, loopback-up,
  namespace-veth config, and IPv6-default-route flush. Current source scan over
  `sandbox/crates` and `sandbox/xtask/src` finds no
  `missing_const_for_fn`, `significant_drop_tightening`, `similar_names`, or
  `needless_pass_by_value` expectations. Verification: `cargo fmt --all
  --check`, `cargo check -p eos-ns-holder --target
  x86_64-unknown-linux-musl --lib --bins`, `cargo test -p eos-ns-holder --lib
  -- --test-threads=1` (`5 passed`), host and Linux-target `cargo clippy -p
  eos-ns-holder` with `-D clippy::missing_const_for_fn`, workspace host
  `cargo clippy --workspace --all-targets --no-deps -- -D warnings -W
  clippy::pedantic -W clippy::nursery -D clippy::missing_const_for_fn -D
  clippy::significant_drop_tightening -D clippy::similar_names -D
  clippy::needless_pass_by_value -D clippy::unwrap_used -D
  clippy::expect_used -D clippy::undocumented_unsafe_blocks -D
  clippy::allow_attributes`, the same Linux-target workspace clippy gate, and
  `cargo test --workspace --all-targets`.
- ✅ Follow-up daemon integration-test cleanup removed the remaining
  `clippy::too_many_lines` expectations from
  `sandbox/crates/eos-daemon/tests/phase2_read_paths.rs` by splitting the
  workspace-base and isolated-workspace lifecycle scenarios into focused
  request/fixture/assertion helpers. The tests still cover the same wire
  contracts: workspace-base build/ensure/binding/read/reset, symlink handling,
  isolated enter/status/duplicate/list/exit/audit/reset, and daemon server
  ready probes. Verification: `cargo test -p eos-daemon --test
  phase2_read_paths -- --test-threads=1` (`10 passed`), `cargo clippy -p
  eos-daemon --test phase2_read_paths --no-deps -- -D warnings -D
  clippy::too_many_lines -D clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks -D clippy::allow_attributes`, workspace
  host clippy with `-D clippy::too_many_lines` plus the cleanup lint set,
  Linux-target workspace clippy, and `cargo test --workspace --all-targets`.
- ✅ Workspace dependency hygiene is now green for all Rust targets, not only
  production lib/bin targets. `RUSTFLAGS='-D unused-crate-dependencies' cargo
  check --workspace --all-targets` passes after documenting the unavoidable
  integration-test crate behavior with explicit underscore imports in the
  daemon phase2/phase3 integration test roots and protocol fixture test roots;
  no crate-wide `allow(...)` suppression was added. The normal package deps
  remain owned by their libraries (`eos-daemon` and `eos-protocol`), while the
  test roots now keep rustc's dependency lint meaningful under `--all-targets`.
  Rechecks also passed with host workspace pedantic/nursery clippy plus the
  denied cleanup lint set, Linux-target workspace clippy, and `cargo test
  --workspace --all-targets`.
- ✅ Follow-up overlay test cleanup removed the last source-level `.expect(...)`
  hits from the sandbox Rust tree by converting the Linux kernel-mount input
  test helpers to fallible `TestResult` / `?` propagation. Current source scans
  over `sandbox/crates` and `sandbox/xtask/src` find no live `allow(...)`,
  `todo!()`, `unimplemented!()`, `panic!()`, `.unwrap()`, `.expect()`, or
  `expect_err(...)` calls, and no remaining `too_many_lines`,
  `missing_const_for_fn`, `significant_drop_tightening`, `similar_names`, or
  `needless_pass_by_value` expectations. Verification: `cargo test -p
  eos-overlay --lib -- --test-threads=1` (`3 passed`), focused overlay clippy
  with `-D clippy::unwrap_used -D clippy::expect_used`, the all-target
  unused-dependency gate, host workspace strict clippy, Linux-target workspace
  clippy, and `cargo test --workspace --all-targets`.
- ✅ Current workspace-wide strict best-practice sweep is clean:
  `cargo clippy --workspace --all-targets --no-deps -- -D warnings -W
  clippy::pedantic -W clippy::nursery` passes across the full sandbox Rust
  workspace, including `eos-plugin` and `xtask`. This is stricter than the
  normal migration gate and confirms the remaining checked exceptions are
  explicit `#[expect(..., reason = "...")]` cases rather than broad
  suppressions.
- ✅ Current Linux-target syscall/cfg best-practice sweep is clean after
  tightening the Linux-only overlay xattr error match to the idiomatic nested
  or-pattern. Verification: `cargo clippy --workspace --target
  x86_64-unknown-linux-musl --lib --bins --no-deps -- -D warnings -W
  clippy::pedantic -W clippy::nursery -D clippy::unwrap_used -D
  clippy::expect_used -D clippy::undocumented_unsafe_blocks`, `cargo check -p
  eos-overlay --target x86_64-unknown-linux-musl --lib --tests`, `cargo test
  -p eos-overlay --lib`, and the workspace strict clippy gate.
- ✅ Current post-cleanup workspace test and arm64 target gates are green:
  `cargo test --workspace --all-targets` passed across daemon unit tests,
  phase2/phase3 integration tests, protocol fixtures, plugin contract tests,
  runner/search/setns tests, overlay capture tests, OCC, isolated, ns-holder,
  `eosd`, and `xtask`; `cargo check --workspace --target
  aarch64-unknown-linux-musl --lib --bins` passed; and `cargo clippy
  --workspace --target aarch64-unknown-linux-musl --lib --bins --no-deps -- -D
  warnings -D clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks` passed.
- ✅ Latest unsafe/FFI audit pass confirms unsafe remains confined to syscall
  crates and documented raw libc gaps. Verification: source inventory over
  `eos-overlay`, `eos-runner`, `eos-ns-holder`, `eos-isolated`, `eos-daemon`,
  and `eosd`; `cargo clippy -p eos-overlay -p eos-runner -p eos-ns-holder -p
  eos-isolated -p eos-daemon -p eosd --target x86_64-unknown-linux-musl --lib
  --bins --no-deps -- -D warnings -D clippy::undocumented_unsafe_blocks`.
  Follow-up inspection covered overlay xattr FFI, runner `setns(2)`, holder
  netlink socket/send/close, struct-byte views, pipe read/write, owned-FD
  conversion, PID-namespace fork/init, and signal-handler `_exit`. Non-syscall
  crates (`eos-protocol`, `eos-layerstack`, `eos-occ`, `eos-isolated`,
  `eos-daemon`, `eos-plugin`, `eosd`) forbid unsafe code.
- ✅ Latest current-checkout production integration gate is clean across the
  Rust workspace: `cargo clippy --workspace --lib --bins --no-deps -- -D
  warnings -D clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks` and `cargo fmt --all --check` both
  passed. This covers production libraries/binaries only; test targets still use
  intentional `expect(...)` assertions.
- ✅ Latest non-plugin Rust behavior test slice passed without using
  TaskCenter-runner paths: `cargo test -p eos-protocol -p eos-overlay -p
  eos-occ -p eos-layerstack -p eos-runner -p eos-isolated -p eos-ns-holder
  --lib --bins` (`55 passed`); `cargo test -p eos-daemon --lib -- --skip
  plugin` (`23 passed`, `27 filtered out`); `cargo test -p eos-daemon --test
  phase2_read_paths` (`10 passed`); and `cargo test -p eos-daemon --test
  phase3_write_paths` (`5 passed`). This covers protocol fixtures, overlay
  capture, OCC batching/CAS retry, LayerStack squash/lease/delete behavior,
  runner glob/grep/setns request shape, isolated ResourceCaps admission helpers,
  ns-holder handshake parsing, daemon command/audit/in-flight/OCC/isolated
  unit paths, and daemon read/write integration paths. Plugin implementation
  tests remain intentionally outside this slice.
- ✅ The Phase 3T handoff/design docs no longer point the Rust closeout at the
  `task_center_runner` harness. Current model-facing/live PTY guidance routes
  through focused engine/tool unit tests plus
  `backend/scripts/bench_rust_daemon_phase3t_pty.py`, while isolated live proof
  remains under `backend/scripts/bench_rust_daemon_isolated_inspection.py`.
- ✅ Latest CLI/PPC visibility cleanup passed: `eosd` no longer carries the
  unused `Peekable` wrappers, `map(...).unwrap_or(...)`, stale Python-backed
  launcher wording, or `spawn_daemon` by-value lint in the focused
  pedantic/nursery slice; daemon PPC helper/process items are scoped to the
  plugin parent module instead of crate-wide visibility, while the private
  command/isolated/plugin dispatcher modules follow Clippy's `pub`-inside-
  private-module convention and daemon docs satisfy the focused markdown lint.
  The daemon plugin callback module now has the explicit `serde` dependency it
  uses, a non-exhaustive OCC status fallback, and warning-clean imports.
  `api.plugin.ensure` now clones the service specs under the plugin registry
  lock, drops that lock while spawning the service process and waiting for the
  PPC socket connection, then re-locks only to publish still-declared processes;
  stale or duplicate starts are dropped through RAII teardown instead of being
  inserted into the registry.
  The follow-up daemon plugin pedantic pass also shortens plugin registry guard
  lifetimes before JSON response construction, uses `clone_from` for registered
  op status refresh, keeps deferred/registered route dispatch borrowed instead
  of by-value, preserves the non-exhaustive OCC status fallback without a
  duplicate match arm, keeps the non-Linux process-group stub const, and borrows
  refresh requests through the PPC send path instead of cloning/moving request
  bodies unnecessarily.
  Focused recheck: `cargo check -p eos-daemon`, `cargo test -p eos-daemon
  plugin --lib` (`27 passed`), focused daemon pedantic/nursery clippy with no
  remaining `plugin/mod.rs`, `plugin/occ_callbacks.rs`, or `plugin/process.rs`
  findings, `cargo fmt --all --check`, and `git diff --check`.
  Verification: `cargo clippy -p eosd --bin eosd --no-deps -- -W
  clippy::pedantic -W clippy::nursery`, `cargo clippy -p eos-daemon -p eosd
  --lib --bins --no-deps -- -D warnings`, `cargo clippy -p eos-daemon --lib
  --no-deps -- -W clippy::redundant_pub_crate -W clippy::doc_markdown`,
  `cargo test -p eos-daemon plugin` (`23 passed`), `cargo test -p eos-daemon
  isolated_workspace --test phase2_read_paths` (`3 passed`), `cargo test -p
  eos-daemon active_pty_records_block_exit_until_cleared` (`1 passed`),
  `cargo test -p eosd` (`0 tests`), `cargo fmt --all --check`, and
  `git diff --check`.
- ✅ `cargo check --workspace` green (10 crates + `xtask`, 11 packages) · `cargo clippy --workspace --all-targets` clean · `cargo fmt --all --check` clean · Linux-target syscall subset check green for `x86_64-unknown-linux-musl` · `cargo clippy --workspace --target x86_64-unknown-linux-musl --lib --bins` clean. The Phase 3/3T/3.5 Rust cleanup removed the stale unused/dead-code skeleton warnings from `eos-daemon`, the old `eos-plugin` standalone warm-server/dispatch scaffold, and the old `eos-ephemeral` crate; `eos-ephemeral`'s final overlay-error wrapper is now folded into daemon-local `DaemonError::OverlayPipeline`. Linux PTY command wrappers use idiomatic tail expressions, protocol tests no longer rely on `unwrap()`, all Rust workspace packages now inherit the explicit MSRV `rust-version = "1.85"`, and the current all-target unused-dependency gate is green with explicit test-root underscore imports rather than crate-wide suppressions.
- ✅ `xtask package` implemented for `eosd-linux-{amd64,arm64}`: default builder is `rust-lld` (`cargo` with `RUSTFLAGS=-C linker=rust-lld`), with optional `cargo`/`cross`; writes binary-only `SHA256SUMS`, `protocol_version`, per-artifact JSON manifests, and optional minisign `.minisig` signatures. Latest local amd64 plugin artifact SHA is `f200673fd47526257e5ea0f2172526702cfb7d7800a0158deb7fdc60beaf9d5e`; prior arm64 package SHA is `e07a59546cecf931922386a91bf08a8ee5e1fa08747cbc45ee56462eeac4417b`.
- ✅ **Build-time guarantee holds**: current `cargo metadata --no-deps
  --format-version 1` lists `eos-daemon`, `eos-isolated`, `eos-layerstack`,
  `eos-ns-holder`, `eos-occ`, `eos-overlay`, `eos-plugin`, `eos-protocol`,
  `eos-runner`, `eosd`, and `xtask`, with no `eos-ephemeral` package. Current
  `cargo tree -p eos-isolated --edges normal --depth 10` and the same command
  with `--target x86_64-unknown-linux-musl` have no `eos-occ` edge; the Linux
  graph adds only the target-gated netlink/syscall helpers and still no
  OCC/publish edge. Current `cargo tree -p eos-plugin --edges normal --depth
  10` on host and Linux remains contract/framing-only with no `eos-occ`,
  `eos-overlay`, `eos-layerstack`, `nix`, or `tokio` edge; the live daemon
  dispatcher owns the concrete per-root OCC service cache and single-writer
  publish path.

**Contracts & fixtures (ground truth)**
- ✅ `sandbox/docs/contract/01-06.md` — source-verified wire/CAS/audit/models/provider/crate-map specs.
- ✅ `sandbox/docs/contract/06-crate-map-and-invariants.md` now distinguishes
  the frozen 2026-05-31 Python-source dependency evidence from the current
  2026-06-01 Rust direct Cargo graph. The refreshed map records the intentionally
  severed `eos-isolated` runtime-child/layerstack/protocol edges, the
  daemon-injected isolated snapshot/lease port, the removed `eos-ephemeral`
  crate boundary, the current no-`eos-occ`/no-`eos-overlay` plugin graph,
  and the daemon-spawned holder/runner subcommand boundary.
- ✅ `sandbox/crates/eos-protocol/fixtures/` — 18 CAS cases + envelope/audit/metrics fixtures (executed from real Python).
- ✅ `sandbox/docs/RUST-GUIDANCE.md` — the Rust standard for all builders (incl. exact `ensure_ascii` escaper spec).
  The refreshed async/syscall boundary now matches the current crate graph:
  `tokio` is allowed in `eos-daemon`/`eosd`, Linux-target `eos-isolated`
  netlink helpers may use target-gated `tokio`, `eos-runner`/`eos-ns-holder`
  remain single-threaded syscall children, and implemented non-Linux cfg parity
  arms should be typed unsupported/no-op paths rather than `todo!()` bodies.
  Callable deferred ports are documented as typed deferred/unsupported errors
  with `// PORT` anchors, not panic placeholders.
- ✅ Unsafe remains confined to the syscall crates (`eos-runner`, `eos-ns-holder`,
  and `eos-overlay`), and the workspace now denies
  `clippy::undocumented_unsafe_blocks` so the existing `// SAFETY:` discipline
  is compiler-checked rather than doc-only. Recheck coverage: `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo clippy -p eos-runner -p
  eos-ns-holder -p eos-overlay --target x86_64-unknown-linux-musl --lib --bins
  --no-deps -- -D warnings`, `cargo clippy -p eos-daemon -p eos-isolated -p
  eosd --target x86_64-unknown-linux-musl --lib --bins --no-deps -- -D
  warnings`, and `RUSTFLAGS='-W unused-crate-dependencies' cargo check
  --workspace --lib --bins`; `cargo test --workspace --all-targets` remains
  green after the lint change.
- ✅ Production lint exceptions for the Phase 3/3T/3.5 Rust cleanup path now
  use checked `#[expect(..., reason = "...")]` attributes instead of broad
  `#[allow(...)]` suppressions for dispatcher ABI parity, non-Linux/test cfg
  parity, Serde predicate ABI parity, and Linux kernel `repr(C)` field names.
  `rg '#\[(cfg_attr\([^\n]+allow|allow)\(' sandbox/crates -g
  '*.rs'` now returns no Rust crate suppressions. Targeted recheck: `cargo fmt
  --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo clippy -p eos-protocol --all-targets --no-deps -- -D warnings -W
  clippy::pedantic -W clippy::nursery`, Linux-target daemon/isolated/eosd
  clippy, Linux-target syscall crate clippy, `cargo test -p eos-protocol
  --all-targets`, and `cargo test -p eos-daemon -p eos-occ -p eos-overlay -p
  eos-ns-holder --all-targets`.
- ✅ Follow-up daemon isolated cfg cleanup removed the remaining production
  `dead_code` expectations from `eos-daemon/src/isolated.rs` and
  `eos-daemon/src/command.rs`: Linux-only command handles, isolated PTY
  register/unregister, and isolated audit helpers are now cfg-gated at their
  real runtime boundary, while non-Linux `exec_command` still fails closed when
  an active isolated handle exists. Verification: `cargo test -p eos-daemon
  --lib -- --skip plugin`, host and `x86_64-unknown-linux-musl`
  `cargo clippy -p eos-daemon --lib --bins --no-deps -- -D warnings -D
  clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks`,
  `RUSTFLAGS='-D unused-crate-dependencies' cargo check -p eos-daemon --lib
  --bins`, `cargo test -p eos-daemon --test phase2_read_paths`, and
  `cargo test -p eos-daemon --test phase3_write_paths`.
- ✅ Follow-up runner test-lint cleanup converted the setns and glob/grep
  primitive tests to return `Result` and use explicit error matches instead of
  `expect` / `expect_err`. `cargo clippy -p eos-runner --all-targets --no-deps
  -- -D warnings -D clippy::unwrap_used -D clippy::expect_used -D
  clippy::undocumented_unsafe_blocks` now passes, and `cargo test -p
  eos-runner` remains green (`7 passed`).
- ✅ Follow-up `eos-runner` command-shape test cleanup converted Linux-cfg
  `fresh_ns` shell/raw-argv/plugin-service command tests to `Result` tests with
  explicit success/error matches instead of `expect` / `expect_err`. Verification
  passed with `cargo test -p eos-runner` (`7 passed`), strict host
  `eos-runner` all-target clippy with `unwrap_used`, `expect_used`, and
  `undocumented_unsafe_blocks` denied, `x86_64-unknown-linux-musl` lib/tests
  `cargo check`, `cargo fmt --all --check`, and focused runner
  `panic!/expect/unwrap` source scans.

**Python-side Phase 0 (surgical; focused sandbox tests passed)**
- ✅ `put_archive` on `ProviderAdapter` Protocol + Docker adapter (async → `container.put_archive`) + Daytona stub.
- ✅ `backend/src/sandbox/host/runtime_artifact/__init__.py` pins the local artifacts: `EOSD_VERSION=0.1.0-local.20260601`, amd64 SHA256 `81eb221542666647a3b0a80a0ed254dff674a0ead27d814bfcea26bd14996d53`, arm64 SHA256 `e07a59546cecf931922386a91bf08a8ee5e1fa08747cbc45ee56462eeac4417b`, protocol version `1`. Minisign remains empty until the later release-provenance gate.
- ✅ `backend/src/sandbox/_contract_fixtures/` vendors the Rust fixtures; `pin.json` is hard-pinned to `2df20649b3158324d1be9c4c6c53a5844034ebc2` with `fixtures_sha256=3d62ff3017bf1b1a76e36de08ea4a3185d9640cb9ca98f7e4a1796b153aab221`; the backend pin assert is hard-fail (no skip).
- ✅ `EOS_SANDBOX_RUNTIME=python|rust` no-op host read exists in `daemon_client.py` and validates values; the actual dispatch fork remains Phase 2.
- ✅ `backend/scripts/bench_sandbox_e2e.py` has Docker-backed Phase 0 mode for CP-0 + CP-1 (`--phase0`) plus local artifact upload verification (`--eosd-binary`) that uses `put_archive`, Docker archive readback, and direct binary exec. `backend/scripts/build_upload_eosd_docker.py` is the narrower build/package/upload script for both arches. Neither path installs `apt`/`pkg` packages or requires Rust/Cargo inside the target sandbox image for the artifact check.
- ✅ GitHub CI is **not** part of the current Phase 0 closeout path. The current path is: build/package locally, then upload the static binary into the sandbox/container.

**Phase 0 CP baseline artifacts**
- ✅ `bench/baseline-amd64.json` captured in `sweevo-dask__dask-10042:latest` (Ubuntu 22.04.4, Python 3.10.14, kernel `6.10.14-linuxkit`, `x86_64`, `/eos-mount-scratch` tmpfs, overlay-in-userns probe green).
- ✅ CP-0 measured: runtime bundle upload `4092.846 ms`; daemon cold-start `885.234 ms`; daemon idle RSS `36,676 KiB`; Python process-start p50 `428.128 ms`; warm heartbeat p50 `1.103 ms`, p95 `1.993 ms`.
- ✅ CP-1 passed: `put_archive` vs base64-over-exec for `1.5 MiB` (`17.260 ms` vs `23,003.217 ms`, 64 chunks) and `3.0 MiB` (`32.196 ms` vs `45,602.537 ms`, 128 chunks); all SHA256s matched; put-archive size ratio `1.865` ≤ `2.5`.
- ✅ `bench/local-eosd-amd64-upload.json` captured the historical Phase 0 bootstrap amd64 handoff: `sandbox/dist/eosd-linux-amd64` (683,328 bytes, static PIE) uploaded to `/tmp/eosd-local/eosd` in `8.121 ms`; readback SHA256 matched `c81993538d4cfb6425e1a00f91d38d0a85dd07a1706907c3b07db6faf5a5629e`; mode `0755`; direct exec returned `eosd 0.1.0`; target `rustc`/`cargo` absent. Current Phase 1 amd64 artifact verification is `bench/phase1-ns-runner-amd64.json`.
- ✅ `bench/local-eosd-arm64-upload.json` captured the historical Phase 0 bootstrap arm64 handoff: `sandbox/dist/eosd-linux-arm64` (597,848 bytes, static aarch64 ELF) uploaded to `/tmp/eosd-local/eosd` in `8.444 ms`; readback SHA256 matched `6edbe7bdc7bb4d6414b2b331d58857b1ce55bcf61bd391f34f34b36bdba716c6`; mode `0755`; direct exec returned `eosd 0.1.0`; target `rustc`/`cargo` absent. Current arm64 artifact is rebuilt and pinned but not re-upload-smoked in this dask-only pass.

**Phase 1 implementation artifacts (local, 2026-05-31)**
- ✅ `eos-overlay::kernel_mount` now validates `O_DIRECTORY|O_NOFOLLOW` inputs, pins lower/upper/work dirs through `/proc/self/fd/*`, calls the raw `fsopen→fsconfig(lowerdir+)→fsconfig(upperdir/workdir)→fsmount→move_mount` sequence, and tears down stacked mounts via RAII drop.
- ✅ `eos-overlay::writable_dirs` now creates the canonical `/eos-mount-scratch/eos-sandbox-runtime` root and per-run `upper`/`work` dirs.
- ✅ `eos-runner` fresh-ns mode now performs best-effort `setsid` (Docker exec may already be process-group leader), `unshare(NEWUSER|NEWNS)`, root uid/gid map setup, private mount propagation, overlay mount guard acquisition, shell command execution with cwd/env policy, timeout kill, and `RunResult` JSON construction. Fast-child wait polling is `5 ms` to avoid an avoidable 100 ms floor.
- ✅ `eosd ns-runner` now reads a `RunRequest` from stdin, `--request PATH`, or one positional request path; writes compact JSON to stdout or `--output PATH`; and wires the runner to the `eos-overlay` mount adapter.
- ✅ Compile/lint checks cover both host and Linux syscall cfg surfaces: host `cargo check --workspace`, host targeted tests, `x86_64-unknown-linux-musl` targeted check, and Linux-target clippy for `eos-overlay`, `eos-runner`, and `eosd`.
- ✅ `bench/phase1-ns-runner-amd64.json` captured direct `eosd ns-runner` in `sweevo-dask__dask-10042:latest` with artifact SHA `f374662b28337575aafb65995c7c3626e4731fc9464cb4ac24bc45ab262acefe`: AV shell smoke green (`hello.txt` read from lower, `generated.txt` captured in upper), timeout cleanup green (non-zero timeout, no lingering `sleep`, no parent-namespace `/testbed` mount leak), and 20/20 perf samples green.
- ✅ CP-2b direct-runner host-wall comparison passed: Rust fresh-ns `true` p50 `361.567 ms`, p95 `373.759 ms` vs refreshed CP-0 Python process-start p50 `428.128 ms` in the same dask image. This is the apples-to-apples direct-runner number: `66.562 ms` faster p50, `15.5%` latency reduction, `1.184×` speedup.
- ✅ CP-2a measured Rust mount-init path passed the ≥20× bar: `workspace.mount_s` p50 `1.076 ms` (`397.8×` faster than CP-0 Python process-start p50). This `397.8×` figure is intentionally **not** an end-to-end tool-call claim: it compares raw Rust/kernel overlay mount initialization (`fsopen→fsconfig→fsmount→move_mount`, no workspace copy) against Python process startup (`python3 -c pass`) in the dask container.
- ✅ Bottleneck interpretation recorded: network is not the main delay in this local dask run. Direct runner host-wall p50 is `361.567 ms`; internal `mount+tool` p50 is `319.288 ms`; raw mount p50 is `1.076 ms`; implied host/Docker/request overhead is about `42.279 ms`. The first optimization split removed the Python/wrapper shell-string cost from the low-level daemon primitive; the chosen model-facing shell engine is now the container's native `/bin/bash` plus PTY, measured separately below.

**Phase 2 implementation artifacts (local, 2026-05-31)**
- ✅ `eos-daemon` now has a real Phase 2 AF_UNIX + Docker-published TCP server: newline-delimited JSON framing, request-size/read-time handling, TCP auth-token stripping, structured error envelopes, `api.runtime.ready`, `api.v1.heartbeat`, `api.layer_metrics`, audit pull/snapshot/reset-floor stubs, and direct `api.v1.read_file` / `api.read_file` LayerStack reads.
- ✅ `eos-layerstack` now has read-side manifest loading, workspace binding translation, merged newest-first read semantics with whiteout/opaque ancestor handling, O(1) snapshot lease plumbing, a process-local dual-layer storage writer lease, and active-lease metrics needed by readiness/layer metrics.
- ✅ `eosd daemon` now starts the Rust daemon, supports `--spawn` for host recovery launches, and supports `--client SOCKET JSON` as the Rust AF_UNIX thin-client replacement preserving the 97/98 connect/I/O exit-code contract.
- ✅ `backend/src/sandbox/host/daemon_client.py` now selects Rust spawn/client commands when `EOS_SANDBOX_RUNTIME=rust`, while Python remains the default. Rust daemon TCP binds `0.0.0.0` inside Docker so the provider's host-loopback port mapping works; stale TCP empty-response/connect-failure paths invalidate the cached endpoint before respawn.
- ✅ Local verification: `.venv/bin/python -m pytest backend/tests/unit_test -q`; `cargo test --workspace`; `cargo check --workspace`; `cargo fmt --all --check`; `cargo clippy -p eos-layerstack -p eos-daemon -p eosd --all-targets`; focused daemon transport/API tests; `.venv/bin/python -m ruff check` and `py_compile` for the Phase 2 harness.
- ✅ Live Docker/dask evidence: `bench/phase2-rust-daemon-amd64.json` uploaded pinned amd64 `eosd` SHA `59c0ae7bc655ba55f59e9d4e228e33340fd6125238d9fc8f4ea1961fd395c7a4` into `sweevo-dask__dask-10042:latest`, launched with `EOS_SANDBOX_RUNTIME=rust`, and closed CP-3/AV-2. Rust daemon spawn was `367.015 ms` vs CP-0 Python `885.234 ms`; idle RSS was `4,112 KiB` vs CP-0 `36,676 KiB`; readiness after spawn was `9.760 ms`; warm TCP heartbeat p50/p95 was `1.173/1.444 ms`. AF_UNIX and TCP both proved `api.runtime.ready`, `api.read_file`/`api.v1.read_file`, `api.v1.heartbeat`, and `api.layer_metrics`. AV-2 killed pid `295`, respawned pid `424`, observed stale TCP `EOS_DAEMON_IO_FAILED:empty_response`, invalidated then repopulated the TCP endpoint cache, left exactly one `eosd daemon` process, and reported zero `eos-sandbox-runtime` mount entries.

**Phase 3 implementation artifacts (closed structural direct write/edit + overlay shell/search slice, 2026-05-31/2026-06-01)**
- ✅ `eos-layerstack` now has a policy-blind immutable layer publish primitive: aggregate accepted changes, compute the AV-1c `layer_digest`, skip duplicate head-layer writes, write layer bytes/whiteouts/symlinks/opaque markers, persist `.layer-metadata/*.digest`, and atomically temp-rename the active manifest. It also implements merged projection, checkpoint squash planning/build/relabel/rollback, manifest-prefix CAS checks, and lease-release GC that retains leased layers until the final lease drops.
- ✅ `eos-daemon` now registers `api.write_file` / `api.v1.write_file` and `api.edit_file` / `api.v1.edit_file` on the Rust op table. The handlers translate workspace-bound paths through `workspace.json`, preserve create-only and edit-anchor guards, then publish direct writes/edits through a per-root `eos_occ::OccService<LayerStackCommitTransaction>` single-writer queue. Responses now expose OCC status/timings while retaining the guarded Python-compatible result shape.
- ✅ `eos-occ::CommitQueue` now has the named single worker, close/drain, submit reply channels, disjoint non-atomic batching, atomic batch isolation, and bounded CAS retry exhaustion to `aborted_version`; `OccService` prepares `.git` drops, DIRECT routes root `.gitignore` matches, attaches GATED base hashes, and routes publishable changes through the queue. The daemon transaction bridge revalidates GATED hashes against current LayerStack bytes, rejects unsupported gated symlinks, drops all accepted paths on atomic validation failure, maps accepted DIRECT/GATED changes into `LayerStack::publish_layer`, and returns published manifest versions.
- ✅ `eos-overlay::capture_upperdir` now captures upperdir regular-file writes, whiteout deletes, symlinks, and opaque directory markers/xattrs into validated `OverlayPathChange`s. `eos-occ` now consumes the real `eos_overlay::OverlayPathChange` one-way edge and converts it into `LayerChange`s with OCC-owned error wrapping.
- ✅ `eos-daemon` now registers `api.v1.shell`, `api.glob` / `api.v1.glob`, and `api.grep` / `api.v1.grep`. The current daemon shell primitive acquires a LayerStack snapshot lease, allocates overlay upper/work dirs, accepts only the public shell-format command string wire shape, rejects raw argv, runs `eosd ns-runner`, captures upperdir changes, computes snapshot base hashes, publishes through OCC, and returns runner stdout/stderr/exit fields plus overlay timing aliases. CP-4s raw-argv results remain historical structural smoke evidence only; Phase 3T's `exec_command` contract rejects raw argv and CP-4 throughput/contention does not use raw argv. Glob/grep acquire the same read-only overlay lease and execute in-namespace Rust primitives via the runner without OCC publish.
- ✅ `eos-runner` now supports fresh-ns `glob` and `grep` in addition to `shell`. The Rust primitives preserve the documented wire shape for sorted/sliced glob results, read-only grep filenames/content/count modes, regex flags, UTF-8/2 MiB skip behavior, inert `head_limit`/`offset`, and workspace escape rejection.
- ✅ `eos-daemon` now registers `api.v1.cancel`, `api.v1.heartbeat`, and `api.v1.inflight_count` against the server-owned `InFlightRegistry`. Server dispatch registers invocation id / agent id / background flag around handler execution, runs a TTL sweep loop, and the registry tracks active-call guards so stale background entries are not TTL-reaped while a call is active.
- 🟡 `eos-plugin` now has concrete PPC frame encode/decode over the shared `eos_protocol` newline-delimited request envelope plus pure manifest/service/refresh/op-name contracts. The old standalone warm-server/dispatch/context scaffold was removed; `eos-daemon` owns service process lifetime, route execution, OCC callbacks, refresh gating, restart fallback, and oneshot overlay execution. The plugin crate graph no longer reaches `eos-occ`, `eos-overlay`, `eos-layerstack`, `nix`, or `tokio`, and the old `eos-ephemeral` Rust crate has been removed from the workspace. `eos-daemon` now derives `/eos/plugin/ppc/*.sock` service process specs, can bind/accept the per-service PPC socket, can spawn declared service commands with the PPC harness environment via `start_services: true`, retains per-service snapshot leases, refreshes stale connected read-only services before dispatch, remounts stale service namespaces in place, restarts stale read-only services when the manifest strategy is `restart_service`, forbids plugin-operation serialization by multiplexing same-service concurrent read-only and self-managed dispatches on the shared client with message-id routed out-of-order replies, drops broken PPC streams from connected-route status, services plugin-originated callback request frames before the final operation reply, routes concurrent connected self-managed `daemon.occ.apply_changeset` callbacks by parent message id through the daemon OCC writer, runs `oneshot_overlay` WRITE_ALLOWED workers through a fresh overlay namespace plus OCC publish, and has live Rust-runtime generic PPC/OCC/repeated-callback/concurrent-runtime-bridge-delay/concurrent-runtime-bridge-apply/refresh/status-health/failed-health/failed-health-recovery/recovery/remount-read/package-adapter/Pyright-read/Pyright-workspaceSymbol/Pyright-completion/Pyright-completionResolve/Pyright-diagnostics/Pyright-codeActions/Pyright-signatureHelp/Pyright-hover/Pyright-typeDefinition/Pyright-declaration/Pyright-callHierarchy-incoming-outgoing/Pyright-documentHighlight/Pyright-prepareRename/Pyright-definition/Pyright-references/Pyright-rename/LSP-applyWorkspaceEdit/LSP-applyCodeAction/LSP-formatDocument/LSP-executeCommand/Pyright-negative-format-execute-capability/restart/crash/timeout/oneshot-overlay coverage. Broader AV-10 LSP parity remains open beyond the representative `documentSymbol` + `workspace/symbol` + `completion` + `completionItem/resolve` + `publishDiagnostics` + `codeAction` + `signatureHelp` + `hover` + `typeDefinition` + `declaration` + `callHierarchy` incoming/outgoing + `documentHighlight` + `prepareRename` + `definition` + `references` + self-managed `rename` + `apply_workspace_edit` + `apply_code_action` + `format_document` + `execute_command` path.
- ✅ `backend/scripts/bench_rust_daemon_phase3.py` now targets the current public shell-string daemon boundary: upload/seed/start Rust daemon through the repo Docker provider path, seed the LayerStack base layer from the image's real `/testbed` workspace, measure `api.v1.shell` no-op via `"command": "true"`, shell-string small-write publish via `"command": "touch <file>"`, `api.v1.glob`, `api.v1.grep`, 1/3/5/10 concurrent shell-string load waves, per-sample phase timings, final small-write readback hash, and daemon memory samples from `/proc/<pid>/smaps_rollup` with RSS fallback. The harness is target-explicit: `--arch` chooses the artifact, default report, and baseline paths; `--docker-platform` is forwarded into Docker creation; artifact metadata, baseline `uname -m`, and container `uname -m` must match before the daemon run starts. Historical `bench/phase3-rust-daemon-amd64.json` captured the retired raw-argv CP-4s evidence in `sweevo-dask__dask-10042:latest` with `--arch amd64 --docker-platform linux/amd64`, run `local-f1bd63a4b0f3`, artifact SHA `5fe3da1c879b9db0ad1776f39b4c2fdfe988de04c9d28772a1d085ad53d40f26`: raw-argv `["true"]` passed (`host-wall` p50/p95 `31.787/32.628 ms`; `command_exec.run_command_s` p50/p95 `16.109/16.498 ms` vs required p50 `<=95.468 ms`; mount p95 `1.409 ms`; host-minus-api p95 `2.369 ms`). The old 1/3/5/10 structural load matrix also passed: no-op host p95 `34.650/37.024/69.151/109.333 ms`, unique `touch` write host p95 `33.940/41.839/74.701/152.729 ms`, all well below Phase 1 host p95 `373.759 ms`; peak daemon PSS/RSS was `31,828/32,420 KiB`, still below the CP-0 Python idle RSS baseline `36,676 KiB`, with idle-return gate true via the Rosetta active-peak ceiling because the amd64 image runs under `/run/rosetta/rosetta` on this host. The repo Docker provider path is the intended route; the `docker` CLI does not need to be present for this benchmark.
- ✅ Native container Bash/PTY viability was measured in the same Dask image family before implementation lock-in. Non-overlay process microbenchmarks remain diagnostic only; overlay-inclusive Bash/PTY measurements remain historical Phase 3T design evidence, superseded for closeout by the implemented `exec_command`, `write_pty_command_stdin`, `check_pty_command_progress`, and `cancel_pty_command` tools. Existing evidence includes `bench/phase3-overlay-bash-microbench-amd64.json` and `bench/phase3-overlay-pty-bash-microbench-amd64.json`: raw argv `true` host p50/p95 `31.918/34.731 ms`, Bash `--noprofile --norc -c true` host p50/p95 `43.940/45.100 ms`, Bash `--noprofile --norc -i -c true` host p50/p95 `43.801/46.312 ms`, Bash write+publish host p50/p95 `43.329/47.303 ms`, and `script(1)` PTY-proxy Bash host p50/p95 `79.735/83.213 ms` (`81.413/88.942 ms` for `-i -c`). Those historical samples went through `api.v1.shell` with LayerStack snapshot lease, overlay mount, capture, OCC publish/cleanup, and release. The PTY-proxy run is conservative because Rust `openpty` session management was not implemented at the time and `script(1)` adds wrapper overhead.
- ✅ `sandbox/crates/eos-daemon/tests/phase3_write_paths.rs` covers Rust daemon direct write publish + readback, create-only existing-file conflict, edit publish + readback, duplicate-head idempotency, and `.git` path route-dropping. Daemon unit tests cover shell string validation, raw-argv rejection, GATED stale-base abort, DIRECT stale-base publish, atomic validation failure drop, root `.gitignore` routing, in-flight TTL active-call protection, and cancel/heartbeat/count control ops. `eos-runner` unit tests cover glob/grep primitive contract slices plus the dedicated `plugin_service` argv runner path for plugin workers. `eos-layerstack` unit tests cover squash read preservation, same-manager lease-retained GC, and cross-instance reopened lease retention/release. `eos-plugin` unit tests cover PPC frame round-trip/reject paths, registration/public-op helpers, manifest validation, refresh ack/request behavior, service key validation, and service registry state. `eos-occ` unit tests cover batching, atomic isolation, CAS retry success/exhaustion, and overlay-change conversion. `eos-overlay` unit tests cover upperdir file/delete/symlink/opaque capture. Local checks passed: `cargo fmt --all`; `cargo check -p eos-occ -p eos-overlay -p eos-layerstack -p eos-daemon -p eos-runner`; `cargo test -p eos-runner --lib`; `cargo test -p eos-daemon`; `cargo test -p eos-occ -p eos-overlay -p eos-layerstack`; `cargo check -p eos-plugin -p eos-daemon`; `cargo test -p eos-plugin -p eos-daemon`; `cargo clippy -p eos-occ -p eos-overlay -p eos-layerstack -p eos-daemon -p eos-runner --all-targets`; `cargo clippy -p eos-plugin -p eos-daemon --all-targets`; focused `cargo test -p eos-plugin`; focused `cargo clippy -p eos-plugin --all-targets`; focused `cargo test -p eos-daemon shell_command`; focused `cargo check -p eos-daemon`; Phase 3 harness `py_compile` + `ruff check` (only pre-existing adjacent skeleton warnings).
- ✅ Phase 3 closeout boundary: CP-4s raw-argv structural performance/load evidence is green, direct write/edit publish and shell/search overlay paths have focused Rust coverage, background active-call TTL protection is covered, and PPC framing/contract scaffolding is covered. Deferred to Phase 3T: full `exec_command`/`write_pty_command_stdin` implementation, CP-4t proof, CP-4/CP-5 proof, AV-3 process-tree cleanup under live shell/background load, daemon-owned plugin process dispatch/self-managed callback, AV-7 forward/back on-disk parity, AV-10 plugin parity, and the §7 differential/property contention gates.

**Phase 3T CP-4t closeout artifacts (Docker shared workspace, 2026-06-01)**
- ✅ Model-facing command tools now use the final names: `exec_command`, `write_pty_command_stdin`, `check_pty_command_progress`, and `cancel_pty_command`. Rust daemon ops are registered for `api.v1.exec_command`, PTY controls, and the completion collector. The public `exec_command` boundary accepts a shell-format `cmd` string and rejects raw argv; raw-argv evidence remains historical CP-4s structural evidence only.
- ✅ CP-4t artifact of record: `bench/phase3t-pty-command-docker-20260601-current-eos-paths-post-notify.json` passed with runtime upload to `/eos/daemon/eosd`, `layer_stack_root=/eos/layer-stack`, workspace root `/testbed`, and correctness gates for stdout/stderr split, explicit Dask command PATH, finite write publish/readback, finite `tty=false` descendant cleanup, and PTY `tty=true` descendant cleanup. The current rerun (`run_id=local-7b9deab71f9f`, artifact SHA `81eb221542666647a3b0a80a0ed254dff674a0ead27d814bfcea26bd14996d53`) kept the gate green with 50/50 operation samples, `operation_samples_ok=true`, and `load.gate_pass=true`.
- ✅ Later timeout/cancel verification superseded the post-notify runtime hash: `bench/phase3t-pty-command-docker-20260601-current-eos-paths-timeout-cancel-fix.json` passed with amd64 SHA `cb949fce52784b6f7634589a707f54f40f01f75051bc7259832bc2fee63c54bf`. Operation p95s were finite `exec_command(tty=false)` `43.047 ms`, `exec_command(tty=true)` `48.337 ms`, `check_pty_command_progress` `1.781 ms`, `write_pty_command_stdin` `53.733 ms`, `cancel_pty_command` `55.796 ms`, and cancel cleanup `381.024 ms`.
- ✅ Full tiered Docker summaries passed for both the sidecar-minimum Rust scratch run `.omc/results/progressive-test-summary-phase3t-rust-scratch-full-final-20260601.jsonl` and the later `/eos` timeout/cancel run `.omc/results/progressive-test-summary-phase3t-current-eos-paths-timeout-cancel-fix-tier0-6-20260601.jsonl`; tiers 0-6 all reported `status=passed` with `failed_cells=0`.
- ✅ The accepted CP-4t samples go through the shared workspace overlay path: LayerStack workspace-base/binding on `/eos/layer-stack`, overlay command execution in `/testbed`, capture, OCC publish or discard, cleanup, and lease release. No model-facing raw-argv performance gate remains for Phase 3T.
- ✅ Deferred non-plugin CP-4/AV-4 live sidecar evidence passed:
  `bench/phase3t-mixed-non-plugin-cp4-av4-20260601.json`
  (`run_id=local-12cb8bd20f51`, artifact SHA
  `81eb221542666647a3b0a80a0ed254dff674a0ead27d814bfcea26bd14996d53`)
  closed 44 green 1/3/5/10 mixed-load cells across read/write/edit,
  `exec_command`, PTY, glob/grep, conflict, and layer maintenance lanes. CP-4
  final state passed with hash
  `83312110b4ab6fffcd046279741d8b5c8d283617c9a6995d1d0a783d2bd6926d`;
  AV-4 pulled 2,422 daemon audit events into
  `bench/phase3t-mixed-non-plugin-cp4-av4-20260601.sandbox_events.jsonl`
  with `dropped_event_count=0`, `lost_before_seq=0`, buffer pressure `0.1574`,
  and required PTY/OCC/lease/cleanup event types present. Performance reports:
  `.performance_report.json` and `.performance_report.md`.
- ✅ Deferred non-plugin CP-5 cache-lock churn passed:
  `bench/phase3t-cache-lock-churn-cp5-20260601.json`
  (`run_id=local-56cf60c52d6f`, same artifact SHA) drove 260 synthetic
  `layer_stack_root` values through Rust OCC runtime services. The report
  passed `samples_ok`, `readbacks_ok`, `distinct_root_contents`, `reuse_hit`,
  `cache_bounded`, `evicted_after_churn`, and `metrics_reported`; cache size was
  bounded at 256 with 5 evictions, 260/260 writes and 260/260 same-path
  readbacks succeeded, write p50/p95/max was `5.425/7.113/9.250 ms`, readback
  p50/p95/max was `1.082/1.826/3.136 ms`, and OCC cache-lock max wait was
  `0.0265 ms`.
- ✅ Deferred non-plugin AV-7 forward/back parity passed:
  `bench/phase3t-av7-forward-back-parity-20260601.json`
  (`run_id=local-a82fa8f20194`, same artifact SHA) proved Python reads
  Rust-published state, Rust reads Python-published state, non-base
  `layer_digest` streams are byte-identical, final workspace hashes are equal,
  and duplicate-head dedup decisions match both directions.
- ✅ Deferred non-plugin §7 differential/property gate passed:
  `bench/phase3t-section7-non-plugin-differential-20260601.json`
  (`run_id=local-42770354ec75`, same artifact SHA) drove matching Python/Rust
  sequences through separate roots covering read/write/edit, glob/grep,
  `exec_command`, conflict contention, atomic multi-path shell writes,
  delete/whiteout, symlink rejection parity, no-op capture, squash pressure,
  and Rust PTY finalization. Canonical outcome classes, conflict counts, and
  final workspace hash all match; Python and Rust both ended at manifest depth
  16 with no missing/orphan layers.
- ✅ Rust PTY control ops now consult the PTY completion mailbox before
  returning `pty_session_not_found`: `api.v1.pty.progress` covers natural
  completion polling, and `api.v1.pty.write_stdin` / `api.v1.pty.cancel` now
  claim an already-finalized result if finalization won the control-call race.
  This removes the need for a harness-only `collect_completed` fallback on
  natural PTY completion races and keeps explicit cancel from masking an
  already completed terminal result.
- ✅ Follow-up current-checkout cleanup fixed the PTY cancel duplicate race and
  made explicit cancel / isolated force-exit cleanup use the same shared
  process-group termination helper as natural PTY finalization. Shared and
  isolated PTY finalizers now publish background completion entries only when
  the session was not explicitly cancelled and the finalizer still owns the live
  PTY registry entry. Verification:
  `cargo test -p eos-daemon pty_cancel_suppresses_background_completion_publication
  --lib`; `cargo test -p eos-daemon --lib -- --skip plugin`; host and Linux
  target daemon clippy with `unwrap_used`, `expect_used`, and
  `undocumented_unsafe_blocks` denied; `cargo fmt --all --check`; and live
  Docker/dask isolated inspection
  `bench/phase3t-rust-isolated-inspection-docker-20260602-pty-cancel-termination-fix.json`
  (`run_id=local-b913335431e4`, artifact SHA
  `2cd435f95b3de918c6344c647c84ee67b78990b6bc9ff9012d804342a5c5b699`) passed
  74/74 checks including PTY progress/stdin, natural completion notification,
  timeout notification, explicit cancel, duplicate-cancel suppression,
  force-exit PTY cleanup, leak inspection, and same-port `3000` isolation.
- ✅ Rust daemon OCC publishes now run LayerStack auto-squash maintenance after
  successful publishes, matching Python under §7 squash pressure. Python
  `LayerPublisher` now skips non-regular files during staging fsync so kernel
  whiteout devices from delete capture do not fail the publisher.

**Phase 3T Rust isolated-workspace slice (control plane + command routing, 2026-06-01)**
- ✅ `eos-daemon` now routes `api.isolated_workspace.enter`, `exit`, `status`, `list_open`, and `test_reset` through `sandbox/crates/eos-daemon/src/isolated.rs` instead of disabled dispatcher stubs. The singleton owns an `eos-isolated::IsolatedSession`, a per-root LayerStack snapshot/lease port, JSONL audit sink, and active PTY records keyed by `agent_id`.
- ✅ `eos-isolated` now implements env-sourced `ResourceCaps`, `TOTAL_CAP` quota and Python-parity `host_ram_pressure` admission gates, append-only `JsonlAuditSink`, daemon-side handle maps, enter/exit lifecycle state, scratch cleanup, audit-only tool-call records, namespace/control FD closure on teardown, IP-pool/veth allocation bookkeeping, Linux-target bridge/veth netlink setup, and static nftables setup via `NETLINK_NETFILTER`: create/up `eos-shared0`, install NAT/filter tables and base chains, add MASQUERADE, IMDS drop, and optional RFC1918 deny rules, allocate the namespace IP/name, create the veth pair, move the namespace peer into the holder netns, attach/up the host side, and request bridge-port isolation. Local amd64 Docker/dask live validation passed with no `ip` or `nft` in the target image.
- ✅ `eos-ns-holder` now performs the first holder syscall slice: unshare user/mount/pid-for-children/net namespaces, write uid/gid maps, set private mount propagation, pin namespace FDs, best-effort `/proc` rbind, handshake over readiness/control pipes, optional veth config parsing, namespace-side link/address/default-route programming, RA-disable hook, best-effort rtnetlink loopback-up/default-IPv6-route deletion, and pause until daemon teardown.
- ✅ `eos-runner` now has a Linux setns execution slice instead of the previous `todo!()` bodies: `RunMode::SetNs` validates namespace FDs, joins the optional isolated cgroup, applies namespace FDs in Python-compatible `user`/`mnt`/`pid`/`net` order, and delegates command/search execution to the same runner primitive used by fresh namespaces. `setns_overlay_mount` now enters `user`+`mnt` namespaces and calls the overlay mount port with the isolated upper/work directories.
- ✅ `eos-daemon` now spawns `eosd ns-holder`, opens inheritable `/proc/<holder>/ns/{user,mnt,pid_for_children,net}` FDs, mounts the isolated overlay through `eosd ns-runner --mount-overlay`, sends the extended `net-ready` payload when veth metadata exists, tracks/kills the holder child, and routes Linux `api.v1.exec_command` and PTY start through `RunMode::SetNs` when the active `agent_id` handle has namespace FDs. Isolated results are marked `workspace=isolated`, capture changed paths for audit/result visibility, and do not call OCC publish. Active PTY records block non-forced isolated exit.
- ✅ Local amd64 Docker/dask isolated proof passed with runtime upload SHA `6ca9c294217c016e7afd130a806cdbd59fee5b6198a02c489a79361b2e538709`: the target image lacked `ip` and `nft`; enter succeeded; host bridge/veth appeared; finite `exec_command` saw the namespace veth and default route; finite and PTY writes stayed private and unpublished; PTY natural exit worked; non-forced exit returned `active_pty_sessions`; force exit succeeded; status/list reported closed; host veth was removed; and isolated writes stayed unpublished after exit.
- ✅ Focused isolated exit inspection now returns and audits daemon-local cleanup fields: handle/agent map counts, lease-release status, active lease count, holder PID/kill error, namespace FD count, cgroup existence, scratch/upper/workdir existence, mountinfo reference count when available, and PTY force-cancel cleanup arrays. Focused lifecycle tests assert no registered handle/agent remains, active leases return to zero, scratch/upper/workdir are removed, the exit audit JSONL carries the same inspection payload, and stale PTY force-cancel cleanup clears active PTY state.
- ✅ Live isolated exit inspection rerun passed in Docker/dask with current amd64 artifact SHA `ddb923eb0f1a3e6b1cd367ab978f7056088175532a26c6b262a94d3ff029b6e7`: `bench/phase3t-rust-isolated-inspection-docker-20260602-post-ephemeral-removal.json` (`run_id=local-d8e7bff8015a`) passed 74/74 checks. The target image lacked `ip` and `nft`; cgroup was writable; enter/status/list_open succeeded; finite command and PTY writes stayed private and unpublished; isolated PTY controls covered progress, stdin, natural completion notification, timeout notification, cancel, and cancel duplicate suppression; two isolated agents both bound port `3000`, each reached its own localhost server, and cross-agent access was blocked; namespace veth/default route were visible; non-forced exit returned `active_pty_sessions`; force exit cancelled the real PTYs and left no active/stale PTY ids; inspection reported zero active leases, zero mountinfo refs, removed cgroup/scratch/upper/workdir, removed host veth, holder process gone, and JSONL exit audit coverage remained aligned with the response.
- ✅ Earlier current-checkout isolated inspection rerun passed after the daemon
  cleanup: `bench/phase3t-rust-isolated-inspection-docker-20260602-current.json`
  (`run_id=local-b26b3a59745e`, artifact SHA
  `0bd46883487a4a632c98f14a348d2efed4f86c8695f92b9d2c4f287f4ee801fe`)
  passed 74/74 checks with the same finite command, PTY control/notification,
  force-exit cleanup, leak-inspection, and same-port `3000` network isolation
  coverage.
- ✅ ResourceCaps host-capacity parity follow-up passed on rebuilt amd64
  artifact SHA
  `85c5f952b4c7210bd97c71e5b6d127450549ce216666642b40e91deef614cfed`:
  `bench/phase3t-rust-isolated-inspection-docker-20260602-host-ram-gate.json`
  (`run_id=local-d416d299bff8`) passed 74/74 checks. The bench now sets
  `EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES=67108864` so its two-agent port
  `3000` namespace-isolation probe is not rejected by the newly enforced
  Python-parity host RAM admission gate on memory-constrained Docker hosts.
  The run still proved the target image lacked `ip`/`nft`, both isolated agents
  entered, force exit left `active_leases_after=0`, and all PTY
  progress/stdin/natural-exit/timeout/cancel notification checks stayed green.
- ✅ Latest isolated-workspace command-routing rerun passed after the PTY
  termination cleanup on rebuilt amd64 artifact SHA
  `2cd435f95b3de918c6344c647c84ee67b78990b6bc9ff9012d804342a5c5b699`:
  `bench/phase3t-rust-isolated-inspection-docker-20260602-pty-cancel-termination-fix.json`
  (`run_id=local-b913335431e4`) passed 74/74 checks. This keeps the quoted
  Phase 3T Rust isolated-workspace slice closed: daemon lifecycle RPC routing,
  `eos-isolated` state/audit/caps/network setup, `eos-ns-holder`, runner
  `RunMode::SetNs`, no-OCC isolated command/PTY writes, active-PTY exit
  blocking, PTY natural/timeout/cancel notifications, same-port `3000`
  isolation, and leak inspection are covered by current live Docker/dask
  evidence. The remaining yellow isolated row is broader AV-9/BYO/cutover
  validation, not this command-routing/control-plane slice.
- ✅ Follow-up `eos-isolated` host-capacity test cleanup replaced the remaining
  panic-style assertion with a `Result` test and explicit
  `HostRamPressure` match. Strict `eos-isolated` all-target clippy with
  `unwrap_used`, `expect_used`, and `undocumented_unsafe_blocks` denied,
  `cargo test -p eos-isolated` (`4 passed`), `cargo fmt --all --check`, and a
  focused `panic!/expect/unwrap` source scan on `session.rs` are green.
- ✅ Rust isolated DNS configuration is no longer a daemon no-op:
  `eos-runner::setns::configure_dns` now mirrors the Python
  `configure_dns_in_ns.py` helper by entering `user`+`mnt` namespaces,
  detecting loopback resolvers, and bind-mounting a private fallback
  `/etc/resolv.conf`; `eosd ns-runner --configure-dns` exposes the one-shot
  helper; and the daemon `NamespaceRuntimePort::configure_dns` calls it and
  returns the `applied_fallback` result. Focused coverage includes the
  host-safe first-nameserver/fallback detector test and Linux-target clippy over
  the setns/bind-mount implementation. Live routable-DNS/symlinked-resolv.conf
  parity remains part of the later AV-9/BYO matrix.
- ✅ Follow-up daemon audit-classifier cleanup keeps the PTY completion mailbox
  endpoint but stops treating `api.v1.pty.collect_completed` as an
  overlay/lease lifecycle operation. It still emits `background_tool.completed`
  audit when the Python background supervisor drains completions, but it no
  longer fabricates `layer_stack.lease_released` /
  `overlay_workspace.cleanup` events for a mailbox-only request. Focused
  coverage:
  `cargo test -p eos-daemon pty_collect_completed_is_background_only_not_overlay_lifecycle`
  (`1 passed`) plus strict workspace clippy.
- ✅ Focused checks: `cargo fmt --all --check`; `cargo check -p eos-ns-holder -p eos-isolated -p eos-daemon --target x86_64-unknown-linux-musl`; `cargo check -p eos-ns-holder -p eos-isolated -p eos-daemon --target aarch64-unknown-linux-musl`; `cargo clippy -p eos-ns-holder -p eos-isolated -p eos-daemon --target x86_64-unknown-linux-musl --all-targets`; `cargo test -p eos-isolated` (`4 passed`, now covers ResourceCaps host-capacity helpers); `cargo test -p eos-runner` (`8 passed`); `cargo test -p eos-ns-holder` (`5 passed`); `cargo test -p eos-daemon isolated_workspace --test phase2_read_paths` (`3 passed`); `cargo test -p eos-daemon host_ram_pressure_error_keeps_capacity_details`; `cargo test -p eos-daemon active_pty_records_block_exit_until_cleared`; `cargo test --workspace --all-targets`; `cargo run -p xtask -- package --target x86_64-unknown-linux-musl --out-dir dist`; `cargo run -p xtask -- package --target aarch64-unknown-linux-musl --out-dir dist`; `cargo clippy --workspace --all-targets --no-deps -- -D warnings -W clippy::pedantic -W clippy::nursery -D clippy::too_many_lines -D clippy::missing_const_for_fn -D clippy::significant_drop_tightening -D clippy::similar_names -D clippy::needless_pass_by_value -D clippy::unwrap_used -D clippy::expect_used -D clippy::undocumented_unsafe_blocks -D clippy::allow_attributes`; and `cargo tree -p eos-isolated --target x86_64-unknown-linux-musl --edges normal` with no `eos-occ` edge. The 2026-06-01 cleanup reran the touched syscall subset for `x86_64-unknown-linux-musl` warning-clean after removing the Linux `ENOATTR` branch; Linux musl test binary linking was not attempted as evidence because the macOS host linker rejects the target's `--as-needed` flag.

**Docs**
- ✅ PLAN §12 (verified Docker/dask/plugin config) + §13 (Phase-0 status + 8 source-verified corrections).

**Re-verify everything:**
```
.venv/bin/python backend/scripts/build_upload_eosd_docker.py --arch amd64 --image sweevo-dask__dask-10042:latest --report bench/local-eosd-amd64-upload.json
.venv/bin/python backend/scripts/build_upload_eosd_docker.py --arch arm64 --image python:3.11-slim --platform linux/arm64 --report bench/local-eosd-arm64-upload.json
cd sandbox && cargo test -p eos-protocol && cargo check --workspace && cargo clippy --workspace && cargo fmt --all --check
cd sandbox && cargo test -p eos-daemon --test phase2_read_paths && cargo clippy -p eos-layerstack -p eos-daemon -p eosd --all-targets
cd .. && .venv/bin/python -m pytest backend/tests/unit_test/test_sandbox/test_provider/ backend/tests/unit_test/test_sandbox/test_contract_fixtures_pin.py backend/tests/unit_test/test_sandbox/test_api/test_daemon_client.py -q
.venv/bin/python backend/scripts/bench_sandbox_e2e.py --commands 10 --report /tmp/eos-synthetic-bench.json
.venv/bin/python backend/scripts/bench_sandbox_e2e.py --docker-image sweevo-dask__dask-10042:latest --phase0 --commands 10 --report bench/baseline-amd64.json
.venv/bin/python backend/scripts/bench_rust_daemon_phase2.py --docker-image sweevo-dask__dask-10042:latest --artifact sandbox/dist/eosd-linux-amd64 --baseline bench/baseline-amd64.json --report bench/phase2-rust-daemon-amd64.json
.venv/bin/python backend/scripts/bench_rust_daemon_phase3.py --arch amd64 --docker-platform linux/amd64 --docker-image sweevo-dask__dask-10042:latest --artifact sandbox/dist/eosd-linux-amd64 --phase1-baseline bench/phase1-ns-runner-amd64.json --phase0-baseline bench/baseline-amd64.json --report bench/phase3-rust-daemon-amd64.json --samples 10 --load-concurrency 1,3,5,10 --load-rounds 10
# Direct Phase 1 dask evidence is currently captured in bench/phase1-ns-runner-amd64.json.
```

---

## NEXT — ordered, concrete

### Current remainder classification — 2026-06-02
- **No remaining functional Phase 3 / Phase 3T non-plugin gates are open**
  outside the explicitly excluded plugin AV-10 tail and broader
  Phase 3.5/AV-9/BYO/cutover scope. CP-4t, typed subagent/background
  surfaces, isolated command-routing proof, CP-4/AV-4 mixed non-plugin load,
  CP-5 cache-lock churn, AV-7 forward/back parity, and §7 non-plugin
  differential/property contention are closed by the artifacts listed below.
- **Remaining nonfunctional closeout hygiene:** keep architecture docs refreshed
  only when surfaces actually change; do not stage/commit the dirty worktree
  accidentally; and keep the arm64 CP baseline leg separate from the already
  captured local arm64 upload/run proof.
- **Not a Phase 3T blocker:** release-grade minisign provenance remains the
  later AV-8/cutover-adjacent gate; CP-1b minimal/BYO-image validation remains
  the later BYO matrix gate.

### A. Phase 0 closeout follow-ups (not blocking local amd64)
1. **Release-grade provenance** — minisign fail-closed verification remains a later AV-8 gate. Current Phase 0 local closeout is SHA-pinned but unsigned by design.
2. **Arm64 CP baseline leg** — `local-eosd` arm64 upload/run is captured; `bench/baseline-arm64.json` CP-0/CP-1 remains for an arm64-native Docker host or explicit local runner. The local `sweevo-dask__dask-10042` image is the amd64 CP baseline leg.
3. **Minimal-image matrix** — when Phase 1/CP-1b starts, extend local upload checks to non-root and read-only-rootfs images. The current amd64 gate proves the artifact needs no in-image Rust/toolchain and can be uploaded via provider `put_archive`.

**Re-run the amd64 CP baseline when needed:**
   ```
   .venv/bin/python backend/scripts/build_upload_eosd_docker.py \
     --arch amd64 \
     --image sweevo-dask__dask-10042:latest \
     --report bench/local-eosd-amd64-upload.json
   .venv/bin/python backend/scripts/build_upload_eosd_docker.py \
     --arch arm64 \
     --image python:3.11-slim \
     --platform linux/arm64 \
     --report bench/local-eosd-arm64-upload.json
   .venv/bin/python backend/scripts/bench_sandbox_e2e.py \
     --docker-image sweevo-dask__dask-10042:latest \
     --phase0 \
     --commands 10 \
     --report bench/baseline-amd64.json
   ```

### B. Phase 1 closeout guardrails
- Treat Phase 1 as closed for the scoped direct `eosd ns-runner` fresh-ns boundary. Keep `bench/phase1-ns-runner-amd64.json` as the direct-runner dask evidence until a checked-in Phase 1 harness exists.
- Do not flip the global default to `EOS_SANDBOX_RUNTIME=rust` from Phase 1 alone. Phase 2 now proves persistent daemon routing and endpoint readiness for the read path, but the global default flip still waits for the later cutover gates.
- Current scope clarification: Phase 1's direct-runner evidence remains fresh-ns
  only, but Linux setns command routing has since landed and is covered by the
  Phase 3T isolated-workspace evidence below. The remaining Phase 3.5 work is
  broader AV-9/BYO/cutover validation, not an unimplemented setns runner body.

### C. Phase 2 — daemon + read paths
- ✅ Closed by `bench/phase2-rust-daemon-amd64.json`. Keep write/publish, shell/search, plugin, and isolated mode out of the Phase 2 result; those remain Phase 3/3.5 gates.

### D. Phase 3 — closed structural core
- ✅ Closed at the structural boundary. The landed slice covers routed OCC write/edit validation, shell/search overlay daemon paths, background registry/control ops, PPC framing/no-OCC plugin edge, LayerStack squash/GC, and CP-4s structural live evidence.
- CP-4s raw-argv live evidence is green and retained as historical structural evidence only: `bench/phase3-rust-daemon-amd64.json` run `local-f1bd63a4b0f3` cleared the 70% target (`command_exec.run_command_s` p50 `16.109 ms` vs required `<=95.468 ms`), `host-wall` p50/p95 was `31.787/32.628 ms`, concurrent no-op host p95 was `34.650/37.024/69.151/109.333 ms` at 1/3/5/10, and concurrent unique `touch` host p95 was `33.940/41.839/74.701/152.729 ms`. This closes CP-4s, not CP-4 throughput/contention.
- The next shell contract no longer gates on raw argv. CP-4 and CP-4t must run against non-login Bash shell strings with overlay/OCC included.

### E. Phase 3T — closed sidecar and deferred gate closeout
1. **CP-4t is closed for Docker shared-workspace command/PTY paths.** Keep `bench/phase3t-pty-command-docker-20260601-current-eos-paths-post-notify.json`, `bench/phase3t-pty-command-docker-20260601-current-eos-paths-timeout-cancel-fix.json`, `bench/phase3t-pty-command-docker-20260601-review-cleanup.json`, and the tiered summaries above as the command/session evidence. Do not reintroduce model-facing raw argv gates.
2. **Typed subagent surfaces and deeper loop evidence are closed.** The model-facing generic background tools are retired from runtime/catalog exposure, and the old `BaseTool.background` / `@tool(background=...)` policy is removed. `engine.background.policy` now hard-codes background-manager attachment for subagent launch and PTY sessions. `run_subagent(agent_name, prompt)` returns `subagent_session_id`; `check_subagent_progress(subagent_session_id, last_n_messages)` and `cancel_subagent(subagent_session_id)` are exposed for parent agents; subagent supervisor records use the same `subagent_session_id` rather than a hidden `bg_N` alias. Mocked query-loop coverage now proves natural completion, no-terminal failure, explicit cancel, and parent terminal submission while a subagent is active; parent terminal exit records `non_cancellation_tool_request` in typed notification/audit evidence.
3. **Rust isolated Docker proof with exit inspection is closed.** Keep `bench/phase3t-rust-isolated-inspection-docker-20260602-post-ephemeral-removal.json` as the original leak-inspection proof and `bench/phase3t-rust-isolated-inspection-docker-20260602-host-ram-gate.json` as the current rebuilt-artifact proof after the host RAM admission gate landed. Together they show no leaked holder, mountinfo refs, cgroups, leases, scratch dirs, host veth, or active PTY records after force exit; isolated PTY stdin/progress/natural-exit/timeout/cancel notification behavior; and same-port network isolation with two agents binding port `3000`.
4. **CP-4 mixed non-plugin load with AV-4 audit pull is closed for the sidecar scope.** Keep `bench/phase3t-mixed-non-plugin-cp4-av4-20260601.json`, `.sandbox_events.jsonl`, `.performance_report.json`, and `.performance_report.md` as the live Docker/dask evidence. Plugin operations remain outside this gate.
5. **CP-5 cache-lock churn is closed for the sidecar scope.** Keep `bench/phase3t-cache-lock-churn-cp5-20260601.json` as the live Docker/dask evidence for >256 roots, bounded LRU eviction, readback, no stale reuse, and cache-lock wait metrics.
6. **AV-7 forward/back parity is closed for the sidecar scope.** Keep `bench/phase3t-av7-forward-back-parity-20260601.json` as the bidirectional on-disk parity artifact.
7. **§7 non-plugin differential/property contention is closed for the sidecar scope.** Keep `bench/phase3t-section7-non-plugin-differential-20260601.json` as the Python/Rust differential artifact. Plugin PPC/AV-10 remains outside this skipped scope.
8. **Plugin PPC/AV-10 remains skipped for the current closeout scope.** Do not treat this as a blocker for the non-plugin Phase 3T sidecar. Plugin PPC multiplexing is now landed and live-gated; the remaining plugin gate is broader AV-10 parity beyond the representative live coverage already listed above, plus broader crash-recovery lanes.
9. **Refresh architecture docs only where surfaces change.** If tool names, terminal-session lifecycle, background identifiers, isolated-workspace routing, or plugin-dispatch ownership change, update the smallest affected `docs/architecture` page alongside the implementation.

### F. Phase 3.5 (isolated) then Phase 5 (cutover) — per PLAN §5
- **Closed inside Phase 3T:** isolated daemon RPC routing, daemon-local handle
  state, ns-holder/setns command routing, shell-free bridge/veth/nft setup,
  no-OCC isolated command/PTY writes, active-PTY exit blocking, leak inspection,
  same-port `3000` network isolation, Rust setns DNS helper wiring, and
  ResourceCaps `TOTAL_CAP` plus `host_ram_pressure` admission parity.
- **Still later-phase non-plugin work:** AV-9 full isolated lifecycle parity
  against Python, including enter-gate/exit-drain/background-work semantics,
  TTL/phase-budget lifecycle behavior, DNS resolver/fallback edge cases, and
  the existing IWS concurrency and phase-budget suites against Rust.
- **Still later-phase environment work:** CP-1b setns validation across the
  full BYO matrix (kernel floor/LTS, amd64 and arm64, non-root image, and
  read-only-rootfs image). The current proof is local amd64 Docker/dask.
- **Still cutover work:** AV-5a/AV-5b read/write A/B traffic, AV-8 minisign
  fail-closed provenance, final AV-9/AV-10 checks, and the Phase 5 deletion of
  Python runtime/bundle paths.

---

## Notes / risks for next session
- **Deferred anchors are not logic.** There are no remaining Rust `todo!()` bodies in the Phase 3/3T/3.5 crates checked by the cleanup pass. Plugin `// PORT` anchors remain useful source-evidence labels, while the skipped work-list is now the broader AV-10 parity and crash-recovery scope called out above; PPC operation multiplexing is no longer skipped.
- **macOS can build/package the pure-Rust static musl amd64 artifact with `rust-lld`, but cannot validate Linux syscall behavior.** All syscall/overlay/OCC-contention work must be checked in the dask container (PLAN §12.2 recipe) — `cargo check` on macOS only validates the non-Linux `cfg` surface.
- **Not committed.** Treat the worktree as parallel-agent dirty; stage intentionally.
- **CAS byte-identity is the sharpest correctness lever** — any new code computing `manifest_root_hash`/`layer_digest` must pass `fixtures/cas/cases.json` (esp. the unicode cases).
