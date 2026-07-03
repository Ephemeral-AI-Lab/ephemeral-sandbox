use crate::runner::shell_security::{
    build_seccomp_programs, syscall_number, CLONE_NEW_FLAGS, KEEP_CAPABILITIES,
};

const S_IFCHR: u64 = 0o020000;
const S_IFBLK: u64 = 0o060000;
const S_IFREG: u64 = 0o100000;

const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

const BPF_LD: u16 = 0x00;
const BPF_LDX: u16 = 0x01;
const BPF_ST: u16 = 0x02;
const BPF_STX: u16 = 0x03;
const BPF_ALU: u16 = 0x04;
const BPF_JMP: u16 = 0x05;
const BPF_RET: u16 = 0x06;
const BPF_MISC: u16 = 0x07;
const BPF_CLASS_MASK: u16 = 0x07;
const BPF_SIZE_MASK: u16 = 0x18;
const BPF_W: u16 = 0x00;
const BPF_MODE_MASK: u16 = 0xe0;
const BPF_IMM: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_MEM: u16 = 0x60;
const BPF_OP_MASK: u16 = 0xf0;
const BPF_SRC_X: u16 = 0x08;
const BPF_RVAL_A: u16 = 0x10;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

const COMMON_DENIED_SYSCALLS: &[&str] = &[
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
    "init_module",
    "finit_module",
    "delete_module",
    "kexec_load",
    "kexec_file_load",
    "reboot",
    "bpf",
    "perf_event_open",
    "userfaultfd",
    "fanotify_init",
    "io_uring_setup",
    "io_uring_enter",
    "io_uring_register",
    "open_by_handle_at",
    "add_key",
    "request_key",
    "keyctl",
    "swapon",
    "swapoff",
    "quotactl",
];

fn syscall(name: &str) -> i64 {
    syscall_number(name).unwrap_or_else(|| panic!("missing syscall {name}"))
}

fn clone_namespace_mask() -> u64 {
    CLONE_NEW_FLAGS
        .iter()
        .copied()
        .fold(0, |mask, flag| mask | flag)
}

fn errno_action(errno: i32) -> u32 {
    SECCOMP_RET_ERRNO | errno as u32
}

#[derive(Clone, Copy)]
struct SeccompData {
    nr: u32,
    arch: u32,
    args: [u64; 6],
}

fn data(name: &str, args: [u64; 6]) -> SeccompData {
    SeccompData {
        nr: syscall(name) as u32,
        arch: AUDIT_ARCH,
        args,
    }
}

fn run_bpf(program: &[seccompiler::sock_filter], data: SeccompData) -> u32 {
    let mut pc = 0usize;
    let mut accumulator = 0u32;
    let mut index = 0u32;
    let mut memory = [0u32; 16];
    while pc < program.len() {
        let instruction = &program[pc];
        match instruction.code & BPF_CLASS_MASK {
            BPF_LD => {
                accumulator = load_value(instruction.code, instruction.k, &memory, data);
                pc += 1;
            }
            BPF_LDX => {
                index = load_value(instruction.code, instruction.k, &memory, data);
                pc += 1;
            }
            BPF_ST => {
                memory[instruction.k as usize] = accumulator;
                pc += 1;
            }
            BPF_STX => {
                memory[instruction.k as usize] = index;
                pc += 1;
            }
            BPF_ALU => {
                accumulator = alu_value(instruction.code, accumulator, index, instruction.k);
                pc += 1;
            }
            BPF_JMP => {
                pc = jump_target(instruction, pc, accumulator, index);
            }
            BPF_RET => {
                return if instruction.code & BPF_RVAL_A == BPF_RVAL_A {
                    accumulator
                } else {
                    instruction.k
                };
            }
            BPF_MISC => {
                match instruction.code {
                    0x07 => index = accumulator,
                    0x87 => accumulator = index,
                    code => panic!("unsupported BPF misc instruction {code:#x}"),
                }
                pc += 1;
            }
            code => panic!("unsupported BPF class {code:#x}"),
        }
    }
    panic!("BPF program terminated without return")
}

fn load_value(code: u16, value: u32, memory: &[u32; 16], data: SeccompData) -> u32 {
    assert_eq!(code & BPF_SIZE_MASK, BPF_W, "only word loads are supported");
    match code & BPF_MODE_MASK {
        BPF_IMM => value,
        BPF_ABS => data_word(data, value),
        BPF_MEM => memory[value as usize],
        mode => panic!("unsupported BPF load mode {mode:#x}"),
    }
}

fn alu_value(code: u16, accumulator: u32, index: u32, value: u32) -> u32 {
    let rhs = if code & BPF_SRC_X == BPF_SRC_X {
        index
    } else {
        value
    };
    match code & BPF_OP_MASK {
        0x00 => accumulator.wrapping_add(rhs),
        0x10 => accumulator.wrapping_sub(rhs),
        0x20 => accumulator.wrapping_mul(rhs),
        0x30 => accumulator / rhs,
        0x40 => accumulator | rhs,
        0x50 => accumulator & rhs,
        0x60 => accumulator.wrapping_shl(rhs),
        0x70 => accumulator.wrapping_shr(rhs),
        0x80 => accumulator.wrapping_neg(),
        0x90 => accumulator % rhs,
        0xa0 => accumulator ^ rhs,
        op => panic!("unsupported BPF alu op {op:#x}"),
    }
}

