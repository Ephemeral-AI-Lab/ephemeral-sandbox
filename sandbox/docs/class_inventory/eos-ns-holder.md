# Crate `eos-ns-holder` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/eos-ns-holder/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**12 items (10 structs, 2 enums, 0 traits, 0 type aliases) across 1 file.**

`eos-ns-holder` is the dedicated single-threaded child of the `eosd` runtime that `unshare`s and pins the isolated workspace's full namespace stack (`CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWNET`), runs the readiness/control-pipe handshake with the daemon, applies best-effort shell-free network hardening, then `pause()`s until `SIGTERM`. Its items group into the handshake state machine (`Handshake`, `HandshakeState`, `NsHolderError`), the pinned-namespace RAII holders (`HeldNamespaces`, `PidNamespaceInit`), and the Linux-gated rtnetlink wire structs used to configure loopback/veth/routes (`NetworkConfig`, `NetlinkAttr`, `IfInfoMsg`, `IfAddrMsg`, `RouteMsg`, `NetlinkSocketAddress`).

## Contents

- **`eos-ns-holder/src/lib.rs`** — `NsHolderError`, `HeldNamespaces`, `PidNamespaceInit`, `NetworkConfig`, `HandshakeState`, `Handshake`, `NetlinkAttr`, `IfInfoMsg`, `IfAddrMsg`, `RouteMsg`, `NetlinkSocketAddress`, `ParentIds`

---

## `eos-ns-holder/src/lib.rs`

#### `NsHolderError`  ·  _enum_  ·  derives: `Debug, thiserror::Error`  ·  `#[non_exhaustive]`  ·  [L99]

Failures raised by the holder lifecycle; variants carry the holder's exit-code contract so daemon-side recovery can map them to process exit codes.

**Variants**: `Unshare`, `ControlPipeClosed`, `UnexpectedToken`, `PipeIo(std::io::Error)`, `SetupIo { path: String, source: std::io::Error }`, `TestCrash`

#### `HeldNamespaces`  ·  _struct_  ·  derives: `Debug`  ·  [L149]

The namespace FDs the holder pins open for its whole lifetime; wrapping `OwnedFd` gives RAII close-on-drop so the kernel tears the namespaces down when the holder exits.

**Fields**

| name | type | vis |
|------|------|-----|
| `user` | `OwnedFd` | `pub` |
| `mnt` | `OwnedFd` | `pub` |
| `pid` | `OwnedFd` | `pub` |
| `net` | `OwnedFd` | `pub` |
| `_pid_init` | `Option<PidNamespaceInit>` (cfg `target_os = "linux"`) |  |

#### `PidNamespaceInit`  ·  _struct_  ·  derives: `Debug`  ·  [L164]

Linux-only handle to the forked PID-namespace init child (PID 1 of the new pidns); its `Drop` best-effort SIGTERMs and reaps the child. (cfg `target_os = "linux"`)

**Fields**

| name | type | vis |
|------|------|-----|
| `pid` | `libc::pid_t` |  |

<details><summary>Methods (1)</summary>

`drop`

</details>

#### `NetworkConfig`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L183]

Parsed veth configuration (interface, namespace IP, prefix length, gateway) carried in the optional `net-ready` control-pipe line.

**Fields**

| name | type | vis |
|------|------|-----|
| `iface` | `String` |  |
| `ns_ip` | `Ipv4Addr` |  |
| `prefix_len` | `u8` |  |
| `gateway` | `Ipv4Addr` |  |

#### `HandshakeState`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq`  ·  `#[non_exhaustive]`  ·  [L197]

Where the handshake driver currently is; total, ordered transitions `Unshared → ProcBound → NsUpSent → NetReadyReceived → Ready → Paused`.

**Variants**: `Unshared`, `ProcBound`, `NsUpSent`, `NetReadyReceived`, `Ready`, `Paused`

#### `Handshake`  ·  _struct_  ·  derives: `Debug`  ·  [L219]

