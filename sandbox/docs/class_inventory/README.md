# Sandbox Workspace Class Inventory

Generated inventory of every struct, enum, trait, and notable public type
alias under the `sandbox/` Rust workspace (the `eosd` runtime), organized by
crate. One page per crate.

**210 items** — **161 structs**, **33 enums**, **12 traits**, and
**4 type aliases** — across **50 files** in **12 crates**.

Item, field, variant, and method data is extracted directly from the Rust
source. One-line purposes come from `///` doc comments, or a reviewer summary
where absent. Test-only `#[cfg(test)]` items are excluded.

> This is a generated reference. The hand-curated contract docs live separately
> under `sandbox/docs/contract/`.

| Crate | Items | Structs | Enums | Traits | Type aliases | Files | Inventory |
|-------|------:|--------:|------:|-------:|-------------:|------:|-----------|
| `eos-protocol` | 40 | 32 | 8 | 0 | 0 | 4 | [eos-protocol.md](./eos-protocol.md) |
| `eos-ns-holder` | 12 | 10 | 2 | 0 | 0 | 1 | [eos-ns-holder.md](./eos-ns-holder.md) |
| `eos-runner` | 14 | 8 | 3 | 3 | 0 | 4 | [eos-runner.md](./eos-runner.md) |
| `eos-layerstack` | 22 | 17 | 4 | 0 | 1 | 7 | [eos-layerstack.md](./eos-layerstack.md) |
| `eos-overlay` | 9 | 5 | 2 | 1 | 1 | 4 | [eos-overlay.md](./eos-overlay.md) |
| `eos-occ` | 20 | 11 | 4 | 5 | 0 | 4 | [eos-occ.md](./eos-occ.md) |
| `eos-isolated` | 15 | 10 | 2 | 3 | 0 | 5 | [eos-isolated.md](./eos-isolated.md) |
| `eos-plugin` | 18 | 11 | 6 | 0 | 1 | 7 | [eos-plugin.md](./eos-plugin.md) |
| `eos-daemon` | 55 | 52 | 2 | 0 | 1 | 11 | [eos-daemon.md](./eos-daemon.md) |
| `eosd` | 3 | 3 | 0 | 0 | 0 | 1 | [eosd.md](./eosd.md) |
| `eos-terminal-pair` | 1 | 1 | 0 | 0 | 0 | 1 | [eos-terminal-pair.md](./eos-terminal-pair.md) |
| `xtask` | 1 | 1 | 0 | 0 | 0 | 1 | [xtask.md](./xtask.md) |
| **TOTAL** | **210** | **161** | **33** | **12** | **4** | **50** | |

## Crate roles

- `eos-protocol` — Dependency-free source of truth for the eosd runtime's wire
  protocol and content-addressed-store byte identity. It owns the two
  correctness-bearing CAS hashes (manifest_root_hash, layer_digest), the framed
  newline-delimited-JSON envelope encode/decode, and the shared schema for
  tool-verb request/response models, daemon audit-event sections, and frozen
  protocol constants.
- `eos-ns-holder` — The single-threaded child of the eosd runtime that unshares
  and pins the isolated workspace's full namespace stack (user/mount/pid/net),
  runs the daemon readiness/control-pipe handshake, applies best-effort
  shell-free network hardening (loopback, veth, IPv6 route flushing) via raw
  rtnetlink, then pauses until SIGTERM. It is a near-leaf syscall crate with no
  tokio dependency, folding the namespace-holder handshake and the unshare(1)
  launcher flags into one in-process step.
- `eos-runner` — The single-threaded, no-tokio namespace runner the eosd daemon
  execs as a dedicated child to perform the kernel syscalls (unshare, setns,
  mount) that require a single-threaded caller. It owns the
  runner's request/result wire types, the overlay-mount inversion port
  (KernelMountPort), the thiserror failure enum, and the fresh-ns / setns
  execution paths.
