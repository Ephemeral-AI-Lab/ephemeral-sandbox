---
title: Daemon Command Child Policy Lite — Spec
tags:
  - ephemeral-os
  - sandbox
  - security
  - implementation-plan
  - spec
status: draft
supersedes: daemon-command-syscall-hardening-spec.md
---

# Daemon Command Child Policy Lite — Spec

This replaces the full default-deny seccomp plan with a smaller, more generic
policy. The product model is hierarchical:

| Tier | Process | Privilege policy |
|---|---|---|
| Setup admin | daemon, ns-holder, ns-runner | Keep the caps and syscalls needed for mount, namespace, layerstack, workspace, and network setup. |
| User command | shell command child only | Apply `no_new_privs`, targeted cap drop, and a small seccomp deny table immediately before `execve`. |
| Outer container | Docker container | Later reduce container privileges only after the setup path proves which caps it actually needs. |

The policy is still installed at the command-child `pre_exec` boundary. It must
not apply to the daemon, ns-holder, ns-runner helper, overlay/layerstack setup,
workspace setup, or network setup.

## Decision

Implement a command-child policy with:

1. `setpgid(0, 0)` as today.
2. `prctl(PR_SET_NO_NEW_PRIVS, 1, ...)`.
3. Targeted capability drop in the command child only.
4. A seccomp-lite denylist filter for dangerous syscall families.

Do not implement:

- Docker-default allowlist vendoring.
- Default-deny seccomp.
- Denied-syscall telemetry.
- `/dev/kmsg` parsing.
- Chrome/Playwright/Puppeteer/Bazel/rootless-Docker compatibility signoff.

Those are either too heavy for the current need or not real supported workloads.

## Goals

- Keep daemon setup fully functional.
- Keep normal command execution, shells, package managers, compilers, and tests
  working.
- Reject the syscall families that can undo namespace/mount assumptions from
  user command code.
- Use one Linux command-child mechanism that works inside Docker on Linux,
  macOS, and Windows. On macOS/Windows it protects Docker's Linux kernel, not the
  host OS kernel.

## Non-goals

- No Phase 2 telemetry in this adjusted plan.
- No full default-deny allowlist.
- No host-native macOS or Windows process sandbox.
- No Docker-in-Docker/rootless-container support unless product explicitly adds
  that workload.
- No browser sandbox support guarantee in `enforce`; browser tests can use
  `--no-sandbox` if they become a real workload.

## Phase 1 — Command Child Policy

### Patch Point

Only extend the command-child hook in:

`crates/sandbox-runtime/namespace-process/src/runner/shell_exec.rs`

Order:

```text
setpgid(0, 0)
set_no_new_privs()
drop_command_child_capabilities()
install_seccomp_lite_filter()
execve(user command)
```

The hook must stay async-signal-safe: build seccomp programs before `fork`; in
the child call only syscall wrappers over prebuilt data.

### Capability Policy

Capabilities are per-process. Keep setup privileges on daemon/setup processes;
drop dangerous caps only in the command child.

| Drop in command child | Reason |
|---|---|
| `SYS_ADMIN` | Blocks most mount and namespace admin operations by capability. |
| `NET_ADMIN` | Blocks network administration from untrusted commands. |
| `SYS_MODULE`, `SYS_BOOT`, `SYS_RAWIO`, `SYS_TIME`, `SYS_PACCT`, `SYS_TTY_CONFIG` | Kernel/system control. |
| `BPF`, `PERFMON`, `AUDIT_CONTROL`, `AUDIT_READ`, `AUDIT_WRITE`, `SYSLOG` | Kernel observability and audit surfaces. |
| `MAC_ADMIN`, `MAC_OVERRIDE`, `LINUX_IMMUTABLE`, `IPC_LOCK`, `IPC_OWNER`, `WAKE_ALARM`, `BLOCK_SUSPEND`, `CHECKPOINT_RESTORE`, `LEASE`, `SYS_NICE` | Not needed for normal command workloads. |

| Keep in command child | Reason |
|---|---|
| `CHOWN`, `DAC_OVERRIDE`, `DAC_READ_SEARCH`, `FOWNER`, `FSETID` | Package managers and root-like overlay writes. |
| `SETUID`, `SETGID`, `SETFCAP` | Package installs and file metadata. |
| `KILL`, `NET_BIND_SERVICE`, `NET_RAW` | Common process/network behavior. |
| `MKNOD` | Kept for compatibility; seccomp-lite blocks char/block device nodes by mode. |

