# Shell-Exec Security — Live E2E Test-Case Catalog

Tiered catalog of live e2e cases for the shell-exec security policy (seccomp deny
table + targeted capability drop + `no_new_privs`, applied unconditionally in
`enforce` at the `shell_exec` child `pre_exec`). Authority:
[[security-policy]] and [[daemon-command-child-policy-refined-spec]]. Supersedes
and expands the former CS-01…CS-06 cases, which are now folded into the matching
SS cases across `test_shell_security_{easy,medium,hard}.py`.

**40 cases: 10 easy · 15 medium · 15 hard.**

**Required image: `ubuntu:24.04`.** Pinned via the suite default (`core.config.IMAGE`)
or the `E2E_IMAGE` env var. Every `apt`/`util-linux` step below assumes ubuntu24
(noble archive pockets, `setpriv`/`setcap` availability, etc.).

## Approach

Same mechanism as the current suite: a single-file **static musl probe**
(`helpers.py::PROBE_SOURCE`, compiled to `/workspace/eos_shell_security_probe`)
issues each security-relevant syscall directly and prints
`key=OK|EPERM|ENOSYS|ERR<errno>`, then dumps
`NoNewPrivs`/`Seccomp`/`CapEff`/`CapBnd` from `/proc/self/status`. In-process
syscalls make the suite deterministic and image-independent; a few cases cross-
check with the image's own tools (`util-linux`, `apt`) for realism. Every case
runs the child through `exec_command`, so its `/proc/self/status` reflects exactly
the policy applied to user commands.

**Coverage legend** — all 40 cases are now landed and green, so every row is **✓**.
- **✓** — implemented as a runnable pytest against the extended `helpers.py` probe
  / harness (probe syscalls, X32 caller, `privinfo` mode, image tooling, or
  orchestration). The historical **†** marker (needs an extension) is retired; the
  "Probe / harness coverage" section below records what the extension added.

## Tiers

| Tier | Marker | Meaning |
|---|---|---|
| **Easy** | `easy` | One deterministic assertion, no setup — single syscall or status field. |
| **Medium** | `medium` | Full family/set, capability decode, real image tooling, or multi-step. |
| **Hard** | `hard` | Adversarial / exploit-shaped, arch-specific, or orchestration-heavy. |

---

## Easy (10)

| ID | Cov | Action | Expected | Guards |
|---|---|---|---|---|
| SS-E01 | ✓ | `id -u`, `uname -m`, `echo ok` via `exec_command` | all `status == ok`, output present | policy doesn't break normal commands; daemon path intact |
| SS-E02 | ✓ | write then read `/workspace/e02.txt` | round-trips; both `ok` | overlay/workspace write path unaffected |
| SS-E03 | ✓ | probe → `nnp` | `NoNewPrivs == 1` | NNP installed before seccomp |
| SS-E04 | ✓ | probe → `seccomp` | `Seccomp == 2` (SECCOMP_MODE_FILTER) | filter mode active |
| SS-E05 | ✓ | probe `mount(none,/tmp/…,tmpfs)` | `mount == EPERM` | mount-mutation denial (T1) |
| SS-E06 | ✓ | probe `unshare(CLONE_NEWNS)` and `unshare(0)` | both `EPERM` | namespace-mutation denial (T2) — not cap-gated |
| SS-E07 | ✓ | probe `mknodat` char device | `mknod_char == EPERM` | device-node denial (T5); `MKNOD` cap kept, seccomp is the barrier |
| SS-E08 | ✓ | probe `mknodat` FIFO | `mknod_fifo == OK` | mode-filter allows non-device nodes (pkg postinst) |
| SS-E09 | ✓ | probe `bpf(0,…)` and `io_uring_setup` | both `EPERM` | kernel-surface denial (T4) — not cap-gated |
| SS-E10 | ✓ | probe `clone3(NULL,0)` | `clone3 == ENOSYS` (not `EPERM`) | forces glibc/musl `clone(2)` fallback the flag-mask inspects |