Drives the readiness/control handshake over a pair of inherited pipe FDs, holding the pinned `HeldNamespaces` so they outlive the handshake and tracking the current `HandshakeState`.

**Fields**

| name | type | vis |
|------|------|-----|
| `readiness_fd` | `RawFd` |  |
| `control_fd` | `RawFd` |  |
| `state` | `HandshakeState` |  |
| `network_config` | `Option<NetworkConfig>` |  |
| `_namespaces` | `HeldNamespaces` |  |

<details><summary>Methods (5)</summary>

`new`, `state`, `signal_ns_up`, `await_net_ready`, `finish_ready`

</details>

#### `NetlinkAttr`  ·  _struct_  ·  [L735]

Linux-only rtnetlink attribute (type + value bytes) appended to netlink messages for address and route configuration. (cfg `target_os = "linux"`)

**Fields**

| name | type | vis |
|------|------|-----|
| `kind` | `u16` |  |
| `value` | `Vec<u8>` |  |

<details><summary>Methods (1)</summary>

`new`

</details>

#### `IfInfoMsg`  ·  _struct_  ·  `#[repr(C)]`  ·  [L753]

Linux-only `repr(C)` mirror of the kernel `ifinfomsg` carried by `RTM_NEWLINK` to bring an interface up. (cfg `target_os = "linux"`)

**Fields**

| name | type | vis |
|------|------|-----|
| `ifi_family` | `u8` |  |
| `ifi_pad` | `u8` |  |
| `ifi_type` | `u16` |  |
| `ifi_index` | `i32` |  |
| `ifi_flags` | `u32` |  |
| `ifi_change` | `u32` |  |

#### `IfAddrMsg`  ·  _struct_  ·  `#[repr(C)]`  ·  [L768]

Linux-only `repr(C)` mirror of the kernel `ifaddrmsg` carried by `RTM_NEWADDR` to add an IPv4 address. (cfg `target_os = "linux"`)

**Fields**

| name | type | vis |
|------|------|-----|
| `ifa_family` | `u8` |  |
| `ifa_prefixlen` | `u8` |  |
| `ifa_flags` | `u8` |  |
| `ifa_scope` | `u8` |  |
| `ifa_index` | `u32` |  |

#### `RouteMsg`  ·  _struct_  ·  `#[repr(C)]`  ·  [L782]

Linux-only `repr(C)` mirror of the kernel `rtmsg` carried by `RTM_NEWROUTE`/`RTM_DELROUTE` to add a default route or flush the IPv6 default route. (cfg `target_os = "linux"`)

**Fields**

| name | type | vis |
|------|------|-----|
| `rtm_family` | `u8` |  |
| `rtm_dst_len` | `u8` |  |
| `rtm_src_len` | `u8` |  |
| `rtm_tos` | `u8` |  |
| `rtm_table` | `u8` |  |
| `rtm_protocol` | `u8` |  |
| `rtm_scope` | `u8` |  |
| `rtm_type` | `u8` |  |
| `rtm_flags` | `u32` |  |

#### `NetlinkSocketAddress`  ·  _struct_  ·  `#[repr(C)]`  ·  [L800]

Linux-only `repr(C)` mirror of the kernel `sockaddr_nl` used as the destination address when sending rtnetlink messages. (cfg `target_os = "linux"`)

**Fields**

| name | type | vis |
|------|------|-----|
| `nl_family` | `libc::sa_family_t` |  |
| `nl_pad` | `u16` |  |
| `nl_pid` | `u32` |  |
| `nl_groups` | `u32` |  |

#### `ParentIds`  ·  _struct_  ·  [L831]

Function-local helper inside `unshare_namespace_stack` that snapshots the parent process's uid/gid before entering the new user namespace so they can be written into the uid/gid maps. (cfg `target_os = "linux"`)

**Fields**

| name | type | vis |
|------|------|-----|
| `user` | `u32` |  |
| `group` | `u32` |  |