Mechanics:

1. Clear ambient caps.
2. Drop each non-kept cap from the bounding set.
3. `capset` effective/permitted/inheritable to the keep set.

### Seccomp-Lite Deny Table

This is an exact reject table, not a default-deny allowlist.

| Family | Syscalls | Return |
|---|---|---|
| Mount mutation | `mount`, `umount2`, `pivot_root`, `move_mount`, `open_tree`, `fsopen`, `fsconfig`, `fsmount`, `fspick`, `mount_setattr` | `EPERM` |
| Namespace mutation | `setns`, `unshare` | `EPERM` |
| Namespace via clone | `clone` when flags contain any `CLONE_NEW*` bit | `EPERM` |
| Opaque clone args | `clone3` | `ENOSYS` |
| Kernel module/control | `init_module`, `finit_module`, `delete_module`, `kexec_load`, `kexec_file_load`, `reboot` | `EPERM` |
| Kernel attack surfaces | `bpf`, `perf_event_open`, `userfaultfd`, `fanotify_init`, `io_uring_setup`, `io_uring_enter`, `io_uring_register` | `EPERM` |
| Device nodes | `mknod`, `mknodat` when mode is char/block | `EPERM` |
| Handle/keyring/resource escape | `open_by_handle_at`, `add_key`, `request_key`, `keyctl`, `swapon`, `swapoff`, `quotactl` | `EPERM` |

Everything else defaults to allow. This preserves compatibility and avoids
owning Docker's full allowlist.

### Config

Keep the config small:

```yaml
manager:
  command_security:
    mode: enforce   # enforce | off
```

- `enforce`: NNP + cap drop + seccomp-lite deny table.
- `off`: NNP + cap drop only; no seccomp filter.

Do not add `relaxed` until a real supported workload needs namespace creation
from inside the command child.

## Phase 2 — Removed

There is no Phase 2 telemetry in this adjusted plan.

Rationale:

- Kernel seccomp logs require `/dev/kmsg` or audit integration.
- Request-path `/dev/kmsg` scanning risks performance regressions.
- The current product need is enforcement, not deny analytics.

Any future telemetry must be designed as a bounded, opt-in collector and must
not run on the request path.

## Phase 3 — Simplified Outer Container Hardening

Phase 3 is separate from the command-child policy. Its job is to reduce the
Docker container privilege surface without breaking daemon setup.

Keep it small:

1. Inventory the real daemon/setup requirements from live product workflows.
2. Replace `--privileged` with the minimal Docker caps/devices/security opts
   that still pass those workflows.
3. Keep command-child Phase 1 in place even after outer hardening.

Candidate Docker direction, subject to live proof:

- Add only needed caps such as `SYS_ADMIN`, `NET_ADMIN`, and `MKNOD` to the
  container if setup still needs them.
- Keep daemon/setup paths privileged enough to mount, set up namespaces, and
  manage layerstack/workspace state.
- Avoid container-level seccomp profiles until they are proven not to block the
  daemon, ns-holder, or ns-runner setup path.

Phase 3 tests must cover real product paths only:

- create sandbox
- daemon ready
- normal `exec_command`
- workspace read/write
- overlay/layerstack whiteout behavior
- package install smoke
- destroy cleanup

Do not add Chrome/Playwright/Puppeteer/Bazel/rootless Docker/Podman to Phase 3
unless those become explicit supported workloads.

## Verification

Build/unit:

```sh
cargo fmt --check
cargo clippy --all-targets
cargo test -p sandbox-runtime-namespace-process
```

Live:

```sh
bin/start-sandbox-docker-gateway --rebuild-binary
cd cli-operation-e2e-live-test
E2E_REBUILD_BINARY=1 pytest runtime/command_security -v
```

Required evidence:

- aarch64 native Docker path green.
- x86_64 native Docker path green.
- No VM/QEMU/emulated Docker path counts as x86_64 seccomp evidence.
- macOS/Windows support means Docker's Linux kernel path works there; it does
  not imply native host-kernel syscall filtering.

## Acceptance

- User command child shows `NoNewPrivs=1`.
- User command child has dangerous caps removed and package-manager caps kept.
- Deny-table syscalls fail as specified.
- Normal command, package install, overlay/layerstack, workspace, and cleanup
  workflows still pass.
- No request-path telemetry or `/dev/kmsg` scanning exists.