## Medium (15)

| ID | Cov | Action | Expected | Guards |
|---|---|---|---|---|
| SS-M01 | ✓ | probe → every `DENIED_SYSCALLS` key | all `EPERM` | full current deny set in one run |
| SS-M02 | ✓ | probe → every `ALLOWED_SYSCALLS` key | all `OK` (`fchmodat2` may be `ENOSYS`) | usability refinements not swept up |
| SS-M03 | ✓ | `has_cap(capeff, …)` for SYS_ADMIN/NET_ADMIN/SYS_MODULE | all **absent** | system-power caps dropped |
| SS-M04 | ✓ | `has_cap(capeff, …)` for CHOWN/DAC_OVERRIDE/FOWNER/SETFCAP | all **present** | FS/identity caps kept so pkg managers work |
| SS-M05 | ✓ | `has_cap(capbnd, SYS_ADMIN)` | **absent** | dropped from the bounding set → not re-gainable via `execve` |
| SS-M06 | ✓ | image `unshare -m true` (skip if absent) | `status != ok` | filter holds against real `util-linux` |
| SS-M07 | ✓ | image `mount -t tmpfs …`, `umount /workspace` | both `status != ok` | mount tools rejected |
| SS-M08 | ✓ | `apt-get --version` | `ok` | pkg manager runs under reduced caps/seccomp |
| SS-M09 | ✓ | `apt-get update` + `install --no-install-recommends hello` | `ok`, or **skip** if no egress | real install under policy; poll the session, don't hold the request |
| SS-M10 | ✓ | probe `dac_override` (open a `chmod 000` file) | `OK` | `DAC_OVERRIDE` kept |
| SS-M11 | ✓ | probe `mknod` char/block **and** FIFO/regular in one run | char/block `EPERM`; FIFO/regular `OK` | mode-filter both directions (add block + regular to probe) |
| SS-M12 | ✓ | probe `renameat`/`renameat2`/`fchmodat2` | `OK` (`fchmodat2` `OK`/`ENOSYS`) | not caught by the deny table |
| SS-M13 | ✓ | probe `ptrace(TRACEME)`, then `fork` + `ptrace(ATTACH)` own child | both `OK` | `ptrace` kept, confined to the PID namespace |
| SS-M14 | ✓ | two independent `exec_command` probe runs | each reports `nnp=1`, `seccomp=2` | every child independently hardened; no leak/persistence |
| SS-M15 | ✓ | overlay write + read-back while the same sandbox runs the probe | writes succeed; probe still fully constrained | policy scoped to the shell-exec child, not setup |

## Hard (15)

