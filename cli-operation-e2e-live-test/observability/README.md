# Observability live E2E

This family verifies the standalone public observability CLI against a real
Docker sandbox. It covers both routing forms:

- `sandbox-observability-cli snapshot` returns the aggregate manager view and
  includes the ready test sandbox with a reachable daemon snapshot.
- `sandbox-observability-cli snapshot --sandbox-id <id>` returns that
  sandbox's scoped live snapshot.

The tests use the shared `core.cli` launcher, so they exercise the same
catalog-driven parsing, configuration discovery, authenticated gateway RPC,
and structured JSON response path as an operator invocation. They do not call
the daemon directly or inspect logs.

Run this family after building or rebuilding the gateway binaries:

```sh
cd cli-operation-e2e-live-test
E2E_REBUILD_BINARY=0 pytest -q observability/test_observability.py
```