- `eos-layerstack` — Durable-truth storage layer of the eosd runtime: it owns
  the single linearization point (one mutable manifest.json over immutable,
  content-addressed layer directories swapped by an atomic pointer write). Item
  groups cover
  the storage facade and merged read view, the dual-set snapshot lease
  registry, non-destructive checkpoint squashing, the dual-layer
  cross-process/in-process writer lock, and workspace base construction/binding.
- `eos-overlay` — The eosd runtime's overlayfs kernel-mount and upper-dir
  capture leaf: it builds workspace overlay mounts via the raw new-mount API
  (fsopen/fsconfig/fsmount/move_mount), allocates the writable upper/work side,
  walks the upperdir to capture a policy-blind write set, and converts captured
  changes one-way into eos_protocol::LayerChange for OCC publication. Item
  groups cover errors (OverlayError, Result), mount mechanics (OverlayHandle,
  OverlayMount, ValidatedMountInputs, MountIo), writable-dir allocation
  (OverlayWritableDirs), and the captured-change model (OverlayPathChange,
  OverlayPathChangeKind).
- `eos-occ` — The optimistic-concurrency commit layer of the eosd runtime: it
  owns the MF-1 single-writer publish decision gate, batching N disjoint
  file-API writes into one manifest CAS attempt per layer_stack_root and routing
  each path to drop/direct/gated/reject. Its items cover route classification
  and per-path outcomes, the single-writer commit queue with its inverted
  transaction port, the changeset-preparing service plus route-provider port,
  and the crate-local error algebra.
- `eos-isolated` — Owns the isolated-workspace subsystem: a persistent,
  network-isolated PRIVATE session whose writes are captured for audit only and
  never published (enforced at build time by not depending on eos-occ). Provides
  env-sourced lifecycle caps, the eos-shared0 bridge plus per-workspace
  veth/nftables wiring, the enter/exit session orchestrator with inverted
  snapshot-lease and namespace-runtime ports, an append-only JSONL audit sink,
  and a wire-mapped lifecycle error.
- `eos-plugin` — Owns the pure plugin PPC contract layer of the eosd runtime:
  plugin/service manifests, validated service keys and status,
  daemon-to-harness refresh messages, public op-name registration, and
  bidirectional message-id'd PPC frames. It deliberately holds no process,
  overlay, OCC, or namespace state (those stay in eos-daemon); it exposes a
  typed daemon-owned service-process contract.
- `eos-daemon` — The `eosd` tokio control-plane crate: it runs the protocol-v1
  RPC server on an AF_UNIX socket plus a loopback-TCP listener, routes ops
  through the `OpTable` dispatcher, and owns the daemon-side impure ports of the
  runtime control plane. Item groups cover the RPC server/config,
  the op dispatcher with its per-root OCC single-writer service cache and
  LayerStack commit/route providers, the in-flight invocation registry + TTL
  reaper, the audit ring buffer, the command-session runtime, the daemon-local
  isolated-workspace lifecycle and its inverted snapshot/namespace ports, and the
  plugin subsystem (PPC transport, process specs, OCC callbacks, route state).
- `eosd` — Binary entry point of the eosd runtime: a single static binary that
  parses argv and dispatches to three library entry points (`daemon` async RPC
  server, `ns-runner` namespace tool runner, `ns-holder` isolated-namespace
  holder) while preserving each library's typed exit codes. Its only items are
  two argv-parser config structs plus a local overlay-mount adapter implementing
  `eos_runner::KernelMountPort`.
- `eos-terminal-pair` — Provides safe allocation of a PTY controller/attached
  File pair for command sessions in the eosd shell-execution path, wrapping the
  Linux posix_openpt/grantpt/unlockpt/ptsname_r FFI sequence behind a single
  owned struct. Its sole item is the TerminalPair handle returned by the crate's
  open_terminal_pair function.
- `xtask` — The dev-only build and release tooling binary for the eosd runtime
  workspace. It packages the eosd daemon for musl Linux targets, copies and
  chmods the artifact, and emits checksums, a protocol-version stamp, a JSON
  manifest, and optional minisign signatures; it is never linked into the
  runtime.