fn jump_target(
    instruction: &seccompiler::sock_filter,
    pc: usize,
    accumulator: u32,
    index: u32,
) -> usize {
    if instruction.code & BPF_OP_MASK == 0x00 {
        return pc + instruction.k as usize + 1;
    }
    let rhs = if instruction.code & BPF_SRC_X == BPF_SRC_X {
        index
    } else {
        instruction.k
    };
    let jump = match instruction.code & BPF_OP_MASK {
        0x10 => accumulator == rhs,
        0x20 => accumulator > rhs,
        0x30 => accumulator >= rhs,
        0x40 => (accumulator & rhs) != 0,
        op => panic!("unsupported BPF jump op {op:#x}"),
    };
    pc + if jump {
        instruction.jt as usize
    } else {
        instruction.jf as usize
    } + 1
}

fn data_word(data: SeccompData, offset: u32) -> u32 {
    match offset {
        0 => data.nr,
        4 => data.arch,
        8 | 12 => 0,
        16..=60 => {
            let arg_offset = offset - 16;
            let arg = data.args[(arg_offset / 8) as usize];
            if arg_offset % 8 == 0 {
                arg as u32
            } else {
                (arg >> 32) as u32
            }
        }
        _ => panic!("unsupported seccomp_data offset {offset}"),
    }
}

#[test]
fn kept_capabilities_match_command_policy() {
    assert_eq!(
        KEEP_CAPABILITIES,
        &[0, 1, 2, 3, 4, 7, 6, 31, 5, 10, 13, 27]
    );
}

#[test]
fn seccomp_programs_build_with_arch_guard_first() {
    let programs = build_seccomp_programs().expect("programs build");
    assert_eq!(programs.filters.len(), 2);
    for program in programs.filters {
        assert!(!program.is_empty());
        assert_eq!(program[0].code, 0x20);
        assert_eq!(program[0].k, 4);
        #[cfg(target_arch = "x86_64")]
        assert!(program.iter().any(|instruction| instruction.k == 0x4000_0000));
    }
}

#[test]
fn explicit_denies_return_eperm_in_enforce() {
    let programs = build_seccomp_programs().expect("programs build");
    let errno_filter = &programs.filters[0];
    for name in COMMON_DENIED_SYSCALLS {
        assert_eq!(
            run_bpf(errno_filter, data(name, [0; 6])),
            errno_action(libc::EPERM),
            "{name} should be denied"
        );
    }
}

#[test]
fn clone3_returns_enosys() {
    let programs = build_seccomp_programs().expect("programs build");
    let clone3_filter = &programs.filters[1];
    assert_eq!(
        run_bpf(clone3_filter, data("clone3", [0; 6])),
        errno_action(libc::ENOSYS)
    );
}

#[test]
fn exec_syscalls_remain_allowed() {
    let programs = build_seccomp_programs().expect("programs build");
    for name in [
        "execve",
        "execveat",
        "fchmodat2",
        "getrlimit",
        "renameat",
        "renameat2",
        "setrlimit",
    ] {
        for filter in &programs.filters {
            assert_eq!(run_bpf(filter, data(name, [0; 6])), SECCOMP_RET_ALLOW);
        }
    }
}

#[test]
fn namespace_creation_is_denied_in_enforce() {
    let programs = build_seccomp_programs().expect("programs build");
    let errno_filter = &programs.filters[0];
    let clone_args = [clone_namespace_mask(), 0, 0, 0, 0, 0];
    assert_eq!(
        run_bpf(errno_filter, data("clone", clone_args)),
        errno_action(libc::EPERM)
    );
    assert_eq!(
        run_bpf(
            errno_filter,
            data("clone", [libc::SIGCHLD as u64, 0, 0, 0, 0, 0])
        ),
        SECCOMP_RET_ALLOW
    );
    for name in ["setns", "unshare"] {
        assert_eq!(
            run_bpf(errno_filter, data(name, [0; 6])),
            errno_action(libc::EPERM)
        );
    }
}

#[test]
fn each_clone_namespace_flag_is_denied_in_enforce() {
    let programs = build_seccomp_programs().expect("programs build");
    let errno_filter = &programs.filters[0];
    for flag in CLONE_NEW_FLAGS {
        let clone_args = [*flag, 0, 0, 0, 0, 0];
        assert_eq!(
            run_bpf(errno_filter, data("clone", clone_args)),
            errno_action(libc::EPERM),
            "clone flag {flag:#x} should be denied"
        );
    }
}

#[test]
fn device_mknod_is_denied_but_regular_nodes_are_not() {
    let programs = build_seccomp_programs().expect("programs build");
    let errno_filter = &programs.filters[0];
    assert_eq!(
        run_bpf(errno_filter, data("mknodat", [0, 0, S_IFCHR, 0, 0, 0])),
        errno_action(libc::EPERM)
    );
    assert_eq!(
        run_bpf(errno_filter, data("mknodat", [0, 0, S_IFBLK, 0, 0, 0])),
        errno_action(libc::EPERM)
    );
    assert_eq!(
        run_bpf(errno_filter, data("mknodat", [0, 0, S_IFREG, 0, 0, 0])),
        SECCOMP_RET_ALLOW
    );
    if syscall_number("mknod").is_some() {
        assert_eq!(
            run_bpf(errno_filter, data("mknod", [0, S_IFCHR, 0, 0, 0, 0])),
            errno_action(libc::EPERM)
        );
    }
}
