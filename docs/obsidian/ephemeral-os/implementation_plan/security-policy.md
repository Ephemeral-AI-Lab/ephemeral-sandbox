---
title: Shell-Exec Security Policy — Consolidated Reference
tags:
  - ephemeral-os
  - sandbox
  - security
  - implementation-plan
  - policy
status: living
consolidates:
  - daemon-command-child-policy-refined-spec.md
  - daemon-command-syscall-hardening-spec.md
  - daemon-command-syscall-hardening-test-case.md
  - daemon-command-child-policy-lite-spec.md
---

# Shell-Exec Security Policy — Consolidated Reference

One-page consolidation of the shell-exec security policy that four specs
describe at different stages. **This is a summary; the authoritative design is
[[daemon-command-child-policy-refined-spec]].** The syscall-hardening and lite
specs are superseded stages of the same policy, not separate policies.

| Doc | Role | Status |
|---|---|---|
| [[daemon-command-child-policy-refined-spec]] | Authoritative design | **Current** |
| [[daemon-command-syscall-hardening-spec]] | Original heavy design (vendored allowlist) | Superseded |
| [[daemon-command-syscall-hardening-test-case]] | Behavioral catalog (CS-01…CS-06 carry over) | Superseded |
| [[daemon-command-child-policy-lite-spec]] | Interim design | Superseded |

## 1. Principle

Treat the **child spawned by `shell_exec` as hostile** and shrink its kernel
interface to the minimum that still runs arbitrary developer workloads, using
in-kernel primitives (seccomp + capability drop + namespaces) applied **only**
to that child. The policy is bound to the `shell_exec` runner path, not to the
`exec_command` operation that commonly reaches it. This is the gVisor
*principle* (a small, mediated syscall interface) without its *architecture* (a
userspace kernel) — hence **surface reduction, not isolation**: a 0-day in an
*allowed* syscall still reaches the shared kernel.