| ID | Cov | Action | Expected | Guards |
|---|---|---|---|---|
| SS-H01 | ✓ | probe `unshare(CLONE_NEWUSER)` (the userns-escape primitive) | `EPERM` | blocks the userns→full-cap-set pivot; **not** cap-gated |
| SS-H02 | ✓ | probe raw `clone(CLONE_NEWUSER\|…)` vs `clone(SIGCHLD)` | `NEW*` → `EPERM`; plain fork → `OK` | flag-mask rule denies exactly the `CLONE_NEW*` bits |
| SS-H03 | ✓ | probe `clone3` with benign args | `ENOSYS` regardless of args | seccomp can't deref the `clone3` args pointer → blanket `ENOSYS` |
| SS-H04 | ✓ | x86_64: issue a syscall with the X32 bit (`nr\|0x40000000`) | **killed** (`SECCOMP_RET_KILL_PROCESS`); native call unaffected | X32-ABI reject; **skip on aarch64** |
| SS-H05 | ✓ | probe `open_by_handle_at` (Shocker) | `EPERM` | seccomp is the *only* barrier — `DAC_READ_SEARCH` is kept |
| SS-H06 | ✓ | probe new mount API: `fsopen`/`fsconfig`/`fsmount`/`fspick`/`move_mount`/`open_tree` | all `EPERM` | can't sidestep the classic `mount` denial via the new API |
| SS-H07 | ✓ | probe `umount2`/`pivot_root`/`mount_setattr` | all `EPERM` | full mount-mutation family, not just `mount` |
| SS-H08 | ✓ | probe `init_module` and `finit_module(fd)` | both `EPERM` | kernel-module load (T3) — `SYS_MODULE` dropped + seccomp |
| SS-H09 | ✓ | probe `io_uring_setup`/`io_uring_enter`/`io_uring_register` | all `EPERM` | close the whole ring — io_uring is a proxy-syscall bypass surface |
| SS-H10 | ✓ | probe `userfaultfd`/`perf_event_open`/`fanotify_init` | all `EPERM` | non-cap-gated LPE surfaces — seccomp load-bearing |
| SS-H11 | ✓ | stage a setuid-root helper (`chmod u+s`), then exec it | NNP=1 inherited across the setuid `execve`; `euid` unchanged; `CapEff` gains nothing (the sandbox userns maps only uid 0, so this is the observable form of "no privilege raised") | `no_new_privs` neutralizes setuid |
| SS-H12 | ✓ | `setcap cap_sys_admin+ep` on a binary (SETFCAP kept), then exec it | `setcap` succeeds; the `execve` is **refused with EPERM** (the `+e` cap names SYS_ADMIN, dropped from the bounding set) — or, where it does exec, child `CapEff` still **lacks** SYS_ADMIN | NNP + bounding-set drop defeat file caps |
| SS-H13 | ✓ | hold a namespace fd, probe `setns` into it | `EPERM` | `setns` denied distinctly from `unshare` |
| SS-H14 | ✓ | probe `swapon`/`swapoff`/`quotactl`/`reboot` | all `EPERM` | resource/DoS family (T7) — belt-and-suspenders (cap + seccomp) |
| SS-H15 | ✓ | a command spawns a same-pgid subtree looping the denied set | every attempt denied; subtree terminates cleanly; daemon/ns-runner/overlay stay privileged | scope-wait + policy scoped to the shell-exec child only |

---

## Probe / harness coverage

The single-file, no-crate `PROBE_SOURCE` (per-arch `nr` tables for x86_64 **and**
aarch64) now emits every key the catalog needs:

- **denied (EPERM):** `mount`, `umount2`, `pivot_root`, `move_mount`, `open_tree`,
  `fsopen`, `fsconfig`, `fsmount`, `fspick`, `mount_setattr`, `unshare_newns`,
  `unshare_zero`, `unshare_newuser`, `setns`, `clone_newuser`, `mknod_char`,
  `mknod_block`, `keyctl`, `add_key`, `open_by_handle_at`, `bpf`, `io_uring`,
  `io_uring_enter`, `io_uring_register`, `perf_event_open`, `userfaultfd`,
  `fanotify_init`, `init_module`, `finit_module`, `reboot`, `swapon`, `swapoff`,
  `quotactl` — all enumerated in `DENIED_SYSCALLS`, so SS-M01 sweeps them.
- **allowed (OK):** `mknod_fifo`, `mknod_regular`, `clone_sigchld`, `ptrace`,
  `ptrace_attach`, `renameat`, `renameat2`, `fchmodat2` (OK/ENOSYS),
  `dac_override` — enumerated in `ALLOWED_SYSCALLS`, so SS-M02 sweeps them.
- **special:** `clone3`/`clone3_args` → blanket `ENOSYS`; plus `nnp`, `seccomp`,
  `capeff`, `capbnd` from `/proc/self/status`.

The forking probes (`ptrace_attach`, `clone_sigchld`/`clone_newuser`, `clone3_args`)
run first, before `PTRACE_TRACEME`, and reap their children with async-signal-safe
`_exit`, so no child signal can stop the later-traced parent.

