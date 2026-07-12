# EphemeralOS Benchmark Laboratory

This directory contains the local, real-product benchmark system. The Rust
runner serves the production React application and the loopback-only
`/api/v1` contract from one origin. Benchmark operations execute through an
isolated `sandbox-gateway` and Docker-backed EphemeralOS daemon; the browser is
never an executor.

## Layout

- `defaults/` contains strict, versioned Default configuration and fixture
  profiles.
- `presets/` contains complete, strict, versioned experiment plans. Presets are
  data only and cannot select executors, product routes, credentials, commands,
  or safety overrides.
- `backend/` is the `sandbox-benchmark` Cargo package and binary.
- `web/` is the same-origin React/Mantine application and its fixture and real
  backend browser suites.

Runtime state is written only beneath the configured dedicated test workspace
root, under `benchmark/{fixtures,runs,results,runtime}`. It is not stored in
this directory.

## Local checks

From the repository root:

```sh
cargo test -p sandbox-benchmark --all-targets
```

From `benchmark/web`:

```sh
npm run test
npm run test:fixture
npm run build
```

The final release gate requires Docker and the real EphemeralOS product path:

```sh
npm run test:real-backend
```

That command builds release binaries and production web assets, starts the
runner on a generated loopback port with a dedicated workspace root, drives
Quick Smoke through the browser, and retains sanitized evidence outside the
repository. It rejects request interception, mock service workers, fake
adapters, browser state injection, required-request failures, unsafe path
changes, secret-like evidence, and incomplete cleanup.

## Safety boundary

Only one campaign may be active. Run All executes Command, Files, Workspace,
then LayerStack sequentially. State-changing HTTP requests require the exact
same origin and a bootstrap nonce. Product access, command cases, session
creation, artifacts, checks, phases, and comparison projections are closed,
typed allowlists compiled into the runner. Workspace deletion requires a
canonical path and matching benchmark ownership marker.
