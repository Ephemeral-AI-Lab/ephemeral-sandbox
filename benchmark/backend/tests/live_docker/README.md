# Live Docker release gate

Run from the repository root:

```sh
sh benchmark/backend/tests/live_docker/run.sh
```

This is the backend-facing entrypoint for the canonical production browser
gate in `benchmark/web/tests/browser/real-backend`. It builds the release
runner and production web bundle, then exercises the actual loopback runner,
isolated gateway, and Docker-backed EphemeralOS product. It does not select a
fake adapter, mock requests, inject browser state, or fabricate artifacts.

There is intentionally no second live-smoke implementation here. Keeping the
Quick Smoke, SSE replay, cancellation/cleanup, comparison, and retained-evidence
assertions in one harness prevents the backend and browser release gates from
silently diverging. The live gate is not part of ordinary `cargo test`; it must
be invoked deliberately after recording the live-E2E invocation in the project
test report.
