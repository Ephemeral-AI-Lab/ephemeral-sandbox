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

# Denied families the probe issues in-process. Every key here must classify as
# ``EPERM`` under the enforce policy, so SS-M01 loops the whole set in one run.
# ``clone3`` is intentionally excluded: it is a blanket ``ENOSYS`` (own case).
DENIED_SYSCALLS = (
    "mount",
    "umount2",
    "pivot_root",
    "move_mount",
    "open_tree",
    "fsopen",
    "fsconfig",
    "fsmount",
    "fspick",
    "mount_setattr",
    "unshare_newns",
    "unshare_zero",
    "unshare_newuser",
    "setns",
    "clone_newuser",
    "mknod_char",
    "mknod_block",
    "keyctl",
    "add_key",
    "open_by_handle_at",
    "bpf",
    "io_uring",
    "io_uring_enter",
    "io_uring_register",
    "perf_event_open",
    "userfaultfd",
    "fanotify_init",
    "init_module",
    "finit_module",
    "reboot",
    "swapon",
    "swapoff",
    "quotactl",
)

# Usability syscalls the probe issues in-process. Every key here must classify as
# ``OK`` (``fchmodat2`` may be ``ENOSYS`` on older kernels), so SS-M02 sweeps the
# whole set to prove the refinements were not caught by the deny table.
ALLOWED_SYSCALLS = (
    "mknod_fifo",
    "mknod_regular",
    "clone_sigchld",
    "ptrace",
    "ptrace_attach",
    "renameat",
    "renameat2",
    "fchmodat2",
    "dac_override",
)

