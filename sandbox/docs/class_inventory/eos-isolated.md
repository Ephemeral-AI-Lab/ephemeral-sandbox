# Crate `eos-isolated` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/eos-isolated/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**15 items (10 structs, 2 enums, 3 traits, 0 type aliases) across 5 files.**

`eos-isolated` ports the Python `isolated_workspace` subsystem of `backend/src/sandbox`: a persistent, network-isolated PRIVATE session whose writes are captured for AUDIT ONLY and NEVER published (enforced at build time by not depending on `eos-occ`). Its item groups are the env-sourced lifecycle caps (`ResourceCaps`, `Rfc1918Egress`), the `eos-shared0` bridge + per-workspace veth/nftables wiring (`IsolatedNetwork`, `BridgeAddressPool`, `VethAllocation`), the enter/exit session orchestrator with its inverted snapshot/lease and namespace-runtime ports (`IsolatedSession`, `LayerStackSnapshotPort`, `NamespaceRuntimePort`, `WorkspaceHandle`, `SnapshotLease`), the append-only JSONL audit sink (`AuditSink`, `JsonlAuditSink`), and the wire-mapped lifecycle error (`IsolatedError`).

## Contents

- **`eos-isolated/src/audit.rs`** — `AuditSink`, `JsonlAuditSink`
- **`eos-isolated/src/caps.rs`** — `Rfc1918Egress`, `ResourceCaps`
- **`eos-isolated/src/error.rs`** — `IsolatedError`
- **`eos-isolated/src/network.rs`** — `VethAllocation`, `BridgeAddressPool`, `IsolatedNetwork`
- **`eos-isolated/src/session.rs`** — `AgentId`, `WorkspaceHandleId`, `SnapshotLease`, `WorkspaceHandle`, `LayerStackSnapshotPort`, `NamespaceRuntimePort`, `IsolatedSession`

---

## `eos-isolated/src/audit.rs`

#### `AuditSink`  ·  _trait_  ·  [L30]

Sink for isolated-workspace audit events; the trait exists so tests can substitute a recording double without touching the filesystem.

<details><summary>Methods (1)</summary>

`emit`

</details>

#### `JsonlAuditSink`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L42]

Append-only JSONL audit sink. Audit-only; no OCC linkage.

**Fields**

| name | type | vis |
|------|------|-----|
| `path` | `PathBuf` |  |

<details><summary>Methods (3)</summary>

`new`, `from_env`, `emit`

</details>

---

## `eos-isolated/src/caps.rs`

#### `Rfc1918Egress`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  [L31]

RFC1918 egress policy: `allow` (default) leaves private-network egress open; `deny` installs the RFC1918 drop rules.

**Variants**: `Allow`, `Deny`

#### `ResourceCaps`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq`  ·  [L41]

Resource caps + lifecycle config; the `Default` impl is the byte-for-byte `from_env` result with no env overrides set.

**Fields**

| name | type | vis |
|------|------|-----|
| `enabled` | `bool` | `pub` |
| `ttl_s` | `f64` | `pub` |
| `total_cap` | `u32` | `pub` |
| `upperdir_bytes` | `u64` | `pub` |
| `memavail_fraction` | `f64` | `pub` |
| `setup_timeout_s` | `f64` | `pub` |
| `exit_grace_s` | `f64` | `pub` |
| `rfc1918_egress` | `Rfc1918Egress` | `pub` |
| `fallback_dns` | `String` | `pub` |
| `sample_interval_s` | `f64` | `pub` |

<details><summary>Methods (2)</summary>

`default`, `from_env`

</details>

---

## `eos-isolated/src/error.rs`

#### `IsolatedError`  ·  _enum_  ·  derives: `Debug, thiserror::Error`  ·  `#[non_exhaustive]`  ·  [L15]

Lifecycle error for the enter/exit isolated-workspace flow; each variant's `kind()` reproduces the Python `kind` string fed onto the daemon RPC response envelope.

**Variants**: `FeatureDisabled`, `InvalidArgument(String)`, `AlreadyOpen { created_at: f64, last_activity: f64 }`, `NotOpen`, `QuotaExceeded { total_cap: u32 }`, `HostRamPressure { required_bytes: u64, budget_bytes: u64 }`, `SetupTimeout { step: String }`, `SetupFailed { step: String }`, `NetworkUnavailable(String)`, `AuditWrite { path: PathBuf, source: std::io::Error }`

<details><summary>Methods (1)</summary>

`kind`

</details>

---

## `eos-isolated/src/network.rs`

#### `VethAllocation`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L67]

One veth `/32` allocation for a workspace.

**Fields**

| name | type | vis |
|------|------|-----|
| `host_name` | `String` | `pub` |
| `ns_name` | `String` | `pub` |
| `ns_ip` | `Ipv4Addr` | `pub` |

#### `BridgeAddressPool`  ·  _struct_  ·  derives: `Debug, Clone, Default`  ·  [L97]

Pure IPv4 `/32` allocator over `10.244.0.2 - 10.244.0.254`; lowest-IP-first O(N) scan with no Linux deps.

**Fields**

| name | type | vis |
|------|------|-----|
| `allocated` | `Vec<Ipv4Addr>` |  |

<details><summary>Methods (4)</summary>

`new`, `reserve`, `allocate`, `free`

</details>

