# Phase 5 — process.exec-backed daemon default

Status: implemented, then corrected on 2026-05-03 after Phase 3.5/3.6 and live Phase 5 measurements showed the native transport-verb wrapper did not beat the direct process.exec socket shim.

## Correction

The earlier Phase 5 plan promoted a first-class sandbox transport verb for code-intelligence daemon command. That design has been retired.

The active contract is simpler:

1. The public sandbox transport exposes `process.exec` only.
2. The code-intelligence daemon backend reaches the in-sandbox Unix socket through a short Python bridge launched with `process.exec`.
3. No feature flag chooses between native transport and shim paths.
4. No provider-specific code-intelligence daemon command verb exists in Daytona or other transports.
5. Future performance work should use batching, persistent sessions, or provider-native socket streaming only if it is genuinely lower overhead than `process.exec`.

## Why the native verb was removed

Phase 3.5/3.6 isolated the large regression to sync-bridge event-loop churn, not the socket protocol itself. After that was fixed, the remaining floor was the per-call `process.exec` launch and transport round trip.

The attempted native transport verb wrapped the same `process.exec` path inside Daytona transport code. It added protocol surface, tests, feature flags, and A/B branches while preserving the same underlying cost. Live comparison showed it was slower/noisier than the direct `DaemonBackend` bridge, so keeping it would make the codebase larger without improving the hot path.

## Goal

Make daemon mode the default without adding a special transport verb:

1. Route service calls through the daemon-backed code-intelligence backend by default.
2. Keep the daemon command bridge explicit and local to the `DaemonBackend`.
3. Delete the native transport-verb branch and the forced-shim feature flag.
4. Keep lifecycle, recovery, telemetry, and overlay behavior unchanged.

## What ships

| Component | File | Active behavior |
| --- | --- | --- |
| Transport protocol | `backend/src/sandbox/api/transport.py` | Only `exec` is required for command execution. |
| Daytona transport | `backend/src/sandbox/daytona/transport.py` | No code-intelligence-specific daemon command method; callers use `exec`. |
| daemon backend | `backend/src/sandbox/code_intelligence/backends/` | Always sends framed daemon requests through the Python Unix-socket bridge launched by `transport.exec`. |
| Backend selection | `backend/src/sandbox/code_intelligence/backends/` | Daemon backend remains the default when sandbox daemon mode is enabled. |
| Live Phase 5 test | `backend/tests/test_e2e/test_live_ci_phase5_default_on.py` | Verifies daemon-default behavior and warm/concurrent service calls, without native-vs-shim A/B branches. |
| Unit tests | `backend/tests/test_sandbox/test_code_intelligence/test_daemon_client_process_exec.py` | Covers framing, retry, daemon-unavailable, and process.exec bridge behavior. |

## Tasks

### Task 5.1 — Keep `SandboxTransport` tool-shaped

`SandboxTransport` should not expose one-off product verbs. Code-intelligence uses the same execution primitive as the rest of the sandbox boundary.

Done criteria:

1. No code-intelligence-specific daemon command method exists on the transport protocol.
2. Daytona transport has no native bridge template for this path.
3. Tests do not assert provider-specific code-intelligence verbs.

### Task 5.2 — Make the daemon backend shim-only

`DaemonBackend` owns the daemon wire protocol and process.exec bridge. It should not branch on provider capabilities or environment flags for the retired native verb.

Done criteria:

1. `_call_once` sends exactly one framed request through the process.exec bridge.
2. Retry and `DaemonUnavailable` semantics are preserved.
3. The bridge remains binary-safe through length-prefixed frames.

### Task 5.3 — Keep live verification focused on product behavior

Live tests should measure the daemon path users actually run, not an artificial A/B between two wrappers over the same primitive.

Done criteria:

1. Default daemon smoke test passes.
2. Warm-path symbol query remains measured.
3. Concurrent query behavior remains measured.
4. Cross-phase regression coverage remains measured.

## Performance interpretation

The useful Phase 5 finding is not "add a transport verb." The useful finding is that after the Phase 3.5/3.6 stable-loop fix, the dominant remaining per-call cost is the `process.exec` round trip itself.

Implications:

1. A wrapper over `process.exec` is not a meaningful optimization.
2. For small operations, batch requests before adding new transport API.
3. For large operations, reduce daemon-side filesystem/index work before optimizing the bridge.
4. Only revisit transport shape if the sandbox provider offers a true persistent byte stream or socket-forwarding primitive that avoids command launch overhead.

## Deletion checklist

- [x] Removed the native transport method from `SandboxTransport`.
- [x] Removed the Daytona native bridge wrapper.
- [x] Removed the feature flag that forced the shim path.
- [x] Removed native-vs-shim selection tests.
- [x] Removed live native-vs-shim A/B test.
- [x] Kept daemon-default live coverage.
