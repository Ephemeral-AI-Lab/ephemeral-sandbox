---
title: Daemon Command Syscall Hardening — Test Cases
tags:
  - ephemeral-os
  - sandbox
  - security
  - implementation-plan
  - test-case
status: draft
spec: daemon-command-syscall-hardening-spec.md
---

# Daemon Command Syscall Hardening — Test Cases

Runnable test catalog for [[daemon-command-syscall-hardening-spec]]. It defines
the live e2e suite that verifies the inward syscall boundary the daemon installs
at the user-command `pre_exec`: dangerous syscalls denied, root-in-sandbox
behaviors preserved, and the exact `NoNewPrivs`/`Seccomp`/capability state.

## What this validates (traceability)

| Spec requirement | Case(s) |
|---|---|
| Policy scoped to command child; daemon path unaffected | CS-01 |
| Seccomp denies mount/namespace/module/keyring/handle/io_uring | CS-02 |
| `mknod` mode-filter and `ptrace` kept (usability refinements) | CS-03 |
| `no_new_privs` + seccomp installed; **targeted** cap drop (correction #1) | CS-04 |
| Filter also holds against the image's real `util-linux` tools | CS-05 |
| Package managers keep working on arbitrary images | CS-06 |
| Arch guard correct on aarch64 **and** x86_64 (correction #2) | Run matrix |
| `relaxed`/`off` modes behave | Mode matrix (env-gated) |
| Filter-builder + cap-set correctness (no daemon needed) | Rust unit tests |

## Approach

Mirror `runtime/network_isolation`: compile a tiny **static musl probe** on the
host with `rustc` (single file, std + `extern "C"` libc symbols — no cargo, no
crates), bake it into the workspace so it lands in the sandbox at
`/workspace/eos_syscall_probe`, then run it via `exec_command`. The probe attempts
each security-relevant syscall directly and prints `key=OK|EPERM|ERR<errno>`, then
dumps `NoNewPrivs`/`Seccomp`/`CapEff`/`CapBnd` from `/proc/self/status`. Running
the syscalls in-process (rather than shelling to image tools) makes the suite
deterministic and independent of what binaries the base image ships. CS-05 adds a
cross-check using the image's own `util-linux` for realism.

Default image is `ubuntu:24.04` (`core/config.py`), overridable via `E2E_IMAGE`.

## Folder layout

```
cli-operation-e2e-live-test/runtime/command_security/
├── __init__.py                 # empty package marker
├── helpers.py                  # probe source, compile, run/parse, cap-bit decode
├── test_command_security.py    # CS-01 … CS-06
└── test_spec.md                # in-tree pointer back to this catalog
```

## Case table

| ID | Tier | Action | Expected | Guards |
|---|---|---|---|---|
| CS-01 | smoke | `echo OK`; write+read `/workspace/cs01.txt` | both `ok`; content round-trips | scope: policy doesn't break normal commands; overlay/daemon path intact |
| CS-02 | smoke | probe: `mount`, `unshare(NEWNS)`, `mknod` char dev, `keyctl`, `add_key`, `bpf`, `io_uring_setup` | every one `EPERM` | seccomp deny set (incl. `io_uring`) |
| CS-03 | smoke | probe: `mknod` FIFO, `ptrace(TRACEME)`, DAC-override open of a `chmod 000` file | every one `OK` | `mknod` mode-filter; `ptrace` kept; kept caps effective |
| CS-04 | smoke | probe: parse `/proc/self/status` | `nnp=1`, `seccomp=2`; `CapEff` lacks SYS_ADMIN/NET_ADMIN/SYS_MODULE, has CHOWN/DAC_OVERRIDE/FOWNER/SETFCAP; `CapBnd` lacks SYS_ADMIN | NNP+seccomp installed; targeted cap drop (correction #1) |
| CS-05 | medium | `unshare -m true`, `mount -t tmpfs …`, `umount /workspace` via image tools | status ≠ `ok` | filter holds against real `util-linux` |
| CS-06 | medium | `apt-get --version`; then `update` + `install hello` | version `ok`; install `ok` or **skip** if no egress | package managers operate under reduced caps/seccomp |

## Probe source (`helpers.py` → `PROBE_SOURCE`)

```rust
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_long, c_uint, c_ulong, c_void};

extern "C" {
    fn mount(src: *const c_char, tgt: *const c_char, fst: *const c_char,
             flags: c_ulong, data: *const c_void) -> c_int;
    fn unshare(flags: c_int) -> c_int;
    fn mknod(path: *const c_char, mode: c_uint, dev: c_ulong) -> c_int;
    fn ptrace(req: c_int, pid: c_int, addr: *mut c_void, data: *mut c_void) -> c_long;
    fn syscall(number: c_long, ...) -> c_long;
    fn __errno_location() -> *mut c_int;
}

const EPERM: c_int = 1;
const CLONE_NEWNS: c_int = 0x0002_0000;
const S_IFCHR: c_uint = 0o020000;
const S_IFIFO: c_uint = 0o010000;
const PTRACE_TRACEME: c_int = 0;

#[cfg(target_arch = "x86_64")]
mod nr { pub const KEYCTL: i64 = 250; pub const ADD_KEY: i64 = 248;
         pub const BPF: i64 = 321; pub const IO_URING_SETUP: i64 = 425; }
#[cfg(target_arch = "aarch64")]
mod nr { pub const KEYCTL: i64 = 219; pub const ADD_KEY: i64 = 217;
         pub const BPF: i64 = 280; pub const IO_URING_SETUP: i64 = 425; }

fn errno() -> c_int { unsafe { *__errno_location() } }
fn c(s: &str) -> CString { CString::new(s).unwrap() }

fn classify(ret: c_long) -> String {
    if ret >= 0 { return "OK".into(); }
    let e = errno();
    if e == EPERM { "EPERM".into() } else { format!("ERR{e}") }
}

fn dac_override() -> &'static str {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    let p = "/tmp/csp_dac";
    if fs::write(p, b"x").is_err() { return "ERRwrite"; }
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o000));
    match fs::OpenOptions::new().read(true).write(true).open(p) {
        Ok(_) => "OK",
        Err(_) => "DENIED",
    }
}

fn main() {
    let r = unsafe { mount(c("none").as_ptr(), c("/tmp").as_ptr(),
                           c("tmpfs").as_ptr(), 0, std::ptr::null()) };
    println!("mount={}", classify(r as c_long));
    println!("unshare={}", classify(unsafe { unshare(CLONE_NEWNS) } as c_long));
    let r = unsafe { mknod(c("/tmp/csp_char").as_ptr(), S_IFCHR | 0o644, 0x103) };
    println!("mknod_char={}", classify(r as c_long));
    println!("keyctl={}", classify(unsafe { syscall(nr::KEYCTL, 0, 0, 0, 0, 0) }));
    let r = unsafe { syscall(nr::ADD_KEY, c("user").as_ptr(), c("k").as_ptr(),
                             std::ptr::null::<c_void>(), 0usize, -2i64) };
    println!("add_key={}", classify(r));
    println!("bpf={}", classify(unsafe {
        syscall(nr::BPF, 0, std::ptr::null::<c_void>(), 0usize) }));
    let mut params = [0u8; 120];
    let r = unsafe { syscall(nr::IO_URING_SETUP, 1u32,
                             params.as_mut_ptr() as *mut c_void) };
    println!("io_uring={}", classify(r));

    let r = unsafe { mknod(c("/tmp/csp_fifo").as_ptr(), S_IFIFO | 0o644, 0) };
    println!("mknod_fifo={}", classify(r as c_long));
    let r = unsafe { ptrace(PTRACE_TRACEME, 0, std::ptr::null_mut(), std::ptr::null_mut()) };
    println!("ptrace={}", classify(r));
    println!("dac_override={}", dac_override());

    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for (key, field) in [("nnp","NoNewPrivs"),("seccomp","Seccomp"),
                         ("capeff","CapEff"),("capbnd","CapBnd")] {
        let v = status.lines()
            .find_map(|l| l.strip_prefix(&format!("{field}:")).map(|x| x.trim().to_string()))
            .unwrap_or_default();
        println!("{key}={v}");
    }
}
```

Syscall numbers are hard-coded per arch because the single-file probe has no
`libc` crate; `mount`/`unshare`/`mknod`/`ptrace` use libc wrappers, the rest go
through `syscall()`. `io_uring_setup` is 425 on both arches.

## `helpers.py`

```python
"""Command syscall-hardening probe: compile a musl static binary on the host,
bake it into the workspace, and run it inside the sandbox. Reports each syscall
as key=OK|EPERM|ERR<errno>, then dumps NoNewPrivs/Seccomp/CapEff/CapBnd."""

import platform
import subprocess
import textwrap

from core.cli import runtime

# Capability bit positions (linux/capability.h).
CAP_CHOWN = 0
CAP_DAC_OVERRIDE = 1
CAP_FOWNER = 3
CAP_NET_ADMIN = 12
CAP_SYS_MODULE = 16
CAP_SYS_ADMIN = 21
CAP_SETFCAP = 31

DENIED = ("mount", "unshare", "mknod_char", "keyctl", "add_key", "bpf", "io_uring")
ALLOWED = ("mknod_fifo", "ptrace", "dac_override")

PROBE_SOURCE = r"""
<the Rust probe above>
"""


def linux_musl_target():
    machine = platform.machine().lower()
    if machine in ("arm64", "aarch64"):
        return "aarch64-unknown-linux-musl"
    if machine in ("x86_64", "amd64"):
        return "x86_64-unknown-linux-musl"
    raise AssertionError(f"unsupported test host architecture: {machine}")


def compile_probe(workspace):
    source = workspace / "eos_syscall_probe.rs"
    binary = workspace / "eos_syscall_probe"
    source.write_text(textwrap.dedent(PROBE_SOURCE).strip() + "\n")
    subprocess.run(
        ["rustc", "--edition", "2021", "--target", linux_musl_target(),
         "-C", "linker=rust-lld", "-O", str(source), "-o", str(binary)],
        check=True,
    )
    return binary


def exec_cmd(sandbox_id, command, *, yield_ms=4000, timeout=60):
    return runtime(sandbox_id, "exec_command",
                   "--yield-time-ms", str(yield_ms), command, timeout=timeout)


def run_probe(sandbox_id, probe_path):
    result = exec_cmd(sandbox_id, probe_path)
    assert result.get("status") == "ok", result
    report = {}
    for line in result["output"].splitlines():
        if "=" in line:
            key, _, value = line.partition("=")
            report[key.strip()] = value.strip()
    return report


def has_cap(cap_hex, bit):
    return (int(cap_hex, 16) >> bit) & 1 == 1
```

## `test_command_security.py`

```python
"""Live coverage for daemon command syscall hardening (spec: enforce mode)."""

import pytest

from core.config import IMAGE
from manager.management import helpers as mgmt
from runtime.command_security.helpers import (
    ALLOWED, DENIED, CAP_CHOWN, CAP_DAC_OVERRIDE, CAP_FOWNER,
    CAP_NET_ADMIN, CAP_SYS_ADMIN, CAP_SYS_MODULE, CAP_SETFCAP,
    compile_probe, exec_cmd, has_cap, run_probe,
)


@pytest.fixture
def probe_sandbox(tmp_path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    probe = compile_probe(workspace)
    created = mgmt.create_sandbox(image=IMAGE, workspace_root=str(workspace))
    sandbox_id = created.get("id")
    assert sandbox_id, f"create_sandbox failed: {created}"
    try:
        yield sandbox_id, f"/workspace/{probe.name}"
    finally:
        mgmt.destroy_sandbox(sandbox_id)


@pytest.mark.smoke
def test_CS_01_normal_commands_and_overlay_unaffected(probe_sandbox):
    sandbox_id, _probe = probe_sandbox
    echo = exec_cmd(sandbox_id, "echo OK")
    assert echo["status"] == "ok" and "OK" in echo["output"], echo

    wrote = exec_cmd(sandbox_id, "echo persisted > /workspace/cs01.txt")
    assert wrote["status"] == "ok", wrote
    read = exec_cmd(sandbox_id, "cat /workspace/cs01.txt")
    assert read["status"] == "ok" and "persisted" in read["output"], read


@pytest.mark.smoke
def test_CS_02_dangerous_syscalls_denied(probe_sandbox):
    sandbox_id, probe = probe_sandbox
    report = run_probe(sandbox_id, probe)
    for key in DENIED:
        assert report.get(key) == "EPERM", (key, report)


@pytest.mark.smoke
def test_CS_03_root_in_sandbox_behaviors_preserved(probe_sandbox):
    sandbox_id, probe = probe_sandbox
    report = run_probe(sandbox_id, probe)
    for key in ALLOWED:
        assert report.get(key) == "OK", (key, report)


@pytest.mark.smoke
def test_CS_04_policy_state_installed(probe_sandbox):
    sandbox_id, probe = probe_sandbox
    report = run_probe(sandbox_id, probe)
    assert report.get("nnp") == "1", report            # no_new_privs
    assert report.get("seccomp") == "2", report        # SECCOMP_MODE_FILTER

    eff = report["capeff"]
    # System-power caps dropped from the command child...
    assert not has_cap(eff, CAP_SYS_ADMIN), report
    assert not has_cap(eff, CAP_NET_ADMIN), report
    assert not has_cap(eff, CAP_SYS_MODULE), report
    # ...filesystem/identity caps kept so package managers work.
    for bit in (CAP_CHOWN, CAP_DAC_OVERRIDE, CAP_FOWNER, CAP_SETFCAP):
        assert has_cap(eff, bit), (bit, report)
    # SYS_ADMIN also gone from the bounding set (not re-gainable via exec).
    assert not has_cap(report["capbnd"], CAP_SYS_ADMIN), report


@pytest.mark.medium
def test_CS_05_image_namespace_and_mount_tools_denied(probe_sandbox):
    sandbox_id, _probe = probe_sandbox
    for command in ("unshare -m true",
                    "mount -t tmpfs tmpfs /tmp/x",
                    "umount /workspace"):
        result = exec_cmd(sandbox_id, command)
        assert result.get("status") != "ok", (command, result)


@pytest.mark.medium
def test_CS_06_package_manager_operates(probe_sandbox):
    sandbox_id, _probe = probe_sandbox
    # apt must at least run under the reduced cap/seccomp set. A networked
    # install is skipped rather than flaked when the profile has no egress.
    version = exec_cmd(sandbox_id, "apt-get --version")
    assert version["status"] == "ok", version

    update = exec_cmd(sandbox_id, "apt-get update", timeout=120)
    if update.get("status") != "ok":
        pytest.skip(f"no package-network egress in this profile: {update}")
    install = exec_cmd(
        sandbox_id,
        "apt-get install -y --no-install-recommends hello && hello",
        timeout=180,
    )
    assert install["status"] == "ok", install
```

## Running

```sh
export PATH="$PWD/bin:$PATH"
cd cli-operation-e2e-live-test

# rebuild the in-container daemon so the new policy is live, then run the suite
E2E_REBUILD_BINARY=1 pytest runtime/command_security -v

# smoke subset only
pytest runtime/command_security -m smoke -v
```

Prereqs: Docker running, and the host `rustc` has the matching musl target
(`rustup target add {aarch64,x86_64}-unknown-linux-musl`) — same as
`network_isolation`.

## Run matrix (required for sign-off)

- **aarch64 host** (Apple Silicon / arm64 VM): full suite green.
- **x86_64 host**: full suite green.

Both are mandatory — this is the only real test of the seccomp arch guard
(correction #2). The probe compiles for the host arch and the daemon is built
per-arch, so a green run on each proves the arch preamble and per-arch syscall
numbers are correct; x86_64 additionally exercises the X32 reject path implicitly
(the probe only issues native-ABI calls, so a mis-built guard that kills native
x86_64 traffic would fail CS-01).

## Mode matrix (env-gated, optional)

`command_security.mode` is daemon-level config, so `relaxed`/`off` need a gateway
started with a different `config/*.yml`. Run these as a separate pass, not in the
default suite (which asserts `enforce`):

| Mode | Setup | Expectation |
|---|---|---|
| `relaxed` | gateway config `command_security.mode: relaxed`, rebuild | CS-02 `unshare`→`OK`; all non-namespace denials still `EPERM`; CS-04 seccomp still `2` |
| `off` | `command_security.mode: off`, rebuild | probe denied set → `OK` (no seccomp); CS-04 `seccomp=0`; cap drop still applied (`CapEff` lacks SYS_ADMIN) |

Gate with an env flag so CI defaults to `enforce`, e.g.
`E2E_COMMAND_SECURITY_MODE=relaxed pytest runtime/command_security/…`.

## Rust unit coverage (`namespace-process/tests/unit/command_security.rs`)

No daemon required — these guard the builder in isolation:

- Cap keep/drop sets partition all 40+ caps with no overlap; keep set ⊇
  {CHOWN, DAC_OVERRIDE, FOWNER, FSETID, SETUID, SETGID, SETFCAP, MKNOD}; drop set
  ⊇ {SYS_ADMIN, SYS_MODULE, NET_ADMIN, BPF}.
- Seccomp program builds for both target arches without panic; contains the arch
  guard as its first instruction; denied syscalls map to `EPERM` and `clone3` to
  `ENOSYS`; `execve`/`execveat` resolve to `ALLOW`.
- `clone` flag mask includes every `CLONE_NEW*` bit; a plain `SIGCHLD` clone
  (fork) is allowed; a `CLONE_NEWUSER` clone is denied.
- `enforce` vs `relaxed` vs `off` produce the expected instruction deltas
  (relaxed drops the namespace-syscall denials; off produces no filter).

## Notes / caveats

- Probe runs in a fresh command child each `exec_command`, so its `/proc/self/status`
  reflects exactly the policy applied to user commands.
- CS-06's install path depends on the workspace network profile having egress;
  the default (`shared`) does, `isolated` does not — hence the skip guard rather
  than a hard failure.
- If `rustc` lacks a `SYS_*`-equivalent for a new arch, extend the `nr` module in
  the probe rather than pulling a crate (keeps single-file compile intact).