#### `IsolatedNetwork`  ·  _struct_  ·  derives: `Debug`  ·  [L163]

Owns the `eos-shared0` bridge + static nft rules + per-workspace veth wiring, replacing the Python `ip`/`nft` shell-out path with `rtnetlink` and `NETLINK_NETFILTER` messages.

**Fields**

| name | type | vis |
|------|------|-----|
| `rfc1918_egress` | `Rfc1918Egress` |  |
| `pool` | `BridgeAddressPool` |  |
| `initialized` | `bool` |  |

<details><summary>Methods (7)</summary>

`new`, `initialized`, `initialize`, `install_veth`, `teardown_veth`, `teardown_host_veth`, `reserve_persisted_ip`

</details>

---

## `eos-isolated/src/session.rs`

#### `AgentId`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Hash`  ·  [L31]

Newtype for an agent identity (the enter/exit key).

**Fields**

| name | type | vis |
|------|------|-----|
| `0` | `String` | `pub` |

#### `WorkspaceHandleId`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Hash`  ·  [L35]

Newtype for a per-workspace handle id.

**Fields**

| name | type | vis |
|------|------|-----|
| `0` | `String` | `pub` |

#### `SnapshotLease`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L44]

A snapshot lease borrowed from the layer stack (snapshot/lease HINGE only); carries the lease id, manifest coordinates, and lower-layer paths the overlay mounts. NEVER a publish transaction.

**Fields**

| name | type | vis |
|------|------|-----|
| `lease_id` | `String` | `pub` |
| `manifest_version` | `i64` | `pub` |
| `root_hash` | `String` | `pub` |
| `layer_paths` | `Vec<String>` | `pub` |

#### `WorkspaceHandle`  ·  _struct_  ·  derives: `Debug, Clone`  ·  [L58]

Per-workspace state. Not a subclass of any overlay handle (C1).

**Fields**

| name | type | vis |
|------|------|-----|
| `workspace_handle_id` | `WorkspaceHandleId` | `pub` |
| `agent_id` | `AgentId` | `pub` |
| `lease_id` | `String` | `pub` |
| `manifest_version` | `i64` | `pub` |
| `manifest_root_hash` | `String` | `pub` |
| `workspace_root` | `String` | `pub` |
| `scratch_dir` | `PathBuf` | `pub` |
| `upperdir` | `PathBuf` | `pub` |
| `workdir` | `PathBuf` | `pub` |
| `layer_paths` | `Vec<String>` | `pub` |
| `ns_fds` | `HashMap<String, i32>` | `pub` |
| `holder_pid` | `i32` | `pub` |
| `readiness_fd` | `i32` | `pub` |
| `control_fd` | `i32` | `pub` |
| `veth` | `Option<VethAllocation>` | `pub` |
| `cgroup_path` | `Option<PathBuf>` | `pub` |
| `created_at` | `f64` | `pub` |
| `last_activity` | `f64` | `pub` |

#### `LayerStackSnapshotPort`  ·  _trait_  ·  [L104]

Snapshot/lease HINGE port — the ONLY layer-stack surface isolated models; exposes snapshot/lease + read methods only, never the publish-transaction half.

<details><summary>Methods (3)</summary>

`acquire_snapshot`, `release_lease`, `active_lease_count`

</details>

#### `NamespaceRuntimePort`  ·  _trait_  ·  [L144]

Kernel-touching namespace operations the pipeline delegates to (inverted port spawning `eosd ns-holder` and driving `setns` mounts/exec via `eosd ns-runner`).

<details><summary>Methods (7)</summary>

`spawn_ns_holder`, `open_ns_fds`, `mount_overlay`, `configure_dns`, `signal_net_ready`, `create_cgroup`, `kill_holder`

</details>

#### `IsolatedSession`  ·  _struct_  ·  generics: `<S, R, A>`  ·  [L227]

Owns the isolated-workspace lifecycle, namespace runtime, capacity, TTL, and GC; generic over the injected snapshot/lease + namespace ports and audit sink so `eos-daemon` wires kernel-backed implementations and tests inject doubles.

**Fields**

| name | type | vis |
|------|------|-----|
| `caps` | `ResourceCaps` |  |
| `layer_stack` | `S` |  |
| `runtime` | `R` |  |
| `audit` | `A` |  |
| `network` | `IsolatedNetwork` |  |
| `scratch_root` | `PathBuf` |  |
| `handles` | `HashMap<WorkspaceHandleId, WorkspaceHandle>` |  |
| `by_agent` | `HashMap<AgentId, WorkspaceHandleId>` |  |

<details><summary>Methods (29)</summary>

`new`, `with_scratch_root`, `initialize`, `enter`, `exit`, `ttl_sweep`, `get_handle`, `list_open_agents`, `record_tool_call`, `session_scratch_root`, `persisted_handles_path`, `persist_handles`, `reap_startup_orphans`, `reap_orphan_resources`, `read_persisted_handle_rows`, `reap_persisted_lease`, `reap_persisted_holder`, `reap_persisted_veth`, `reap_persisted_cgroup`, `reap_persisted_scratch`, `reap_named_orphans`, `reap_named_veth_orphans`, `reap_named_cgroup_orphans`, `reap_named_scratch_orphans`, `emit_gc_orphan`, `check_host_capacity`, `wire_handle`, `rollback_partial`, `teardown_handle`

</details>
