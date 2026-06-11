# plugin

## Overview

This module owns the unified live E2E contract for plugin package setup, daemon-hosted PPC service dispatch, workspace refresh, restart behavior, LSP dispatch, and isolated-gating coverage for the sandbox plugin subsystem. It exercises daemon ops `api.plugin.ensure`, `api.plugin.status`, dynamic plugin ops `plugin.generic.query` and `plugin.lsp.query_symbols`, plus `api.v1.write_file`, `api.v1.exec_command`, `api.isolated_workspace.enter`, and `api.isolated_workspace.exit` for fixture setup and routing gates. Module config lives at `crates/eos-e2e-test/tests/plugin/config/default.test.yml`.

## Checklist

- [ ] plugin-package-ensure: Warm/cold ensure requests upload when missing, publishes digest-keyed package roots, runs setup, and creates dependency/scratch setup artifacts.
- [ ] plugin-setup-idempotent: Re-ensure with matching package and setup digests skips upload/setup and keeps the setup count at one.
- [ ] plugin-dispatch-roundtrip: Dynamic daemon PPC dispatch preserves operation name, request body, success envelope, package root, and dependency root.
- [ ] plugin-service-hosted: Daemon-hosted plugin services start as live PPC workers, connect routes, and report accepted health probes.
- [ ] plugin-service-cleanup: Reload removes old routes, PPC clients, service snapshots, staged uploads, and worker processes. Socket-path unlink is not asserted: `sandbox/crates/eos-plugin-ops/src/process.rs` removes stale socket paths before bind, while `PluginServiceProcess::teardown` only terminates the child process.
- [ ] plugin-refresh-remount: A read-only service sees the latest workspace after a LayerStack update and records bounded refresh activity.
- [ ] plugin-refresh-singleflight: Concurrent refreshes after one workspace edit all observe new content while refresh counts stay bounded.
- [ ] plugin-restart-policy: The `restart_service` strategy restarts the worker process instead of remounting the workspace.
- [ ] plugin-isolated-gate: Plugin dynamic ops are rejected while the caller is in isolated workspace mode and the handle exits cleanly.
- [ ] plugin-lsp-lifecycle: The LSP package uses the generic lifecycle, installs node/pyright dependency roots, connects `plugin.lsp.query_symbols`, and returns live workspace symbols.
- [ ] plugin-write-allowed: Write-allowed or oneshot plugin operations publish only through daemon-owned OCC paths and report changed paths.
- [ ] plugin-lsp-apply-and-failures: LSP read-only queries, workspace edit application, stale edit conflict/retry, and structured setup failures are covered through daemon plugin/LSP flows.
- [ ] plugin-intent-contract: Missing intent, non-write caller misuse, unbootstrapped plugin calls, and foreign/lifecycle misuse fail fast with stable error payloads.
- [ ] plugin-reload-dispatch-race: Concurrent plugin dispatches during a package reload each return a structured payload (success, or a structured error through the worker swap window), the reload succeeds and reconnects the route, and the post-reload steady state runs the reloaded digest with a single worker process.

## Test Case

| Test name | Test description | Command to run | Checklist item |
|---|---|---|---|
| `plugin_package_setup_lifecycle` | Groups `host_ensure_plugin_package_installs_generic_package`, `generic_package_installs_and_sets_up`, `generic_package_reensure_is_idempotent`, and the package/setup portion of `lsp_package_uses_generic_lifecycle_and_dispatches_symbols`: missing packages request upload, staged packages publish digest roots, setup artifacts are created, and repeated ensure skips setup. | `cargo test -p eos-e2e-test --features e2e --test plugin -- --nocapture` | `plugin-package-ensure`, `plugin-setup-idempotent`, `plugin-lsp-lifecycle` |
| `plugin_failure_contracts` | Uses `plugin_setup_and_manifest_failures_are_structured`: manifest intent validation and setup-command failures return structured daemon error envelopes with stable kind/message payloads. | `cargo test -p eos-e2e-test --features e2e --test plugin plugin_setup_and_manifest_failures_are_structured -- --nocapture` | `plugin-lsp-apply-and-failures`, `plugin-intent-contract` |
| `plugin_service_dispatch_and_health` | Groups `generic_plugin_dispatch_roundtrip`, `service_health_probe_reports_connected_service`, and the LSP dispatch portion of `lsp_package_uses_generic_lifecycle_and_dispatches_symbols`: dynamic PPC routes preserve request/response envelopes, service health probes are accepted, and `plugin.lsp.query_symbols` returns live workspace symbols. | `cargo test -p eos-e2e-test --features e2e --test plugin -- --nocapture` | `plugin-dispatch-roundtrip`, `plugin-service-hosted`, `plugin-lsp-lifecycle` |
| `plugin_workspace_refresh_and_restart` | Groups `generic_plugin_refreshes_after_workspace_edit`, `concurrent_plugin_refresh_singleflight`, and `restart_service_strategy_restarts_on_workspace_edit`: services must observe LayerStack updates, concurrent stale dispatch shares one daemon refresh, and `restart_service` restarts rather than remounts. | `cargo test -p eos-e2e-test --features e2e --test plugin -- --nocapture` | `plugin-refresh-remount`, `plugin-refresh-singleflight`, `plugin-restart-policy` |
| `plugin_isolated_gate` | Uses `generic_plugin_rejected_in_isolated_workspace`: dynamic plugin operations are rejected with `forbidden_in_isolated_workspace` while the caller is isolated, and the isolated handle exits cleanly afterward. | `cargo test -p eos-e2e-test --features e2e --test plugin -- --nocapture` | `plugin-isolated-gate` |
| `plugin_cleanup_and_write_publish` | Groups `package_reload_reaps_old_service_and_routes` and `oneshot_overlay_plugin_write_publishes_through_occ`: package reload replaces dynamic routes, old worker processes, service status, and staged upload roots; write-allowed oneshot overlay operations publish changed paths through daemon-owned OCC. Socket unlink remains unsupported as described in `plugin-service-cleanup`. | `cargo test -p eos-e2e-test --features e2e --test plugin -- --nocapture` | `plugin-service-cleanup`, `plugin-write-allowed` |
| `plugin_reload_dispatch_race` | Uses `concurrent_dispatch_during_reload_stays_structured`: N `plugin.generic.query` dispatches race an `api.plugin.ensure` package reload; every dispatch returns a structured payload through the worker swap, the reload reconnects the route, and the steady state routes to the reloaded digest with a single worker process. | `cargo test -p eos-e2e-test --features e2e --test plugin concurrent_dispatch_during_reload_stays_structured -- --nocapture` | `plugin-reload-dispatch-race`, `plugin-service-cleanup`, `plugin-refresh-singleflight` |
