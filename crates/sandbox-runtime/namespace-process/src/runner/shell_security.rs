//! Command-child security policy.
//!
//! Seccomp is a deny table: a fixed set of dangerous syscall families is
//! rejected and every other syscall is allowed, so the policy stays compatible
//! with arbitrary images without vendoring an allowlist. Filters are built in
//! the parent before `fork`, then installed in the child from prebuilt BPF
//! slices immediately before `execve`.

use std::collections::BTreeMap;
use std::io;
use std::sync::OnceLock;

use seccompiler::{
    sock_filter, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch,
};

const PR_CAPBSET_DROP: libc::c_int = 24;
const PR_SET_NO_NEW_PRIVS: libc::c_int = 38;
const PR_CAP_AMBIENT: libc::c_int = 47;
const PR_CAP_AMBIENT_CLEAR_ALL: libc::c_ulong = 4;
const SECCOMP_SET_MODE_FILTER: libc::c_long = 1;
const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
const MAX_CAPABILITY: u32 = 40;
const CAP_WORDS: usize = 2;

const CAP_CHOWN: u32 = 0;
const CAP_DAC_OVERRIDE: u32 = 1;
const CAP_DAC_READ_SEARCH: u32 = 2;
const CAP_FOWNER: u32 = 3;
const CAP_FSETID: u32 = 4;
const CAP_KILL: u32 = 5;
const CAP_SETGID: u32 = 6;
const CAP_SETUID: u32 = 7;
const CAP_NET_BIND_SERVICE: u32 = 10;
const CAP_NET_RAW: u32 = 13;
const CAP_MKNOD: u32 = 27;
const CAP_SETFCAP: u32 = 31;

pub(crate) const KEEP_CAPABILITIES: &[u32] = &[
    CAP_CHOWN,
    CAP_DAC_OVERRIDE,
    CAP_DAC_READ_SEARCH,
    CAP_FOWNER,
    CAP_FSETID,
    CAP_SETUID,
    CAP_SETGID,
    CAP_SETFCAP,
    CAP_KILL,
    CAP_NET_BIND_SERVICE,
    CAP_NET_RAW,
    CAP_MKNOD,
];

const CLONE_NEWNS: u64 = 0x0002_0000;
const CLONE_NEWCGROUP: u64 = 0x0200_0000;
const CLONE_NEWUTS: u64 = 0x0400_0000;
const CLONE_NEWIPC: u64 = 0x0800_0000;
const CLONE_NEWUSER: u64 = 0x1000_0000;
const CLONE_NEWPID: u64 = 0x2000_0000;
const CLONE_NEWNET: u64 = 0x4000_0000;
const CLONE_NEWTIME: u64 = 0x0000_0080;
pub(crate) const CLONE_NEW_FLAGS: &[u64] = &[
    CLONE_NEWNS,
    CLONE_NEWCGROUP,
    CLONE_NEWUTS,
    CLONE_NEWIPC,
    CLONE_NEWUSER,
    CLONE_NEWPID,
    CLONE_NEWNET,
    CLONE_NEWTIME,
];

const S_IFMT: u64 = 0o170000;
const S_IFCHR: u64 = 0o020000;
const S_IFBLK: u64 = 0o060000;

const FILESYSTEM_DENY_SYSCALLS: &[&str] = &[
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
];

const NAMESPACE_DENY_SYSCALLS: &[&str] = &["setns", "unshare"];

const SYSTEM_DENY_SYSCALLS: &[&str] = &[
    "init_module",
    "finit_module",
    "delete_module",
    "kexec_load",
    "kexec_file_load",
    "reboot",
];

const OBSERVABILITY_DENY_SYSCALLS: &[&str] = &[
    "bpf",
    "perf_event_open",
    "userfaultfd",
    "fanotify_init",
    "io_uring_setup",
    "io_uring_enter",
    "io_uring_register",
];

const RESOURCE_DENY_SYSCALLS: &[&str] = &[
    "open_by_handle_at",
    "add_key",
    "request_key",
    "keyctl",
    "swapon",
    "swapoff",
    "quotactl",
];

pub(crate) struct SeccompPrograms {
    pub(crate) filters: Box<[BpfProgram]>,
}

#[repr(C)]
struct SockFprog {
    len: libc::c_ushort,
    filter: *const sock_filter,
}

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