PROBE_SOURCE = r"""
use std::env;
use std::ffi::CString;
use std::fs;
use std::os::raw::{c_int, c_long, c_uint, c_ulong, c_void};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::ptr;

extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
    fn __errno_location() -> *mut c_int;
    fn fork() -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn pause() -> c_int;
    fn _exit(status: c_int) -> !;
}

const EPERM: c_int = 1;
const ENOSYS: c_int = 38;
const AT_FDCWD: c_int = -100;
const CLONE_NEWNS: c_int = 0x0002_0000;
const CLONE_NEWUSER: c_ulong = 0x1000_0000;
const SIGCHLD: c_int = 17;
const SIGKILL: c_int = 9;
const S_IFCHR: c_uint = 0o020000;
const S_IFBLK: c_uint = 0o060000;
const S_IFIFO: c_uint = 0o010000;
const S_IFREG: c_uint = 0o100000;
const PTRACE_TRACEME: c_int = 0;
const PTRACE_ATTACH: c_int = 16;
const PTRACE_DETACH: c_int = 17;

#[cfg(target_arch = "x86_64")]
mod nr {
    pub const MOUNT: i64 = 165;
    pub const UMOUNT2: i64 = 166;
    pub const PIVOT_ROOT: i64 = 155;
    pub const MOVE_MOUNT: i64 = 429;
    pub const OPEN_TREE: i64 = 428;
    pub const FSOPEN: i64 = 430;
    pub const FSCONFIG: i64 = 431;
    pub const FSMOUNT: i64 = 432;
    pub const FSPICK: i64 = 433;
    pub const MOUNT_SETATTR: i64 = 442;
    pub const UNSHARE: i64 = 272;
    pub const SETNS: i64 = 308;
    pub const CLONE: i64 = 56;
    pub const CLONE3: i64 = 435;
    pub const MKNODAT: i64 = 259;
    pub const PTRACE: i64 = 101;
    pub const RENAMEAT: i64 = 264;
    pub const RENAMEAT2: i64 = 316;
    pub const FCHMODAT2: i64 = 452;
    pub const KEYCTL: i64 = 250;
    pub const ADD_KEY: i64 = 248;
    pub const BPF: i64 = 321;
    pub const PERF_EVENT_OPEN: i64 = 298;
    pub const USERFAULTFD: i64 = 323;
    pub const FANOTIFY_INIT: i64 = 300;
    pub const IO_URING_SETUP: i64 = 425;
    pub const IO_URING_ENTER: i64 = 426;
    pub const IO_URING_REGISTER: i64 = 427;
    pub const OPEN_BY_HANDLE_AT: i64 = 304;
    pub const INIT_MODULE: i64 = 175;
    pub const FINIT_MODULE: i64 = 313;
    pub const SWAPON: i64 = 167;
    pub const SWAPOFF: i64 = 168;
    pub const QUOTACTL: i64 = 179;
    pub const REBOOT: i64 = 169;
}

#[cfg(target_arch = "aarch64")]
mod nr {
    pub const MOUNT: i64 = 40;
    pub const UMOUNT2: i64 = 39;
    pub const PIVOT_ROOT: i64 = 41;
    pub const MOVE_MOUNT: i64 = 429;
    pub const OPEN_TREE: i64 = 428;
    pub const FSOPEN: i64 = 430;
    pub const FSCONFIG: i64 = 431;
    pub const FSMOUNT: i64 = 432;
    pub const FSPICK: i64 = 433;
    pub const MOUNT_SETATTR: i64 = 442;
    pub const UNSHARE: i64 = 97;
    pub const SETNS: i64 = 268;
    pub const CLONE: i64 = 220;
    pub const CLONE3: i64 = 435;
    pub const MKNODAT: i64 = 33;
    pub const PTRACE: i64 = 117;
    pub const RENAMEAT: i64 = 38;
    pub const RENAMEAT2: i64 = 276;
    pub const FCHMODAT2: i64 = 452;
    pub const KEYCTL: i64 = 219;
    pub const ADD_KEY: i64 = 217;
    pub const BPF: i64 = 280;
    pub const PERF_EVENT_OPEN: i64 = 241;
    pub const USERFAULTFD: i64 = 282;
    pub const FANOTIFY_INIT: i64 = 262;
    pub const IO_URING_SETUP: i64 = 425;
    pub const IO_URING_ENTER: i64 = 426;
    pub const IO_URING_REGISTER: i64 = 427;
    pub const OPEN_BY_HANDLE_AT: i64 = 265;
    pub const INIT_MODULE: i64 = 105;
    pub const FINIT_MODULE: i64 = 273;
    pub const SWAPON: i64 = 224;
    pub const SWAPOFF: i64 = 225;
    pub const QUOTACTL: i64 = 60;
    pub const REBOOT: i64 = 142;
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

// Raw `clone(2)` with only `SIGCHLD` — a plain fork the flag-mask rule must
// allow. The child is not permitted to touch the parent's heap or stdout, so it
// exits immediately with an async-signal-safe raw `_exit`.
fn probe_clone_sigchld() -> String {
    clear_errno();
    let ret = unsafe {
        syscall(nr::CLONE, SIGCHLD as c_ulong, 0usize, 0usize, 0usize, 0usize)
    };
    if ret == 0 {
        unsafe { _exit(0) };
    }
    let verdict = classify(ret);
    if ret > 0 {
        let mut status: c_int = 0;
        unsafe { waitpid(ret as c_int, &mut status, 0) };
    }
    verdict
}

// Raw `clone(CLONE_NEWUSER | SIGCHLD)` — the flag-mask rule must reject exactly
// the `CLONE_NEW*` bits with `EPERM` before any child is created.
fn probe_clone_newuser() -> String {
    clear_errno();
    let ret = unsafe {
        syscall(nr::CLONE, CLONE_NEWUSER | SIGCHLD as c_ulong, 0usize, 0usize, 0usize, 0usize)
    };
    if ret == 0 {
        unsafe { _exit(0) };
    }
    let verdict = classify(ret);
    if ret > 0 {
        let mut status: c_int = 0;
        unsafe { waitpid(ret as c_int, &mut status, 0) };
    }
    verdict
}

// `clone3` with a fully-populated benign args pointer: seccomp cannot deref the
// pointer, so it is a blanket `ENOSYS` regardless of the arguments.
fn probe_clone3_args() -> String {
    #[repr(C)]
    struct CloneArgs {
        flags: u64,
        pidfd: u64,
        child_tid: u64,
        parent_tid: u64,
        exit_signal: u64,
        stack: u64,
        stack_size: u64,
        tls: u64,
        set_tid: u64,
        set_tid_size: u64,
        cgroup: u64,
    }
    let args = CloneArgs {
        flags: 0,
        pidfd: 0,
        child_tid: 0,
        parent_tid: 0,
        exit_signal: SIGCHLD as u64,
        stack: 0,
        stack_size: 0,
        tls: 0,
        set_tid: 0,
        set_tid_size: 0,
        cgroup: 0,
    };
    clear_errno();
    let ret = unsafe {
        syscall(
            nr::CLONE3,
            &args as *const CloneArgs,
            std::mem::size_of::<CloneArgs>(),
        )
    };
    if ret == 0 {
        unsafe { _exit(0) };
    }
    let verdict = classify(ret);
    if ret > 0 {
        let mut status: c_int = 0;
        unsafe { waitpid(ret as c_int, &mut status, 0) };
    }
    verdict
}

// `ptrace(TRACEME)` then `fork` + `ptrace(ATTACH)` on the child: ptrace is kept,
// confined to the PID namespace, so attaching to one's own child succeeds. The
// child pauses until it is detached and killed; the parent reaps it so no signal
// reaches the tracer after `PTRACE_TRACEME` runs later in `main`.
fn probe_ptrace_attach() -> String {
    let pid = unsafe { fork() };
    if pid == 0 {
        loop {
            unsafe { pause() };
        }
    }
    if pid < 0 {
        return "ERRfork".to_string();
    }
    clear_errno();
    let ret = unsafe { syscall(nr::PTRACE, PTRACE_ATTACH, pid as c_long, 0, 0) };
    let verdict = classify(ret);
    let mut status: c_int = 0;
    if ret == 0 {
        unsafe { waitpid(pid, &mut status, 0) };
        unsafe { syscall(nr::PTRACE, PTRACE_DETACH, pid as c_long, 0, 0) };
    }
    unsafe { kill(pid, SIGKILL) };
    unsafe { waitpid(pid, &mut status, 0) };
    verdict
}

// SS-H04: issue a syscall with the X32 bit set. The filter's arch guard must
// terminate the process with `SECCOMP_RET_KILL_PROCESS` before the line below
// runs. Reaching the print means the reject is missing.
#[cfg(target_arch = "x86_64")]
fn run_x32_probe() {
    const X32_GETPID: c_long = 0x4000_0000 | 39;
    let ret = unsafe { syscall(X32_GETPID) };
    println!("x32_survived=ret{ret}");
}

#[cfg(not(target_arch = "x86_64"))]
fn run_x32_probe() {
    println!("x32_skipped=aarch64");
}

// SS-H11/H12: report the privilege state a setuid-root / file-capability helper
// actually inherits across `execve`. Under NoNewPrivs the setuid bit and file
// caps are neutralized, so `euid` stays the caller's and `capeff` never gains a
// dropped capability.
fn run_privinfo() {
    let uid = status_field("Uid");
    let mut uids = uid.split_whitespace();
    println!("ruid={}", uids.next().unwrap_or(""));
    println!("euid={}", uids.next().unwrap_or(""));
    println!("nnp={}", status_field("NoNewPrivs"));
    println!("capeff={}", status_field("CapEff"));
    println!("capbnd={}", status_field("CapBnd"));
}

fn run_suite() {
    let _ = fs::create_dir_all("/tmp/eos-cs-mount");

    // Forking probes run first, before `PTRACE_TRACEME`, so no child exit can
    // stop the (then-traced) parent.
    println!("clone_sigchld={}", probe_clone_sigchld());
    println!("clone_newuser={}", probe_clone_newuser());
    println!("clone3_args={}", probe_clone3_args());
    println!("ptrace_attach={}", probe_ptrace_attach());

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

    report("umount2", syscall_result(|| unsafe {
        syscall(nr::UMOUNT2, target.as_ptr(), 0)
    }));

    let pivot_new = c("/tmp/eos-cs-mount");
    let pivot_old = c("/tmp/eos-cs-mount");
    report("pivot_root", syscall_result(|| unsafe {
        syscall(nr::PIVOT_ROOT, pivot_new.as_ptr(), pivot_old.as_ptr())
    }));

    let move_from = c("/tmp/eos-cs-mount");
    let move_to = c("/tmp/eos-cs-mount");
    report("move_mount", syscall_result(|| unsafe {
        syscall(
            nr::MOVE_MOUNT,
            AT_FDCWD,
            move_from.as_ptr(),
            AT_FDCWD,
            move_to.as_ptr(),
            0 as c_uint,
        )
    }));

    let tree_path = c("/tmp/eos-cs-mount");
    report("open_tree", syscall_result(|| unsafe {
        syscall(nr::OPEN_TREE, AT_FDCWD, tree_path.as_ptr(), 0 as c_uint)
    }));

    let fs_name = c("tmpfs");
    report("fsopen", syscall_result(|| unsafe {
        syscall(nr::FSOPEN, fs_name.as_ptr(), 0 as c_uint)
    }));

    report("fsconfig", syscall_result(|| unsafe {
        syscall(
            nr::FSCONFIG,
            -1,
            0 as c_uint,
            ptr::null::<c_void>(),
            ptr::null::<c_void>(),
            0,
        )
    }));

    report("fsmount", syscall_result(|| unsafe {
        syscall(nr::FSMOUNT, -1, 0 as c_uint, 0 as c_uint)
    }));

    let pick_path = c("/tmp/eos-cs-mount");
    report("fspick", syscall_result(|| unsafe {
        syscall(nr::FSPICK, AT_FDCWD, pick_path.as_ptr(), 0 as c_uint)
    }));

    let setattr_path = c("/tmp/eos-cs-mount");
    report("mount_setattr", syscall_result(|| unsafe {
        syscall(
            nr::MOUNT_SETATTR,
            AT_FDCWD,
            setattr_path.as_ptr(),
            0 as c_uint,
            ptr::null::<c_void>(),
            0usize,
        )
    }));

    report("unshare_newns", syscall_result(|| unsafe {
        syscall(nr::UNSHARE, CLONE_NEWNS)
    }));

    report("unshare_zero", syscall_result(|| unsafe {
        syscall(nr::UNSHARE, 0)
    }));

    report("unshare_newuser", syscall_result(|| unsafe {
        syscall(nr::UNSHARE, CLONE_NEWUSER)
    }));

    let ns_file = fs::File::open("/proc/self/ns/uts").ok();
    let ns_fd = ns_file.as_ref().map(|file| file.as_raw_fd()).unwrap_or(-1);
    report("setns", syscall_result(|| unsafe {
        syscall(nr::SETNS, ns_fd, 0)
    }));
    drop(ns_file);

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

    let block_path = c("/tmp/eos-cs-block");
    report("mknod_block", syscall_result(|| unsafe {
        syscall(
            nr::MKNODAT,
            AT_FDCWD,
            block_path.as_ptr(),
            S_IFBLK | 0o600,
            0x0700 as c_ulong,
        )
    }));

    report("keyctl", syscall_result(|| unsafe {
        syscall(nr::KEYCTL, 0, 0, 0, 0, 0)
    }));

    let key_type = c("user");
    let key_name = c("eos-shell-security");
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

    report("open_by_handle_at", syscall_result(|| unsafe {
        syscall(
            nr::OPEN_BY_HANDLE_AT,
            -1,
            ptr::null::<c_void>(),
            0,
        )
    }));

    report("bpf", syscall_result(|| unsafe {
        syscall(nr::BPF, 0, ptr::null::<c_void>(), 0usize)
    }));

    report("io_uring", syscall_result(|| unsafe {
        syscall(nr::IO_URING_SETUP, 1u32, ptr::null::<c_void>())
    }));

    report("io_uring_enter", syscall_result(|| unsafe {
        syscall(
            nr::IO_URING_ENTER,
            -1,
            0u32,
            0u32,
            0u32,
            ptr::null::<c_void>(),
            0usize,
        )
    }));

    report("io_uring_register", syscall_result(|| unsafe {
        syscall(nr::IO_URING_REGISTER, -1, 0u32, ptr::null::<c_void>(), 0u32)
    }));

    report("perf_event_open", syscall_result(|| unsafe {
        syscall(
            nr::PERF_EVENT_OPEN,
            ptr::null::<c_void>(),
            0,
            -1,
            -1,
            0 as c_ulong,
        )
    }));

    report("userfaultfd", syscall_result(|| unsafe {
        syscall(nr::USERFAULTFD, 0)
    }));

    report("fanotify_init", syscall_result(|| unsafe {
        syscall(nr::FANOTIFY_INIT, 0u32, 0u32)
    }));

    let empty = c("");
    report("init_module", syscall_result(|| unsafe {
        syscall(nr::INIT_MODULE, ptr::null::<c_void>(), 0usize, empty.as_ptr())
    }));

    report("finit_module", syscall_result(|| unsafe {
        syscall(nr::FINIT_MODULE, -1, empty.as_ptr(), 0)
    }));

    // cmd=0 is not a real reboot command and magic1=0 is invalid, so even absent
    // seccomp this cannot restart the host — the deny is still the barrier.
    report("reboot", syscall_result(|| unsafe {
        syscall(nr::REBOOT, 0, 0, 0, ptr::null::<c_void>())
    }));

    let swap_path = c("/tmp/eos-cs-swap");
    report("swapon", syscall_result(|| unsafe {
        syscall(nr::SWAPON, swap_path.as_ptr(), 0)
    }));

    report("swapoff", syscall_result(|| unsafe {
        syscall(nr::SWAPOFF, swap_path.as_ptr())
    }));

    report("quotactl", syscall_result(|| unsafe {
        syscall(nr::QUOTACTL, 0, ptr::null::<c_void>(), 0, ptr::null::<c_void>())
    }));

    report("clone3", syscall_result(|| unsafe {
        syscall(nr::CLONE3, ptr::null::<c_void>(), 0usize)
    }));

    let fifo_path = c("/tmp/eos-cs-fifo");
    let _ = fs::remove_file("/tmp/eos-cs-fifo");
    report("mknod_fifo", syscall_result(|| unsafe {
        syscall(
            nr::MKNODAT,
            AT_FDCWD,
            fifo_path.as_ptr(),
            S_IFIFO | 0o600,
            0 as c_ulong,
        )
    }));

    let reg_path = c("/tmp/eos-cs-regnode");
    let _ = fs::remove_file("/tmp/eos-cs-regnode");
    report("mknod_regular", syscall_result(|| unsafe {
        syscall(
            nr::MKNODAT,
            AT_FDCWD,
            reg_path.as_ptr(),
            S_IFREG | 0o600,
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

fn main() {
    match env::args().nth(1).as_deref() {
        Some("x32") => run_x32_probe(),
        Some("privinfo") => run_privinfo(),
        _ => run_suite(),
    }
}
"""


