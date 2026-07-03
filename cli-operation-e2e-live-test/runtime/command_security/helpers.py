import os
import platform
import subprocess
import textwrap
import time

from core.cli import runtime

CAP_CHOWN = 0
CAP_DAC_OVERRIDE = 1
CAP_FOWNER = 3
CAP_NET_ADMIN = 12
CAP_SYS_MODULE = 16
CAP_SYS_ADMIN = 21
CAP_SETFCAP = 31

DENIED_SYSCALLS = (
    "mount",
    "unshare_newns",
    "unshare_zero",
    "mknod_char",
    "keyctl",
    "add_key",
    "bpf",
    "io_uring",
)

ALLOWED_SYSCALLS = (
    "mknod_fifo",
    "ptrace",
    "renameat",
    "renameat2",
    "fchmodat2",
    "dac_override",
)

PROBE_SOURCE = r"""
use std::ffi::CString;
use std::fs;
use std::os::raw::{c_int, c_long, c_uint, c_ulong, c_void};
use std::os::unix::fs::PermissionsExt;
use std::ptr;

extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
    fn __errno_location() -> *mut c_int;
}

const EPERM: c_int = 1;
const ENOSYS: c_int = 38;
const AT_FDCWD: c_int = -100;
const CLONE_NEWNS: c_int = 0x0002_0000;
const S_IFCHR: c_uint = 0o020000;
const S_IFIFO: c_uint = 0o010000;
const PTRACE_TRACEME: c_int = 0;

#[cfg(target_arch = "x86_64")]
mod nr {
    pub const MOUNT: i64 = 165;
    pub const UNSHARE: i64 = 272;
    pub const MKNODAT: i64 = 259;
    pub const PTRACE: i64 = 101;
    pub const RENAMEAT: i64 = 264;
    pub const RENAMEAT2: i64 = 316;
    pub const FCHMODAT2: i64 = 452;
    pub const KEYCTL: i64 = 250;
    pub const ADD_KEY: i64 = 248;
    pub const BPF: i64 = 321;
    pub const IO_URING_SETUP: i64 = 425;
    pub const CLONE3: i64 = 435;
}

#[cfg(target_arch = "aarch64")]
mod nr {
    pub const MOUNT: i64 = 40;
    pub const UNSHARE: i64 = 97;
    pub const MKNODAT: i64 = 33;
    pub const PTRACE: i64 = 117;
    pub const RENAMEAT: i64 = 38;
    pub const RENAMEAT2: i64 = 276;
    pub const FCHMODAT2: i64 = 452;
    pub const KEYCTL: i64 = 219;
    pub const ADD_KEY: i64 = 217;
    pub const BPF: i64 = 280;
    pub const IO_URING_SETUP: i64 = 425;
    pub const CLONE3: i64 = 435;
}

fn c(value: &str) -> CString {
    CString::new(value).unwrap()
}

fn errno() -> c_int {
    unsafe { *__errno_location() }
}

fn clear_errno() {
    unsafe { *__errno_location() = 0 };
}

fn classify(ret: c_long) -> String {
    if ret >= 0 {
        return "OK".to_string();
    }
    match errno() {
        EPERM => "EPERM".to_string(),
        ENOSYS => "ENOSYS".to_string(),
        value => format!("ERR{value}"),
    }
}

fn report(name: &str, ret: c_long) {
    println!("{name}={}", classify(ret));
}

fn syscall_result<F>(call: F) -> c_long
where
    F: FnOnce() -> c_long,
{
    clear_errno();
    call()
}

fn dac_override() -> &'static str {
    let path = "/tmp/eos-cs-dac";
    if fs::write(path, b"x").is_err() {
        return "ERRwrite";
    }
    if fs::set_permissions(path, fs::Permissions::from_mode(0o000)).is_err() {
        return "ERRchmod";
    }
    match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(_) => "OK",
        Err(_) => "DENIED",
    }
}

fn status_field(name: &str) -> String {
    let prefix = format!("{name}:");
    fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find_map(|line| line.strip_prefix(&prefix).map(|value| value.trim().to_string()))
        .unwrap_or_default()
}

fn main() {
    let _ = fs::create_dir_all("/tmp/eos-cs-mount");

    let src = c("none");
    let target = c("/tmp/eos-cs-mount");
    let fstype = c("tmpfs");
    report("mount", syscall_result(|| unsafe {
        syscall(
            nr::MOUNT,
            src.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            0 as c_ulong,
            ptr::null::<c_void>(),
        )
    }));

    report("unshare_newns", syscall_result(|| unsafe {
        syscall(nr::UNSHARE, CLONE_NEWNS)
    }));

    report("unshare_zero", syscall_result(|| unsafe {
        syscall(nr::UNSHARE, 0)
    }));

    let char_path = c("/tmp/eos-cs-char");
    report("mknod_char", syscall_result(|| unsafe {
        syscall(
            nr::MKNODAT,
            AT_FDCWD,
            char_path.as_ptr(),
            S_IFCHR | 0o600,
            0x107 as c_ulong,
        )
    }));

    report("keyctl", syscall_result(|| unsafe {
        syscall(nr::KEYCTL, 0, 0, 0, 0, 0)
    }));

    let key_type = c("user");
    let key_name = c("eos-command-security");
    report("add_key", syscall_result(|| unsafe {
        syscall(
            nr::ADD_KEY,
            key_type.as_ptr(),
            key_name.as_ptr(),
            ptr::null::<c_void>(),
            0usize,
            -2i64,
        )
    }));

    report("bpf", syscall_result(|| unsafe {
        syscall(nr::BPF, 0, ptr::null::<c_void>(), 0usize)
    }));

    report("io_uring", syscall_result(|| unsafe {
        syscall(nr::IO_URING_SETUP, 1u32, ptr::null::<c_void>())
    }));

    report("clone3", syscall_result(|| unsafe {
        syscall(nr::CLONE3, ptr::null::<c_void>(), 0usize)
    }));

    let fifo_path = c("/tmp/eos-cs-fifo");
    report("mknod_fifo", syscall_result(|| unsafe {
        syscall(
            nr::MKNODAT,
            AT_FDCWD,
            fifo_path.as_ptr(),
            S_IFIFO | 0o600,
            0 as c_ulong,
        )
    }));

    report("ptrace", syscall_result(|| unsafe {
        syscall(
            nr::PTRACE,
            PTRACE_TRACEME,
            0,
            ptr::null_mut::<c_void>(),
            ptr::null_mut::<c_void>(),
        )
    }));

    let _ = fs::create_dir_all("/tmp/eos-cs-rename");
    let _ = fs::write("/tmp/eos-cs-rename/a", b"x");
    let rename_a = c("/tmp/eos-cs-rename/a");
    let rename_b = c("/tmp/eos-cs-rename/b");
    report("renameat", syscall_result(|| unsafe {
        syscall(
            nr::RENAMEAT,
            AT_FDCWD,
            rename_a.as_ptr(),
            AT_FDCWD,
            rename_b.as_ptr(),
        )
    }));

    let _ = fs::write("/tmp/eos-cs-rename/c", b"x");
    let rename_c = c("/tmp/eos-cs-rename/c");
    let rename_d = c("/tmp/eos-cs-rename/d");
    report("renameat2", syscall_result(|| unsafe {
        syscall(
            nr::RENAMEAT2,
            AT_FDCWD,
            rename_c.as_ptr(),
            AT_FDCWD,
            rename_d.as_ptr(),
            0u32,
        )
    }));

    let _ = fs::create_dir_all("/tmp/eos-cs-chmod");
    let chmod_path = c("/tmp/eos-cs-chmod");
    report("fchmodat2", syscall_result(|| unsafe {
        syscall(
            nr::FCHMODAT2,
            AT_FDCWD,
            chmod_path.as_ptr(),
            0o700u32,
            0u32,
        )
    }));

    println!("dac_override={}", dac_override());
    println!("nnp={}", status_field("NoNewPrivs"));
    println!("seccomp={}", status_field("Seccomp"));
    println!("capeff={}", status_field("CapEff"));
    println!("capbnd={}", status_field("CapBnd"));
}
"""


