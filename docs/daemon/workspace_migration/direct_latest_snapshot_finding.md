# Finding: Direct Latest Workspace Operations Are Head-Based, Not Snapshot-Scoped

Date: 2026-06-17
Status: Open
Severity: Medium

## Summary

The current direct workspace file path does not operate on a single coherent
leased "latest snapshot" of the workspace. Direct file operations read the
active LayerStack head at operation time, then direct write/edit commits carry
the observed base bytes as a hash for OCC. This protects source content from
blind overwrites, but the `manifest_version` attached to the read is sampled
separately from the bytes and should not be treated as a coherent snapshot
identity.

The Pyright plugin path is different: it explicitly acquires the active
LayerStack snapshot, projects that manifest into the configured analyzer
workspace root, and reports a `manifest_key` derived from the leased manifest
version and root hash.

## Evidence

- `crates/daemon/operation/src/file/direct.rs`
  - `DirectBackend::read_bytes` calls `LayerStack::read_bytes_limited(...)` and
    then separately calls `read_active_manifest()` to populate
    `ReadBytes.manifest_version`.
  - `DirectBackend::apply` hashes the previously read base bytes and passes the
    base hash plus sampled manifest version into
    `service::commit_direct_with_options(...)`.
- `crates/daemon/layerstack/src/stack/mod.rs`
  - `LayerStack::read_bytes_limited` reads the active manifest under its own
    shared lock and reads bytes from that manifest.
  - `LayerStack::read_active_manifest` is a separate active-head read.
- `crates/daemon/layerstack/src/service.rs`
  - `commit_direct_with_options` forwards `snapshot_version` and base hashes
    into `apply_changeset_with_base_hashes`.
- `crates/daemon/layerstack/src/commit/worker/transaction.rs`
  - Gated paths validate the current content hash against the supplied
    `base_hash`; a mismatch aborts with `AbortedVersion`.
- `crates/daemon/plugin/src/pyright_lsp/runtime.rs`
  - `ensure_projection_current` acquires a LayerStack snapshot, builds a
    `manifest_key` from `manifest_version` and `root_hash`, projects the leased
    manifest when the key changes, and releases the lease.

## Impact

For direct file operations, "latest" means current active head at the moments
the backend reads and commits, not a pinned snapshot spanning resolve, read,
edit construction, and apply. The path is safe for content OCC because it uses
base-hash validation, but it is not safe to use `ReadBytes.manifest_version` as
proof that the returned bytes came from that exact manifest under concurrent
publishes.

This matters for the workspace migration because future unified workspace APIs
should avoid describing direct file behavior as snapshot-scoped unless the
backend is changed to acquire and carry a real snapshot or the contract is
defined explicitly as head-based with hash-checked OCC.

## Recommendation

Keep the current direct file path contract as head-based plus base-hash OCC, or
introduce a real leased snapshot for direct file read/edit/write flows if future
workspace APIs need coherent snapshot identity. Do not reuse the Pyright
plugin's `manifest_key` semantics for direct file operations without adding an
equivalent snapshot acquisition/projection or read-from-manifest path.