def linux_musl_target():
    target = os.environ.get("E2E_SHELL_SECURITY_TARGET")
    if target:
        return target

    machine = platform.machine().lower()
    if machine in ("arm64", "aarch64"):
        return "aarch64-unknown-linux-musl"
    if machine in ("x86_64", "amd64"):
        return "x86_64-unknown-linux-musl"
    raise AssertionError(f"unsupported test host architecture: {machine}")


def compile_probe(workspace):
    source = workspace / "eos_shell_security_probe.rs"
    binary = workspace / "eos_shell_security_probe"
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


def file_write(sandbox_id, path, content, *, timeout=90):
    """Overlay/setup write path (daemon file runner), distinct from shell_exec."""
    return runtime(
        sandbox_id, "file_write", "--path", path, "--content", content, timeout=timeout
    )


def file_read(sandbox_id, path, *, timeout=90):
    return runtime(sandbox_id, "file_read", "--path", path, timeout=timeout)


APT_ARCHIVE_SOURCES = (
    'printf "'
    "Types: deb\\n"
    "URIs: http://archive.ubuntu.com/ubuntu/\\n"
    "Suites: noble\\n"
    "Components: main universe restricted multiverse\\n"
    "Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\\n"
    '" > /etc/apt/sources.list.d/ubuntu.sources'
)
APT_OPTIONS = "-o APT::Sandbox::User=root -o Dir::Cache::archives=/tmp/eos-apt-archives"