static ENFORCE_PROGRAMS: OnceLock<SeccompPrograms> = OnceLock::new();

#[cfg(target_arch = "x86_64")]
const SYS_CAPSET: libc::c_long = 126;
#[cfg(target_arch = "x86_64")]
const SYS_SECCOMP: libc::c_long = 317;

#[cfg(target_arch = "aarch64")]
const SYS_CAPSET: libc::c_long = 91;
#[cfg(target_arch = "aarch64")]
const SYS_SECCOMP: libc::c_long = 277;

pub(crate) fn prepare_shell_security_policy() -> io::Result<()> {
    prepare_seccomp_program(&ENFORCE_PROGRAMS)
}

pub(crate) fn apply_shell_security_policy() -> io::Result<()> {
    set_no_new_privs()?;
    drop_capabilities()?;
    install_prepared_filters(&ENFORCE_PROGRAMS)
}

fn prepare_seccomp_program(slot: &'static OnceLock<SeccompPrograms>) -> io::Result<()> {
    if slot.get().is_some() {
        return Ok(());
    }
    let programs = build_seccomp_programs()?;
    let _ = slot.set(programs);
    Ok(())
}

fn install_prepared_filters(slot: &'static OnceLock<SeccompPrograms>) -> io::Result<()> {
    let Some(programs) = slot.get() else {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    };
    for program in &programs.filters {
        install_seccomp_filter(program)?;
    }
    Ok(())
}

pub(crate) fn build_seccomp_programs() -> io::Result<SeccompPrograms> {
    let filters = vec![build_errno_filter(libc::EPERM)?, build_clone3_filter()?];
    Ok(SeccompPrograms {
        filters: filters.into_boxed_slice(),
    })
}

fn build_errno_filter(errno: i32) -> io::Result<BpfProgram> {
    let mut rules = BTreeMap::new();
    for name in FILESYSTEM_DENY_SYSCALLS {
        add_syscall_rule(&mut rules, name, vec![])?;
    }
    for name in NAMESPACE_DENY_SYSCALLS {
        add_syscall_rule(&mut rules, name, vec![])?;
    }
    add_clone_namespace_rule(&mut rules)?;
    for name in SYSTEM_DENY_SYSCALLS {
        add_syscall_rule(&mut rules, name, vec![])?;
    }
    for name in OBSERVABILITY_DENY_SYSCALLS {
        add_syscall_rule(&mut rules, name, vec![])?;
    }
    for name in RESOURCE_DENY_SYSCALLS {
        add_syscall_rule(&mut rules, name, vec![])?;
    }
    add_mknod_rules(&mut rules)?;
    bpf_filter(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(errno as u32),
    )
}

fn build_clone3_filter() -> io::Result<BpfProgram> {
    let mut rules = BTreeMap::new();
    add_syscall_rule(&mut rules, "clone3", vec![])?;
    bpf_filter(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::ENOSYS as u32),
    )
}

fn bpf_filter(
    rules: BTreeMap<i64, Vec<SeccompRule>>,
    mismatch: SeccompAction,
    matched: SeccompAction,
) -> io::Result<BpfProgram> {
    let filter =
        SeccompFilter::new(rules, mismatch, matched, target_arch()).map_err(seccomp_build_error)?;
    let mut program = BpfProgram::try_from(filter).map_err(seccomp_build_error)?;
    reject_x32_abi(&mut program);
    Ok(program)
}

fn add_syscall_rule(
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
    name: &str,
    syscall_rules: Vec<SeccompRule>,
) -> io::Result<()> {
    if let Some(syscall) = syscall_number(name) {
        rules.insert(syscall, syscall_rules);
    }
    Ok(())
}

fn add_clone_namespace_rule(rules: &mut BTreeMap<i64, Vec<SeccompRule>>) -> io::Result<()> {
    let Some(syscall) = syscall_number("clone") else {
        return Ok(());
    };
    let mut namespace_rules = Vec::with_capacity(CLONE_NEW_FLAGS.len());
    for flag in CLONE_NEW_FLAGS {
        namespace_rules.push(
            SeccompRule::new(vec![SeccompCondition::new(
                0,
                SeccompCmpArgLen::Qword,
                SeccompCmpOp::MaskedEq(*flag),
                *flag,
            )
            .map_err(seccomp_build_error)?])
            .map_err(seccomp_build_error)?,
        );
    }
    rules.insert(syscall, namespace_rules);
    Ok(())
}

