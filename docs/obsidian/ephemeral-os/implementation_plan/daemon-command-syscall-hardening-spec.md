---
title: Daemon Command Syscall Hardening — Spec
tags:
  - ephemeral-os
  - sandbox
  - security
  - implementation-plan
  - spec
status: draft
supersedes: daemon-command-syscall-hardening.md
---

# Daemon Command Syscall Hardening — Spec

This spec refines [[daemon-command-syscall-hardening]] into an implementable
design. It keeps that plan's core decision (a Linux-only, image-agnostic syscall
policy installed by the daemon at the user-command boundary) and corrects two
issues found while verifying it against the code and against live Docker
experiments:

1. The capability policy as originally written ("drop command-child
   capabilities") breaks package managers, contradicting the plan's own
   compatibility contract. This spec replaces it with a **targeted** drop.
2. The seccomp policy was not multi-arch-safe. This spec adds an **arch guard**,
   which is what actually makes the policy portable across Apple Silicon
   (aarch64) and x86_64 hosts.

It also refines the deny set (mode-based `mknod`, `io_uring`, keep `ptrace`
usable) and adds one escape-hatch config field.

The runnable test catalog for this spec is
[[daemon-command-syscall-hardening-test-case]].

## Decision

Install an inward syscall boundary on the **user-command child only**, at
`pre_exec`, in three layers applied in this order:

1. `no_new_privs`
2. targeted capability drop (drop system-power caps, keep filesystem/identity caps)
3. a seccomp-BPF filter (default-deny allowlist, arch-guarded)

Do **not** apply it to the daemon, the ns-holder, the ns-runner helper, overlay
mounting, or network setup — those paths legitimately need privileged syscalls
that run *before* the user command starts.

This is not a userspace kernel and does not replace de-privileging the
container (that is the separate *outward* defense; see
[Layered model](#layered-model-inward-vs-outward)). It is the smallest useful
patch that stops untrusted command code from using kernel APIs that can undo the
sandbox's namespace/mount assumptions or reach the host kernel — and it delivers
its largest value *today*, while the container is still `--privileged`.

## Goals

- Block mount, namespace, module, BPF, tracing-escape, keyring, and
  handle-based-escape syscalls for user commands.
- Work on **any Docker image** with no in-image dependency (the policy travels
  in the daemon binary).
- Work identically on **Linux, macOS, and Windows** hosts.
- Keep the interactive/build/test workload contract intact — including
  `apt`/`apk`/`dnf` installs inside the overlay.

## Non-goals

- No gVisor / `runsc`, no custom userspace kernel.
- No image-specific package installation, no host-specific setup.
- No container-level (OCI) seccomp profile in v1 (it would also constrain the
  daemon's own privileged setup — see [Why pre_exec](#why-pre_exec-not-a-container-level-profile)).
- Container de-privileging and userns-remap are tracked separately.

## Current state (verified)

- Single user-command exec path: `run()` → `run_setns` → `shell::run_setns` →
  `shell_exec::execute_shell`
  (`crates/sandbox-runtime/namespace-process/src/runner/mod.rs:47`,
  `.../runner/setns.rs:24`, `.../runner/setns/shell.rs:9`,
  `.../runner/shell_exec.rs:28`). The command child's `pre_exec` today is
  `install_command_process_group` and does only `setpgid(0,0)`
  (`.../runner/shell_exec.rs:88-102`). **This is the sole patch point.**
- The other `pre_exec`, `install_pgid_leader_hook`
  (`crates/sandbox-runtime/namespace-execution/src/launcher.rs:379-392`), is on
  the **ns-runner helper** (`current_exe() ns-runner`), which must keep its
  privileges to `setns`/`mount`. The policy must **not** go here.
- No existing hardening: the only `prctl` in production is `PR_SET_PDEATHSIG`
  (`.../holder/namespace.rs:189`, `.../holder/mod.rs:155`). No `no_new_privs`,
  no seccomp, no capability drop anywhere.
- The user command runs as **mapped root in a per-command user namespace**
  (uid_map `0 <container-uid> 1`, `.../holder/namespace.rs:80-95`) and in a
  child **PID namespace** (`unshare(NEWPID)` + fork, `.../holder/namespace.rs:158-174`).
  This is why "root in the sandbox" is expected to install packages, and why the
  daemon is not ptrace-reachable from the command.
- The daemon is a **static musl binary**, built per-arch
  (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`;
  `xtask/src/main.rs:19-20`). It is uploaded into the container and run directly.
  This is what makes the policy image-agnostic *and* makes `libc::SYS_*` resolve
  to correct per-arch numbers automatically.
- The daemon itself uses `mknod c 0:0` for overlay whiteouts on the layerstack
  volume (`crates/sandbox-runtime/layerstack/src/storage/whiteout.rs:20`) — on
  the daemon path, **not** the command path, so it is unaffected by this filter.

## Design

### Patch point and scope

Extend the existing command-child `pre_exec` in
`crates/sandbox-runtime/namespace-process/src/runner/shell_exec.rs`. The closure
runs post-fork, pre-exec, in the user-command child only:

```text
setpgid(0, 0)                 // existing
apply_command_security_policy // new: NNP -> cap drop -> seccomp
execve(user command)          // must remain permitted by the filter
```

Add one Linux-only module: `crates/sandbox-runtime/namespace-process/src/runner/command_security.rs`, exposing:

```rust
pub(crate) fn apply_command_security_policy(policy: &CommandSecurityPolicy) -> io::Result<()>;
```

with private `set_no_new_privs`, `drop_capabilities`, `install_seccomp_filter`.

### Why `pre_exec`, not a container-level profile

A container-level OCI seccomp/cap profile applies to the whole container,
including the daemon, ns-holder, and ns-runner — which need `mount`, `unshare`,
`setns`, and the `fsopen` mount API to build the sandbox. Installing at the
command child's `pre_exec` is the only point where the daemon has already done
its privileged setup and the *next* thing to run is untrusted. This is the
central design constraint and the reason the policy lives in the daemon binary
rather than in Docker config.

### Order of operations (and why)

1. `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)` — required before an unprivileged
   process may install a seccomp filter, and it neutralizes setuid/file-cap
   privilege gain across the upcoming `execve`.
2. Capability drop (needs `CAP_SETPCAP` in the effective set, so it must precede
   the seccomp filter, which does **not** block `capset`/`prctl`).
3. `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog, 0, 0)`.

All three are async-signal-safe syscalls. See
[Async-signal-safety](#async-signal-safety-hard-requirement).

### Capability policy — targeted drop (correction #1)

The command child is userns-root and legitimately needs the "act as root on the
overlay" capabilities: `apt`/`apk`/`dnf` rely on `CHOWN`, `DAC_OVERRIDE`,
`FOWNER`, `FSETID` (preserve setuid bits on `sudo`/`ping`), and `SETFCAP`
(dpkg/rpm set file caps such as `cap_net_raw` on `ping`). A blanket drop makes
installs fail or silently produce broken binaries. Instead:

| Drop (system-power) | Keep (sandbox-root must work) |
|---|---|
| `SYS_ADMIN`, `SYS_MODULE`, `SYS_RAWIO`, `SYS_BOOT`, `SYS_TIME`, `SYS_PACCT`, `SYS_TTY_CONFIG`, `NET_ADMIN`, `BPF`, `PERFMON`, `AUDIT_CONTROL`, `AUDIT_READ`, `AUDIT_WRITE`, `MAC_ADMIN`, `MAC_OVERRIDE`, `SYSLOG`, `LINUX_IMMUTABLE`, `IPC_LOCK`, `IPC_OWNER`, `WAKE_ALARM`, `BLOCK_SUSPEND`, `CHECKPOINT_RESTORE`, `LEASE`, `SYS_NICE` | `CHOWN`, `DAC_OVERRIDE`, `DAC_READ_SEARCH`, `FOWNER`, `FSETID`, `SETUID`, `SETGID`, `SETFCAP`, `KILL`, `NET_BIND_SERVICE`, `NET_RAW`, `MKNOD` |

Dropping `SYS_ADMIN` from the workload is the single highest-value removal — the
userns+`SYS_ADMIN` pair is the classic escape primitive — and the seccomp filter
closes the syscalls those caps would unlock. Most kept caps are already neutered
against *host* resources inside a non-initial userns, so this is defense in depth
layered under seccomp, not the primary barrier.

Mechanics (all async-signal-safe):

1. `prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0)`.
2. For each cap **not** in the keep set: `prctl(PR_CAPBSET_DROP, cap, 0, 0, 0)`
   (requires effective `SETPCAP`, still held at this point).
3. `capset()` (`_LINUX_CAPABILITY_VERSION_3`) setting effective = permitted =
   inheritable = keep set. This is where effective `SETPCAP` finally goes away
   (it is not in the keep set).

`MKNOD` is kept in caps because device-node creation is instead restricted
precisely, in seccomp, by mode (below).

### Seccomp policy — default-deny allowlist, arch-guarded

**Suggestion: use a default-deny allowlist seeded from Docker's default seccomp
profile**, not a hand-written denylist. Rationale:

- Default-deny is the correct posture for untrusted code and auto-closes
  obscure/future syscalls a denylist would miss.
- Docker's default allow-set is tuned for broad image compatibility, which
  directly serves the "support any image" goal.
- The allow-set is reused as a vetted *list* embedded in our binary; we never
  rely on Docker applying it at runtime (a privileged container disables it).

Filter structure:

1. **Arch guard (correction #2).** Load `seccomp_data.arch`; if it is not the
   native arch (`AUDIT_ARCH_X86_64` or `AUDIT_ARCH_AARCH64` for the respective
   build), `SECCOMP_RET_KILL_PROCESS`. On x86_64 additionally reject any
   `nr >= X32_SYSCALL_BIT (0x40000000)`. Without this, the filter is bypassable
   on x86_64 via the x32/compat ABI, and would silently mis-key on the wrong
   arch. This is what makes one codebase correct on Apple Silicon *and* x86_64.
2. **Explicit denials** applied before the allowlist check (return `EPERM`
   unless noted):

| Category | Syscalls | Return | Notes |
|---|---|---|---|
| mount mutation | `mount`, `umount2`, `pivot_root`, `move_mount`, `open_tree`, `fsopen`, `fsconfig`, `fsmount`, `fspick`, `mount_setattr` | `EPERM` | |
| namespace mutation | `setns`, `unshare` | `EPERM` | |
| namespace via clone | `clone` when `args[0] & CLONE_NEW_MASK != 0` | `EPERM` | mask = `NEWNS\|NEWUTS\|NEWIPC\|NEWUSER\|NEWPID\|NEWNET\|NEWCGROUP\|NEWTIME`; plain fork/thread clones pass |
| clone3 | `clone3` | `ENOSYS` | flags live behind a pointer seccomp can't read; `ENOSYS` makes glibc/musl fall back to `clone`, which the rule above inspects |
| module/kernel control | `init_module`, `finit_module`, `delete_module`, `kexec_load`, `kexec_file_load`, `reboot` | `EPERM` | |
| kernel exploit surfaces | `bpf`, `perf_event_open`, `userfaultfd`, `fanotify_init`, `io_uring_setup`, `io_uring_enter`, `io_uring_register` | `EPERM` | `io_uring` added — repeated escape/LPE surface; runtimes fall back to epoll |
| device nodes | `mknod`/`mknodat` when `mode & S_IFMT ∈ {S_IFCHR, S_IFBLK}` | `EPERM` | refinement: FIFOs/sockets/regular still allowed, so package postinst and normal use keep working |
| handle escape | `open_by_handle_at` | `EPERM` | classic bind-mount ("Shocker") breakout |
| keyrings | `add_key`, `request_key`, `keyctl` | `EPERM` | |
| swap/quota | `swapon`, `swapoff`, `quotactl` | `EPERM` | |

3. **Allowlist:** everything in the Docker-default allow-set that is not denied
   above → `SECCOMP_RET_ALLOW`. Default action for anything else →
   `SECCOMP_RET_ERRNO(EPERM)`. `execve`/`execveat` and the dynamic-linker set
   (`mmap`, `mprotect`, `openat`, `read`, `write`, `futex`, …) are in the
   allow-set — required, since the user program has not `execve`d yet when the
   filter is installed.

Refinements from the original denylist, with rationale:

- **`ptrace` / `process_vm_readv` / `process_vm_writev` are kept allowed.** The
  command runs in its own child PID namespace, so it cannot see — let alone trace
  — the daemon or ns-runner; tracing is confined to the workload's own processes.
  Blocking it would break `gdb`/`strace`/sanitizers, which are common in dev/test
  sandboxes, for no boundary gain. (Revisit if the PID-namespace guarantee ever
  changes.)
- **`mknod` is mode-filtered** rather than fully blocked, so it stops device
  nodes (the real risk, especially while the container is privileged) without
  breaking FIFO/socket creation.

### Escape hatch (one config field)

The plan's "no config flag" stance conflicts with common workloads:
Chrome/Chromium and therefore **Puppeteer/Playwright** headless tests, **Bazel**
sandboxing, bubblewrap/flatpak, and rootless docker/podman-in-sandbox all use
`clone(CLONE_NEWUSER…)`/`unshare`. Add one daemon-config field so operators can
relax the namespace denials per deployment:

```
manager.command_security.mode = "enforce" | "relaxed" | "off"   # default: "enforce"
```

- `enforce` (default): full policy above.
- `relaxed`: allow the namespace-creation syscalls (`unshare`, `setns`,
  `clone(NEW*)`) so nested-sandbox tooling works; all other denials stay.
- `off`: NNP + cap drop only, no seccomp (debug/triage escape hatch).

Keep it a single enum, not per-syscall knobs, until a real workload needs finer
control. Document that browser test runners can also just pass `--no-sandbox`.

### Multi-arch and portability

- The daemon is built per-arch (musl static), so `libc::SYS_*` and the
  `AUDIT_ARCH_*` constant are selected at compile time per target — no runtime
  arch table needed. The in-filter arch guard defends against the *foreign* ABI,
  not against choosing the wrong numbers.
- The policy is a Linux-kernel feature applied to a process that is always
  Linux-in-a-container: native on Linux, the LinuxKit VM on macOS, WSL2/LinuxKit
  on Windows. Verified the Docker VM kernel here is 6.12 with the full seccomp
  action set; Docker's own default seccomp runs there, so `CONFIG_SECCOMP_FILTER`
  is present on all three. On macOS/Windows the policy protects the Linux-VM
  boundary — the only meaningful boundary there.

### Async-signal-safety (hard requirement)

`pre_exec` runs after `fork` in a possibly-multithreaded process; only
async-signal-safe operations are legal. Therefore:

- **Build the BPF program outside `pre_exec`** — construct the `sock_filter`
  array once at process init (or `OnceLock`), and in the child call only
  `prctl`/`capset` syscalls with pointers to already-initialized data. No
  allocation, no `Vec`, no formatting in the closure.
- The child is single-threaded at this point, so no `SECCOMP_FILTER_FLAG_TSYNC`
  is needed; the `prctl` form suffices.

### Dependency decision

Recommendation: take one small, pure-Rust dependency, **`seccompiler`**
(Firecracker's; no C deps; supports x86_64 + aarch64), added once to
`[workspace.dependencies]`. Hand-rolling a correct, arch-guarded, X32-aware BPF
program with raw `libc` is error-prone (jump offsets, the arch preamble, clone
flag arithmetic). This is the one place I'd bend the repo's "existing
dependencies only" guidance, because the failure mode is a silently-open
security filter.

Zero-dependency fallback if a new crate is rejected: emit a `const`/`static`
`sock_filter` array (still built outside `pre_exec`) and install it via `prctl`.
Capability drop uses `libc` (`prctl`, `capset`) either way.

## Implementation layout

### Source (Rust)

```
crates/sandbox-runtime/namespace-process/
├── Cargo.toml                         (edit: + seccompiler.workspace)
├── src/runner/
│   ├── command_security.rs            (NEW — NNP + targeted cap drop + seccomp)
│   ├── shell_exec.rs                  (edit: call apply_command_security_policy in pre_exec)
│   ├── mod.rs                         (edit: `mod command_security;`)
│   └── protocol.rs                    (edit: carry CommandSecurityMode into the runner request)
└── tests/unit/
    └── command_security.rs            (NEW — filter-builder + cap-set unit tests)

crates/sandbox-config/src/configs/manager.rs   (edit: command_security section + enum + default)
Cargo.toml                                       (edit: seccompiler in [workspace.dependencies])
```

Per the repo rule, test code lives in `tests/`, never `src/` — the filter-builder
and cap-set assertions go in `namespace-process/tests/unit/command_security.rs`.

### Live e2e (Python)

```
cli-operation-e2e-live-test/runtime/command_security/
├── __init__.py                        (NEW)
├── helpers.py                         (NEW — embedded musl probe, compile, run/parse, cap-bit decode)
├── test_command_security.py           (NEW — CS-01 … CS-06)
└── test_spec.md                       (NEW — in-tree pointer to the case catalog)
```

The suite reuses `core.cli` and the host-`rustc` compile-and-inject pattern from
`runtime/network_isolation`; it needs no `conftest.py`/`core/` changes. The full
case catalog, probe source, and run matrix live in
[[daemon-command-syscall-hardening-test-case]].

## LOC estimate

| Area | File | Kind | LOC |
|---|---|---|---:|
| Impl | `runner/command_security.rs` | new | ~220 |
| Impl | `runner/shell_exec.rs` | edit | +18 |
| Impl | `runner/mod.rs` | edit | +1 |
| Impl | `runner/protocol.rs` | edit | +8 |
| Impl | `namespace-process/Cargo.toml` | edit | +1 |
| Config | `sandbox-config/.../manager.rs` | edit | +40 |
| Config | workspace `Cargo.toml` | edit | +1 |
| Plumb | daemon/operation → runner request | edit | ~25 |
| Unit | `tests/unit/command_security.rs` | new | ~160 |
| e2e | `command_security/helpers.py` | new | ~210 |
| e2e | `command_security/test_command_security.py` | new | ~200 |
| e2e | `command_security/test_spec.md` | new | ~30 (doc) |
| e2e | `command_security/__init__.py` | new | 1 |
| | | **Total** | **~915** (~730 code) |

Impl ≈ 315 code LOC; e2e ≈ 410 code LOC. The `seccompiler` dependency keeps
`command_security.rs` small; the hand-rolled-BPF fallback pushes it to ~420.

## Compatibility contract

Must keep working (add the first item — it is the one the original plan's cap
policy would have broken):

- **`apk add` / `apt install` / `dnf install` inside the overlay**, including
  chown, setuid-bit preservation, and file-capability setting.
- shell startup; compilers and test runners; process spawning; pipes, PTYs,
  signals; TCP/UDP per the workspace network profile; one-shot and session
  `exec_command`.
- `gdb`/`strace` on the workload's own processes (ptrace kept allowed).

Expected to fail with `EPERM` (in `enforce`):

```sh
mount -t tmpfs tmpfs /tmp/x
umount /eos
unshare -m true
mknod /tmp/disk b 8 0            # device node
python -c 'import ctypes; ctypes.CDLL(None).keyctl(0,0,0,0,0)'
```

Known to require `relaxed` (or `--no-sandbox`): Chrome/Puppeteer/Playwright,
Bazel sandboxing, bubblewrap, rootless docker/podman-in-sandbox.

## Layered model (inward vs outward)

This spec is the **inward** boundary (command → kernel). It is independent of and
complementary to the **outward** boundary (container → host):

| Layer | Defends | Mechanism | Status |
|---|---|---|---|
| Inward (this spec) | host kernel vs. untrusted command | NNP + targeted cap drop + seccomp at `pre_exec` | this doc |
| Outward | host vs. container/daemon | drop `--privileged` → `SYS_ADMIN`+`NET_ADMIN`, seccomp default, `no-new-privileges`; then userns-remap; then Kata per-sandbox kernel | separate track |

Ship inward first: it hardens the *current* privileged containers immediately
with no dependency on the harder de-privileging work, and it stays valuable
afterward (it blocks the userns+`SYS_ADMIN` chain, `io_uring`, and `userfaultfd`
that reduced container caps alone do not fully close).

## Verification

Build/unit:

```sh
cargo test -p sandbox-runtime-namespace-process
cargo clippy --all-targets
cargo fmt
```

Live e2e: the runnable suite is
`cli-operation-e2e-live-test/runtime/command_security/` (rebuild first with
`bin/start-sandbox-docker-gateway --rebuild-binary`). It compiles a musl syscall
probe on the host, bakes it into the workspace, and runs it inside the sandbox —
mirroring `runtime/network_isolation`. Full case catalog, probe source, and run
matrix: [[daemon-command-syscall-hardening-test-case]]. Headline cases:

- CS-01 normal commands + overlay writes unaffected (daemon path intact).
- CS-02 `mount`/`unshare`/device-`mknod`/`keyctl`/`add_key`/`bpf`/`io_uring` → `EPERM`.
- CS-03 FIFO-`mknod`/`ptrace`/DAC-override → `OK` (usability refinements hold).
- CS-04 `NoNewPrivs=1`, `Seccomp=2`; `CapEff` drops SYS_ADMIN/NET_ADMIN/SYS_MODULE,
  keeps CHOWN/DAC_OVERRIDE/FOWNER/SETFCAP.
- CS-05 image `util-linux` tools (`unshare -m`, `mount`, `umount`) rejected.
- CS-06 `apt` operates; networked install guarded by a skip when the profile has
  no egress.

Pass criteria: denied syscalls fail with `EPERM`/`ENOSYS`, the daemon survives
denied syscalls, package managers keep working, destroy still cleans up container
and volumes, and the matrix passes on **both aarch64 and x86_64**.

## Phasing

1. NNP + targeted capability drop + seccomp (`enforce`), `command_security.rs`
   wired into `shell_exec.rs`; config field with `enforce` default.
2. Add allow/deny telemetry counter keyed by blocked syscall name (feeds the
   allowlist tuning).
3. (Separate track) container de-privileging and userns-remap.

## Open questions

- Confirm the shipped toolchains (Go, Node, Bazel, Python test stacks) tolerate
  `ENOSYS`-for-`clone3`; capture any that need `relaxed`.
- Decide whether `relaxed` should be selectable per-sandbox (create-time) rather
  than only per-daemon. Default to per-daemon until a caller needs otherwise.
- Confirm no PTY path in `sandbox-runtime/namespace-execution` execs user argv
  directly (current reading: it only spawns the ns-runner helper).

## References

- Original plan: [[daemon-command-syscall-hardening]]
- Test catalog: [[daemon-command-syscall-hardening-test-case]]
- Docker default seccomp profile (allow-set baseline):
  https://github.com/moby/moby/blob/master/profiles/seccomp/default.json
- seccompiler (pure-Rust BPF compiler): https://github.com/firecracker-microvm/seccompiler
- New mount API (`fsopen`/`fsconfig`/`fsmount`): https://lwn.net/Articles/759499/
- gVisor security model (principle, not dependency):
  https://gvisor.dev/docs/architecture_guide/security/