def apt_install_command(packages, *, then=()):
    """A ``sh -lc`` command that points apt at the ubuntu24 noble archive pockets,
    installs ``packages`` under the reduced caps/seccomp policy (root apt sandbox,
    a root-owned cache under ``/tmp``), then runs any ``then`` follow-up commands.
    """
    body = " && ".join(
        [
            APT_ARCHIVE_SOURCES,
            "mkdir -p /tmp/eos-apt-archives/partial",
            "chmod 0700 /tmp/eos-apt-archives/partial",
            f"apt-get {APT_OPTIONS} update",
            f"apt-get {APT_OPTIONS} install -y --no-install-recommends {' '.join(packages)}",
            *then,
        ]
    )
    return "sh -lc '" + body + "'"


def run_to_completion(sandbox_id, command, *, timeout_s=180, **exec_kwargs):
    """Start a long-running command and poll its session instead of holding the
    daemon request open. Returns the terminal command state."""
    result = exec_cmd(sandbox_id, command, yield_ms=0, timeout_ms=timeout_s * 1000, **exec_kwargs)
    if result.get("status") == "running":
        command_session_id = result.get("command_session_id")
        assert command_session_id, result
        result = wait_command(sandbox_id, command_session_id, timeout_s=timeout_s)
    return result


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
    result = run_probe_raw(sandbox_id)
    assert result.get("status") == "ok", result
    return parse_probe_output(result.get("output", ""))


def run_probe_raw(sandbox_id, *args):
    command = "./eos_shell_security_probe"
    if args:
        command += " " + " ".join(args)
    return exec_cmd(sandbox_id, command)


def parse_probe_output(output):
    report = {}
    for line in output.splitlines():
        key, separator, value = line.partition("=")
        if separator:
            report[key.strip()] = value.strip()
    return report


def has_cap(cap_hex, bit):
    return ((int(cap_hex, 16) >> bit) & 1) == 1
