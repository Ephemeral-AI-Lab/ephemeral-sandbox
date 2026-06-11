# Cross-repo contract: protocol version + fixture pin

The sandbox system pins two version surfaces that must move deliberately.
A careless bump silently breaks the thin-client handshake or the on-disk
manifest read path. The binding host↔box artifact is `contract/`
(`ops.json` + `PROTOCOL.md` + `fixtures/`); no compiled code crosses that
boundary, and `cargo run -p xtask -- check-contract` is the drift gate.

## 1. Wire protocol version

- `DAEMON_PROTOCOL_VERSION = 1`
- Carried as the `_eos_daemon_protocol_version` field **inside `args`** on every
  request. The daemon does **not** gate on it today (inert hook); it is present
  so a future version can branch.
- Pinned in three places that the conformance suites hold in lockstep:
  - `contract/ops.json` (`protocol_version`) — the reviewed artifact;
  - `crates/eos-daemon/src/wire/version.rs` — the box side;
  - `crates/eos-sandbox-host/src/protocol.rs` — the host side's deliberate copy
    (no shared crate; drift is caught by the fixture conformance tests, not
    the compiler).

## 2. On-disk manifest schema version

- `MANIFEST_SCHEMA_VERSION = 1`
- Stamped into the persisted layer-stack manifest. The CAS `manifest_root_hash`
  hashes **only** the `layers` array, never `version`/`schema_version`, so the
  schema version can change without invalidating existing layer hashes — but a
  reader that does not understand a new schema version must refuse to load it.
- Source of truth: `crates/eos-layerstack/src/model.rs` (`MANIFEST_SCHEMA_VERSION`).

## 3. Bump procedure

When either version must change:

1. Bump the constant in its owning location(s) above, regenerate
   `contract/ops.json` (`cargo run -p eosd -- dump-ops > contract/ops.json`)
   and `docs/API.md` (`cargo run -p xtask -- gen-docs`), and update this file
   in the same change. `check-contract` enforces the lockstep.
2. The golden fixtures (`contract/fixtures/`) are **immutable ground truth**
   captured from the original Python runtime, which has been removed — they
   can no longer be regenerated. Never edit a fixture to match code. One
   deliberate exception: when the legacy `api.*` aliases were retired
   (2026-06), the `op` field of the three request fixtures was rewritten to
   the canonical `sandbox.*` spellings. Every other byte — args, response
   envelopes, timing keys — remains the original capture.

Until such a change, both versions are pinned at `1`.