The scope is Linux-in-a-container, which is the **Docker sandbox** — the only
real backend. seccomp/caps protect the shared Linux kernel (the host's on Linux;
Docker's Linux VM on macOS/Windows). They do **not** protect the native
macOS/Windows kernel — that is the hypervisor's boundary.

## 2. Tiered privilege model

| Tier | Processes | Policy |
|---|---|---|
| **Setup / admin** | daemon, ns-holder, ns-runner helper, overlay/layerstack/workspace/network setup | **Untouched** — keep every cap and syscall they need |
| **Shell exec** | the child process spawned by `runner/shell_exec.rs` | `no_new_privs` + targeted cap drop + seccomp deny table |
| **Outer container** | the Docker container | Still `--privileged` — de-privileging is Phase 3 (not yet done) |

Setup/admin paths run through different ns-runner entry points
(mount/remount/file) that **never** install the child policy, so they retain full
privilege by construction — not by opting out.

## 3. Shell-exec enforcement

Installed at the `shell_exec` child `pre_exec` in
`crates/sandbox-runtime/namespace-process/src/runner/shell_exec.rs` — the **sole**
enforcement site — in this fixed order:

```text
setpgid(0, 0)                    # process-group leader (existing)
prctl(PR_SET_NO_NEW_PRIVS, 1)    # no privilege gain across execve; unlocks seccomp
drop_capabilities()              # clear ambient → PR_CAPBSET_DROP non-kept → capset keep-set
install_seccomp_lite()           # deny table (default ALLOW), arch-guarded
execve(user command)             # permitted by the filter
```

Order is load-bearing: `no_new_privs` **before** seccomp (kernel requirement); cap
drop **before** seccomp (needs `CAP_SETPCAP`, still held); `execve` never denied.
The BPF is built **before `fork`** and cached in a `OnceLock`; the child's
`pre_exec` closure only issues `prctl`/`capset`/`seccomp` over prebuilt immutable
data — async-signal-safe, no allocation.

## 4. Seccomp — deny table, not allowlist

Default action is **ALLOW**; a fixed named set is rejected. A denylist (vs the
deleted `DOCKER_DEFAULT_SYSCALLS` allowlist) is what makes the policy compatible
with **any** image — uncommon and future syscalls pass instead of breaking. Two
stacked filters: the deny table (→ `EPERM`) and `clone3` (→ `ENOSYS`, so libc
falls back to `clone(2)`, whose flag mask seccomp can inspect). Arch-guarded, with
an X32-ABI reject on x86_64.

| Family | Syscalls | Threat |
|---|---|---|
| Mount mutation | `mount`, `umount2`, `pivot_root`, `move_mount`, `open_tree`, `fsopen`, `fsconfig`, `fsmount`, `fspick`, `mount_setattr` | Undo the `/eos` mount mask; overlay tamper |
| Namespace mutation | `setns`, `unshare`, `clone(CLONE_NEW*)` | Fresh userns → full cap set (classic escape) |
| Opaque clone | `clone3` → `ENOSYS` | Bypass the `clone` flag-mask check |
| Kernel module / control | `init_module`, `finit_module`, `delete_module`, `kexec_load`, `kexec_file_load`, `reboot` | Arbitrary kernel code; reboot DoS |
| Kernel attack surface | `bpf`, `perf_event_open`, `userfaultfd`, `fanotify_init`, `io_uring_setup/enter/register` | Most-exploited LPE surfaces |
| Device nodes | `mknod`/`mknodat` with char/block mode | Raw disk / `/dev/mem` / host device |
| Handle / keyring | `open_by_handle_at`, `add_key`, `request_key`, `keyctl` | Shocker fs breakout; keyring UAF |
| Swap / quota | `swapon`, `swapoff`, `quotactl` | Resource DoS; `quotactl` CVEs |

**Why seccomp is load-bearing, not redundant with caps:** the highest-value
denials are **not capability-gated** — `unshare(NEWUSER)` needs no capability,
and `io_uring`/`userfaultfd`/`perf_event_open`/`open_by_handle_at`/keyrings are
reachable regardless of caps. seccomp is the primary barrier for these.

## 5. Capability policy (targeted drop)

Per-process drop, shell-exec child only. Most kept caps are already neutered against
host resources by the non-initial user namespace; the drop is defense-in-depth
under seccomp.

- **Keep** (sandbox-root must work, so `apt`/`apk`/`dnf` do): `CHOWN`,
  `DAC_OVERRIDE`, `DAC_READ_SEARCH`, `FOWNER`, `FSETID`, `SETUID`, `SETGID`,
  `SETFCAP`, `KILL`, `NET_BIND_SERVICE`, `NET_RAW`, `MKNOD`.
- **Drop** (system-power / kernel surface): `SYS_ADMIN` (**the** highest-value
  removal — breaks the userns+`SYS_ADMIN` escape), `NET_ADMIN`, `SYS_MODULE`,
  `SYS_BOOT`, `SYS_RAWIO`, `BPF`, `PERFMON`, `SYSLOG`, `SYS_PTRACE`, and every
  other non-kept cap.

Where a cap is deliberately **kept**, seccomp is the compensating backstop:
`MKNOD` kept ↔ char/block `mknod` denied; `DAC_READ_SEARCH` kept ↔
`open_by_handle_at` denied. `ptrace(2)` stays **allowed** (confined by the PID
namespace) even though `SYS_PTRACE` is dropped, so `gdb`/`strace`/sanitizers work.

## 6. Mode: unconditional enforce

There is **no operator-facing mode knob and no mode selection**. Every user
command that reaches `shell_exec` is hardened with the full policy above. The
former `manager.shell_security.mode` config key, the `relaxed` mode (which
dropped the namespace denials), and the selectable `off` mode were all removed
— they were speculative, security-reducing, or silently-misconfigurable surface.
The policy is now a **compiled-in constant**: the `shell_exec` child `pre_exec`
applies enforce unconditionally — there is no policy value to select, thread
through the config, or carry on the request path.

## 7. Compatibility model

- **Any image:** ships in the daemon binary (no in-image dependency); denylist +
  mode-filtered `mknod` + kept FS caps keep package managers working generically;
  `ptrace` kept for debuggers/sanitizers.
- **Unsupported** (would need `--no-sandbox` or a future gated mode; not declared
  workloads): anything that creates namespaces — nested containers, rootless
  Docker/Podman, Bazel sandboxing, bubblewrap/flatpak, Chromium/Puppeteer
  sandboxes. The one *rising* compat risk in the deny table is `io_uring`
  (runtimes fall back to epoll/threadpool today).

## 8. Posture — what "~80%" means

A **qualitative** label, not a metric: block the syscall/capability families that
appear in the large majority of *published* container escapes and kernel-LPE
chains (namespace/mount abuse, module loading, `bpf`/`io_uring`/`userfaultfd`/
`perf`, keyring UAF, `open_by_handle_at`, device-node creation). What it does
**not** get — and only a separate kernel per sandbox could — is memory-corruption
0-days through the *allowed* core surface (`read`/`write`/`ioctl`/`futex`/`mmap`/
net), side-channels, and shared-kernel machinery bugs. **Do not represent it as a
guarantee of sandbox-to-sandbox kernel isolation.**

## 9. Residual risks (owned, by design)

- **Not isolation:** a 0-day in an allowed core syscall reaches the shared
  host/VM kernel and can cross to co-tenant sandboxes.
- **`TIOCSTI`** terminal-injection `ioctl` is not filtered (too fragile to
  arg-filter; sysctl-mitigated on modern kernels).
- **Denylist can't auto-close future/obscure syscalls.** The intended backstop is
  **Phase 3** — restore Docker's own default seccomp at the container boundary and
  de-privilege the container — which is **not yet implemented**. Until then the
  deny table is the whole barrier and the container is still `--privileged`, so
  the mount/device-node denials are doing acute work; do not defer Phase 3
  indefinitely.

## 10. Enforcement invariants

- **Sole site:** only the `shell_exec` child `pre_exec` installs the
  policy. The ns-runner helper's `pre_exec` (`install_pgid_leader_hook`) only
  `setpgid`s and re-execs the runner — never user argv.
- **Setup untouched:** setup/admin runner modes never enter `shell_exec`, so
  they skip the policy by construction (daemon, ns-holder, ns-runner,
  mount/remount/file runners, PTY path).
- **No downgrade surface:** the ns-runner applies enforce as a compiled-in
  constant (`apply_shell_security_policy()` takes no argument), so there is no
  policy value on the config or request path to drop, default, or tamper — hence
  no weaker posture to fall back to.
- **No request-path telemetry:** no `/dev/kmsg` scanning or audit reads on the
  command path (Phase 2 explicitly removed).

## References

- Authoritative: [[daemon-command-child-policy-refined-spec]]
- Superseded stages: [[daemon-command-syscall-hardening-spec]],
  [[daemon-command-syscall-hardening-test-case]],
  [[daemon-command-child-policy-lite-spec]]
- Adversarial reviews:
  [[daemon-command-child-policy-refined-adversarial-review-prompt]],
  [[daemon-shell-security-knob-removal-adversarial-review-prompt]]
- Live e2e: `cli-operation-e2e-live-test/runtime/shell_security/`
- Enforcement: `crates/sandbox-runtime/namespace-process/src/runner/shell_exec.rs`
  (install site) and `.../runner/shell_security.rs` (deny-table + cap builder)