def linux_musl_target():
    target = os.environ.get("E2E_COMMAND_SECURITY_TARGET")
    if target:
        return target

    machine = platform.machine().lower()
    if machine in ("arm64", "aarch64"):
        return "aarch64-unknown-linux-musl"
    if machine in ("x86_64", "amd64"):
        return "x86_64-unknown-linux-musl"
    raise AssertionError(f"unsupported test host architecture: {machine}")


def compile_probe(workspace):
    source = workspace / "eos_command_security_probe.rs"
    binary = workspace / "eos_command_security_probe"
    source.write_text(textwrap.dedent(PROBE_SOURCE).strip() + "\n")
    subprocess.run(
        [
            "rustc",
            "--edition",
            "2021",
            "--target",
            linux_musl_target(),
            "-C",
            "linker=rust-lld",
            "-O",
            str(source),
            "-o",
            str(binary),
        ],
        check=True,
    )
    return binary


def exec_cmd(sandbox_id, command, *, yield_ms=4_000, timeout_ms=None, timeout=90):
    args = []
    if timeout_ms is not None:
        args += ["--timeout-ms", str(timeout_ms)]
    args += ["--yield-time-ms", str(yield_ms), command]
    return runtime(sandbox_id, "exec_command", *args, timeout=timeout)


def read_command_lines(
    sandbox_id,
    command_session_id,
    *,
    start_offset=0,
    limit=1000,
    timeout=60,
):
    return runtime(
        sandbox_id,
        "read_command_lines",
        "--command-session-id",
        command_session_id,
        "--start-offset",
        str(start_offset),
        "--limit",
        str(limit),
        timeout=timeout,
    )


def wait_command(sandbox_id, command_session_id, *, timeout_s=180):
    deadline = time.monotonic() + timeout_s
    last = None
    while time.monotonic() < deadline:
        last = read_command_lines(sandbox_id, command_session_id)
        if last.get("status") != "running":
            return last
        time.sleep(1.0)
    return last or {"status": "running", "command_session_id": command_session_id}


def run_probe(sandbox_id):
    result = exec_cmd(sandbox_id, "./eos_command_security_probe")
    assert result.get("status") == "ok", result
    return parse_probe_output(result.get("output", ""))


def parse_probe_output(output):
    report = {}
    for line in output.splitlines():
        key, separator, value = line.partition("=")
        if separator:
            report[key.strip()] = value.strip()
    return report


def has_cap(cap_hex, bit):
    return ((int(cap_hex, 16) >> bit) & 1) == 1