fn add_mknod_rules(rules: &mut BTreeMap<i64, Vec<SeccompRule>>) -> io::Result<()> {
    add_device_node_rule(rules, "mknod", 1)?;
    add_device_node_rule(rules, "mknodat", 2)
}

fn add_device_node_rule(
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
    name: &str,
    mode_arg: u8,
) -> io::Result<()> {
    let Some(syscall) = syscall_number(name) else {
        return Ok(());
    };
    let char_rule = masked_mode_rule(mode_arg, S_IFCHR)?;
    let block_rule = masked_mode_rule(mode_arg, S_IFBLK)?;
    rules.insert(syscall, vec![char_rule, block_rule]);
    Ok(())
}

fn masked_mode_rule(mode_arg: u8, mode_type: u64) -> io::Result<SeccompRule> {
    SeccompRule::new(vec![SeccompCondition::new(
        mode_arg,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::MaskedEq(S_IFMT),
        mode_type,
    )
    .map_err(seccomp_build_error)?])
    .map_err(seccomp_build_error)
}

fn target_arch() -> TargetArch {
    #[cfg(target_arch = "x86_64")]
    {
        TargetArch::x86_64
    }
    #[cfg(target_arch = "aarch64")]
    {
        TargetArch::aarch64
    }
}

#[cfg(target_arch = "x86_64")]
fn reject_x32_abi(program: &mut BpfProgram) {
    let guard = [
        bpf_stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFFSET),
        bpf_jump(BPF_JMP | BPF_JGE | BPF_K, X32_SYSCALL_BIT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
    ];
    program.splice(3..3, guard);
}

#[cfg(not(target_arch = "x86_64"))]
fn reject_x32_abi(_program: &mut BpfProgram) {}

