# Shell-Exec Security — Live E2E Test-Case Catalog

Tiered catalog of live e2e cases for the shell-exec security policy (seccomp deny
table + targeted capability drop + `no_new_privs`, applied unconditionally in
`enforce` at the `shell_exec` child `pre_exec`). Authority:
[[security-policy]] and [[daemon-command-child-policy-refined-spec]]. Supersedes
and expands the CS-01…CS-06 cases now in `test_shell_security.py`.

**40 cases: 10 easy · 15 medium · 15 hard.**

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

**Coverage legend** (does the *current* probe/harness already exercise it?):
- **✓** — covered by today's `helpers.py` probe / helpers.
- **†** — requires a probe or harness extension (new syscall in `PROBE_SOURCE`,
  a raw-`clone`/X32 caller, image tooling, or orchestration). Listed so the
  catalog defines intended coverage, not just what exists.

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
| SS-M11 | † | probe `mknod` char/block **and** FIFO/regular in one run | char/block `EPERM`; FIFO/regular `OK` | mode-filter both directions (add block + regular to probe) |
| SS-M12 | ✓ | probe `renameat`/`renameat2`/`fchmodat2` | `OK` (`fchmodat2` `OK`/`ENOSYS`) | not caught by the deny table |
| SS-M13 | † | probe `ptrace(TRACEME)`, then `fork` + `ptrace(ATTACH)` own child | both `OK` | `ptrace` kept, confined to the PID namespace |
| SS-M14 | ✓ | two independent `exec_command` probe runs | each reports `nnp=1`, `seccomp=2` | every child independently hardened; no leak/persistence |
| SS-M15 | ✓ | overlay write + read-back while the same sandbox runs the probe | writes succeed; probe still fully constrained | policy scoped to the shell-exec child, not setup |

## Hard (15)

| ID | Cov | Action | Expected | Guards |
|---|---|---|---|---|
| SS-H01 | † | probe `unshare(CLONE_NEWUSER)` (the userns-escape primitive) | `EPERM` | blocks the userns→full-cap-set pivot; **not** cap-gated |
| SS-H02 | † | probe raw `clone(CLONE_NEWUSER\|…)` vs `clone(SIGCHLD)` | `NEW*` → `EPERM`; plain fork → `OK` | flag-mask rule denies exactly the `CLONE_NEW*` bits |
| SS-H03 | ✓ | probe `clone3` with benign args | `ENOSYS` regardless of args | seccomp can't deref the `clone3` args pointer → blanket `ENOSYS` |
| SS-H04 | † | x86_64: issue a syscall with the X32 bit (`nr\|0x40000000`) | **killed** (`SECCOMP_RET_KILL_PROCESS`); native call unaffected | X32-ABI reject; **skip on aarch64** |
| SS-H05 | † | probe `open_by_handle_at` (Shocker) | `EPERM` | seccomp is the *only* barrier — `DAC_READ_SEARCH` is kept |
| SS-H06 | † | probe new mount API: `fsopen`/`fsconfig`/`fsmount`/`fspick`/`move_mount`/`open_tree` | all `EPERM` | can't sidestep the classic `mount` denial via the new API |
| SS-H07 | † | probe `umount2`/`pivot_root`/`mount_setattr` | all `EPERM` | full mount-mutation family, not just `mount` |
| SS-H08 | † | probe `init_module` and `finit_module(fd)` | both `EPERM` | kernel-module load (T3) — `SYS_MODULE` dropped + seccomp |
| SS-H09 | † | probe `io_uring_setup`/`io_uring_enter`/`io_uring_register` | all `EPERM` | close the whole ring — io_uring is a proxy-syscall bypass surface |
| SS-H10 | † | probe `userfaultfd`/`perf_event_open`/`fanotify_init` | all `EPERM` | non-cap-gated LPE surfaces — seccomp load-bearing |
| SS-H11 | † | exec a setuid-root helper from the image | does **not** raise privilege across `execve` (NNP=1) | `no_new_privs` neutralizes setuid |
| SS-H12 | † | `setcap cap_sys_admin+ep` on a binary (SETFCAP kept), then exec it | `setcap` succeeds; child `CapEff` still **lacks** SYS_ADMIN | NNP + bounding-set drop defeat file caps |
| SS-H13 | † | hold a namespace fd, probe `setns` into it | `EPERM` | `setns` denied distinctly from `unshare` |
| SS-H14 | † | probe `swapon`/`swapoff`/`quotactl`/`reboot` | all `EPERM` | resource/DoS family (T7) — belt-and-suspenders (cap + seccomp) |
| SS-H15 | † | a command spawns a same-pgid subtree looping the denied set | every attempt denied; subtree terminates cleanly; daemon/ns-runner/overlay stay privileged | scope-wait + policy scoped to the shell-exec child only |

---

## Probe / harness coverage

The current `PROBE_SOURCE` emits: `mount`, `unshare_newns`, `unshare_zero`,
`mknod_char`, `mknod_fifo`, `keyctl`, `add_key`, `bpf`, `io_uring`, `clone3`,
`ptrace`, `renameat`, `renameat2`, `fchmodat2`, `dac_override`, plus `nnp`,
`seccomp`, `capeff`, `capbnd`. All **✓** cases map to these directly.

**† cases need one of:**
- extra syscalls in `PROBE_SOURCE` — `unshare(NEWUSER)`, raw `clone` with a flag
  arg, `setns`, `open_by_handle_at`, `fsopen`/`fsconfig`/`fsmount`/`fspick`/
  `move_mount`/`open_tree`, `umount2`/`pivot_root`/`mount_setattr`,
  `init_module`/`finit_module`, `io_uring_enter`/`io_uring_register`,
  `userfaultfd`/`perf_event_open`/`fanotify_init`, `swapon`/`swapoff`/`quotactl`/
  `reboot`, a block-device + regular `mknod`, and a `ptrace(ATTACH)` of a child;
- an **X32 caller** (SS-H04) — a second tiny probe built for the `x32` path, or a
  raw `int 0x80`/`syscall` with the X32 bit; skipped on aarch64;
- **image tooling / caps** — a setuid-root binary (SS-H11) and `setcap` (SS-H12);
- **orchestration** — SS-H15's same-pgid subtree and the SS-M15/SS-H15 scoping
  checks against the privileged setup path.

Extend `PROBE_SOURCE` additively (keep the single-file, no-crate compile) and add
the new keys to `DENIED_SYSCALLS`/`ALLOWED_SYSCALLS` so the M01/M02 set loops pick
them up.

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
cd cli-operation-e2e-live-test
# rebuild the in-container daemon so the policy is live, then run the suite
E2E_REBUILD_BINARY=1 pytest runtime/shell_security -v
# a single tier
pytest runtime/shell_security -m easy -v
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
- † cases are the thoroughness backlog: they define intended coverage. Land them
  by extending the probe additively; until then they are `xfail`/`skip`, logged
  so partial coverage never reads as full.