Two extra argv modes keep the harness single-file:
- `./eos_shell_security_probe x32` (SS-H04) issues a syscall with the X32 bit set;
  the arch guard must kill it with `SECCOMP_RET_KILL_PROCESS`. `x86_64`-only —
  `pytest.mark.skipif` on aarch64.
- `./eos_shell_security_probe privinfo` (SS-H11/H12) dumps `ruid`/`euid`/`nnp`/
  `capeff` so a setuid-root helper (`chmod u+s`) and a `setcap cap_sys_admin+ep`
  helper can prove no privilege is gained across `execve`.

Image tooling (SS-H12 `setcap`, installed from `libcap2-bin`) and orchestration
(SS-H15's same-pgid subtree; the SS-M15/SS-H15 checks against the privileged
overlay write path via `file_write`) round out the harness.

## Run matrix (required for sign-off)

- **aarch64** and **x86_64** native Docker: full suite green. This is the only
  real test of the seccomp arch guard and per-arch syscall numbers; SS-H04
  additionally exercises the X32 reject (x86_64 only). No VM/QEMU/emulated path
  counts as x86_64 evidence. macOS/Windows sign-off means "Docker's Linux-VM path
  is green there," not host-kernel filtering.

## Traceability (case → policy)

| Policy element ([[security-policy]]) | Cases |
|---|---|
| `NoNewPrivs` + `Seccomp=2` installed | SS-E03, SS-E04, SS-M14 |
| Mount-mutation denial (incl. new mount API) | SS-E05, SS-M06, SS-M07, SS-H06, SS-H07 |
| Namespace-mutation denial (`unshare`/`setns`/`clone(NEW*)`) | SS-E06, SS-H01, SS-H02, SS-H03, SS-H13 |
| Device-node mode-filter | SS-E07, SS-E08, SS-M11 |
| Kernel-surface denial (`bpf`/`io_uring`/`uffd`/`perf`) | SS-E09, SS-H09, SS-H10 |
| Handle/keyring denial | SS-E09*, SS-H05 (keyring in SS-M01) |
| Module/kexec/reboot + swap/quota | SS-H08, SS-H14 |
| Targeted capability drop / keep | SS-M03, SS-M04, SS-M05, SS-H12 |
| NNP neutralizes setuid/file-caps | SS-H11, SS-H12 |
| Usability preserved (`ptrace`/rename/`fchmodat2`/DAC) | SS-M02, SS-M10, SS-M12, SS-M13 |
| Package managers work on arbitrary images | SS-M08, SS-M09 |
| Policy scoped to the shell-exec child only | SS-M15, SS-H15 |
| Arch guard + X32 reject | SS-H04, Run matrix |

## Running

```sh
export PATH="$PWD/bin:$PATH"
cd e2e
# rebuild the in-container daemon so the policy is live, then run the suite
E2E_REBUILD_BINARY=1 python3 -m pytest runtime/shell_security -v
# a single tier
python3 -m pytest runtime/shell_security -m easy -v
```

Prereqs: Docker running; host `rustc` has the matching musl target
(`rustup target add {aarch64,x86_64}-unknown-linux-musl`). Cross-arch: set
`E2E_SHELL_SECURITY_TARGET` to the musl target matching the sandbox container.

## Notes / caveats

- The policy is **unconditional `enforce`** — there is no mode knob, so there is
  no mode matrix. Every case asserts enforce behavior.
- `fchmodat2` counts as allowed on `OK` **or** `ENOSYS` (older/emulated kernels
  may not implement it); `EPERM` still fails the case.
- SS-M09's install depends on the workspace network profile having egress
  (`shared` does, `isolated` does not) — hence skip, not fail.
- All 40 cases are landed and green; the former **†** backlog was cleared by
  extending the probe additively (single-file, no crates). A case that cannot run
  in a given environment still `skip`s with a logged reason (SS-M09 without egress;
  SS-H12 when there is no package egress to install `libcap2-bin` for `setcap`;
  SS-H04 on aarch64) — it never silently passes.