fn set_no_new_privs() -> io::Result<()> {
    // SAFETY: prctl is called with fixed integer arguments and no borrowed memory.
    let rc = unsafe { libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    syscall_result(rc)
}

fn drop_capabilities() -> io::Result<()> {
    // SAFETY: prctl is called with fixed integer arguments and no borrowed memory.
    let ambient_rc = unsafe {
        libc::prctl(
            PR_CAP_AMBIENT,
            PR_CAP_AMBIENT_CLEAR_ALL,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    syscall_result(ambient_rc)?;
    for cap in 0..=MAX_CAPABILITY {
        if !capability_is_kept(cap) {
            drop_bounding_capability(cap)?;
        }
    }
    capset_keep_set()
}

fn drop_bounding_capability(capability: u32) -> io::Result<()> {
    // SAFETY: prctl is called with fixed integer arguments and no borrowed memory.
    let rc = unsafe {
        libc::prctl(
            PR_CAPBSET_DROP,
            capability as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EINVAL) {
        Ok(())
    } else {
        Err(err)
    }
}

fn capset_keep_set() -> io::Result<()> {
    let header = CapHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; CAP_WORDS];
    for capability in KEEP_CAPABILITIES {
        let word = (*capability / 32) as usize;
        let bit = 1u32 << (*capability % 32);
        data[word].effective |= bit;
        data[word].permitted |= bit;
        data[word].inheritable |= bit;
    }
    // SAFETY: capset reads the fixed header and two-word capability array for this process.
    let rc = unsafe { libc::syscall(SYS_CAPSET, &header, data.as_mut_ptr()) };
    syscall_result_long(rc)
}

fn install_seccomp_filter(program: &BpfProgram) -> io::Result<()> {
    let filter = SockFprog {
        len: program
            .len()
            .try_into()
            .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?,
        filter: program.as_ptr(),
    };
    // SAFETY: seccomp reads the sock_fprog and immutable BPF slice prepared before pre_exec.
    let rc = unsafe { libc::syscall(SYS_SECCOMP, SECCOMP_SET_MODE_FILTER, 0, &filter) };
    syscall_result_long(rc)
}

fn syscall_result(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn syscall_result_long(rc: libc::c_long) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn seccomp_build_error(err: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, err.to_string())
}

fn capability_is_kept(capability: u32) -> bool {
    KEEP_CAPABILITIES.contains(&capability)
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn syscall_number(name: &str) -> Option<i64> {
    match name {
        "add_key" => Some(248),
        "bpf" => Some(321),
        "clone" => Some(56),
        "clone3" => Some(435),
        "delete_module" => Some(176),
        "execve" => Some(59),
        "execveat" => Some(322),
        "fanotify_init" => Some(300),
        "fchmodat2" => Some(452),
        "finit_module" => Some(313),
        "fsconfig" => Some(431),
        "fsmount" => Some(432),
        "fsopen" => Some(430),
        "fspick" => Some(433),
        "getrlimit" => Some(97),
        "init_module" => Some(175),
        "io_uring_enter" => Some(426),
        "io_uring_register" => Some(427),
        "io_uring_setup" => Some(425),
        "keyctl" => Some(250),
        "kexec_file_load" => Some(320),
        "kexec_load" => Some(246),
        "mknod" => Some(133),
        "mknodat" => Some(259),
        "mount" => Some(165),
        "mount_setattr" => Some(442),
        "move_mount" => Some(429),
        "open_by_handle_at" => Some(304),
        "open_tree" => Some(428),
        "perf_event_open" => Some(298),
        "pivot_root" => Some(155),
        "quotactl" => Some(179),
        "reboot" => Some(169),
        "renameat" => Some(264),
        "renameat2" => Some(316),
        "request_key" => Some(249),
        "seccomp" => Some(317),
        "setrlimit" => Some(160),
        "setns" => Some(308),
        "swapoff" => Some(168),
        "swapon" => Some(167),
        "umount2" => Some(166),
        "unshare" => Some(272),
        "userfaultfd" => Some(323),
        _ => None,
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn syscall_number(name: &str) -> Option<i64> {
    match name {
        "add_key" => Some(217),
        "bpf" => Some(280),
        "clone" => Some(220),
        "clone3" => Some(435),
        "delete_module" => Some(106),
        "execve" => Some(221),
        "execveat" => Some(281),
        "fanotify_init" => Some(262),
        "fchmodat2" => Some(452),
        "finit_module" => Some(273),
        "fsconfig" => Some(431),
        "fsmount" => Some(432),
        "fsopen" => Some(430),
        "fspick" => Some(433),
        "getrlimit" => Some(163),
        "init_module" => Some(105),
        "io_uring_enter" => Some(426),
        "io_uring_register" => Some(427),
        "io_uring_setup" => Some(425),
        "keyctl" => Some(219),
        "kexec_file_load" => Some(294),
        "kexec_load" => Some(104),
        "mknodat" => Some(33),
        "mount" => Some(40),
        "mount_setattr" => Some(442),
        "move_mount" => Some(429),
        "open_by_handle_at" => Some(265),
        "open_tree" => Some(428),
        "perf_event_open" => Some(241),
        "pivot_root" => Some(41),
        "quotactl" => Some(60),
        "reboot" => Some(142),
        "renameat" => Some(38),
        "renameat2" => Some(276),
        "request_key" => Some(218),
        "seccomp" => Some(277),
        "setrlimit" => Some(164),
        "setns" => Some(268),
        "swapoff" => Some(225),
        "swapon" => Some(224),
        "umount2" => Some(39),
        "unshare" => Some(97),
        "userfaultfd" => Some(282),
        _ => None,
    }
}

#[cfg(target_arch = "x86_64")]
const BPF_LD: u16 = 0x00;
#[cfg(target_arch = "x86_64")]
const BPF_W: u16 = 0x00;
#[cfg(target_arch = "x86_64")]
const BPF_ABS: u16 = 0x20;
#[cfg(target_arch = "x86_64")]
const BPF_JMP: u16 = 0x05;
#[cfg(target_arch = "x86_64")]
const BPF_JGE: u16 = 0x30;
#[cfg(target_arch = "x86_64")]
const BPF_RET: u16 = 0x06;
#[cfg(target_arch = "x86_64")]
const BPF_K: u16 = 0x00;
#[cfg(target_arch = "x86_64")]
const SECCOMP_DATA_NR_OFFSET: u32 = 0;
#[cfg(target_arch = "x86_64")]
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

#[cfg(target_arch = "x86_64")]
const fn bpf_stmt(code: u16, k: u32) -> sock_filter {
    sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

#[cfg(target_arch = "x86_64")]
const fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> sock_filter {
    sock_filter { code, jt, jf, k }
}
