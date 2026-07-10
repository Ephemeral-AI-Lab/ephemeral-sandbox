"""SS-H01..SS-H15 — hard tier.

Adversarial / exploit-shaped, arch-specific, and orchestration-heavy cases. Most
denial checks read the shared module probe; the X32 reject, setuid/setcap
helpers, and the same-pgid subtree take dedicated sandboxes. Each test is named
after its SS-ID and carries the catalog's Guards text as the assert message.

Image: ubuntu:24.04 (pinned via ``core.config.IMAGE`` / ``E2E_IMAGE``).
"""

import pytest

from core.cli import is_error
from runtime.shell_security.helpers import (
    CAP_SYS_ADMIN,
    apt_install_command,
    exec_cmd,
    file_write,
    has_cap,
    linux_musl_target,
    parse_probe_output,
    run_probe_raw,
    run_to_completion,
)

pytestmark = pytest.mark.hard

_TARGET_IS_X86_64 = linux_musl_target().startswith("x86_64")

_NEW_MOUNT_API = ("fsopen", "fsconfig", "fsmount", "fspick", "move_mount", "open_tree")
_MOUNT_MUTATION_FAMILY = ("umount2", "pivot_root", "mount_setattr")
_MODULE_LOAD = ("init_module", "finit_module")
_IO_URING = ("io_uring", "io_uring_enter", "io_uring_register")
_KERNEL_LPE = ("userfaultfd", "perf_event_open", "fanotify_init")
_RESOURCE_DOS = ("swapon", "swapoff", "quotactl", "reboot")


def test_ss_h01_userns_escape_primitive_denied(probe):
    assert probe["unshare_newuser"] == "EPERM", f"blocks the userns→full-cap-set pivot; not cap-gated | unshare_newuser={probe.get('unshare_newuser')}"


def test_ss_h02_clone_new_flag_mask_denies_exactly(probe):
    assert probe["clone_newuser"] == "EPERM", f"flag-mask rule denies exactly the CLONE_NEW* bits | clone_newuser={probe.get('clone_newuser')}"
    assert probe["clone_sigchld"] == "OK", f"flag-mask rule denies exactly the CLONE_NEW* bits | clone_sigchld={probe.get('clone_sigchld')}"


def test_ss_h03_clone3_blanket_enosys(probe):
    assert probe["clone3"] == "ENOSYS", f"seccomp can't deref the clone3 args pointer → blanket ENOSYS | clone3={probe.get('clone3')}"
    assert probe["clone3_args"] == "ENOSYS", f"seccomp can't deref the clone3 args pointer → blanket ENOSYS | clone3_args={probe.get('clone3_args')}"


@pytest.mark.skipif(not _TARGET_IS_X86_64, reason="X32 ABI reject is x86_64-only; skip on aarch64")
def test_ss_h04_x32_abi_rejected(module_sandbox, probe):
    result = run_probe_raw(module_sandbox, "x32")
    assert result.get("status") != "ok", f"killed (SECCOMP_RET_KILL_PROCESS); native call unaffected | x32 not rejected: {result}"
    assert "x32_survived" not in result.get("output", ""), f"killed (SECCOMP_RET_KILL_PROCESS); native call unaffected | x32 filter absent: {result}"
    assert probe["nnp"] == "1", f"killed (SECCOMP_RET_KILL_PROCESS); native call unaffected | native probe broken: {probe}"


def test_ss_h05_open_by_handle_at_denied(probe):
    assert probe["open_by_handle_at"] == "EPERM", f"seccomp is the only barrier — DAC_READ_SEARCH is kept | open_by_handle_at={probe.get('open_by_handle_at')}"


def test_ss_h06_new_mount_api_denied(probe):
    for name in _NEW_MOUNT_API:
        assert probe.get(name) == "EPERM", f"can't sidestep the classic mount denial via the new API | {name}={probe.get(name)}"


def test_ss_h07_mount_mutation_family_denied(probe):
    for name in _MOUNT_MUTATION_FAMILY:
        assert probe.get(name) == "EPERM", f"full mount-mutation family, not just mount | {name}={probe.get(name)}"


def test_ss_h08_module_load_denied(probe):
    for name in _MODULE_LOAD:
        assert probe.get(name) == "EPERM", f"kernel-module load (T3) — SYS_MODULE dropped + seccomp | {name}={probe.get(name)}"


def test_ss_h09_io_uring_ring_closed(probe):
    for name in _IO_URING:
        assert probe.get(name) == "EPERM", f"close the whole ring — io_uring is a proxy-syscall bypass surface | {name}={probe.get(name)}"


def test_ss_h10_kernel_lpe_surfaces_denied(probe):
    for name in _KERNEL_LPE:
        assert probe.get(name) == "EPERM", f"non-cap-gated LPE surfaces — seccomp load-bearing | {name}={probe.get(name)}"


def test_ss_h11_no_new_privs_neutralizes_setuid(fresh_sandbox):
    # The sandbox userns maps only uid 0 (`uid_map: 0 0 1`), so a setuid-root exec
    # cannot be observed as a uid *raise*. NoNewPrivs is what makes that safe: it
    # is inherited across the setuid execve, the euid does not transition, and no
    # dropped capability is regained. We assert exactly those invariants on a
    # genuine setuid-root helper (`test -u` guarantees the bit is set).
    stage = exec_cmd(
        fresh_sandbox,
        "sh -lc 'cp eos_shell_security_probe /tmp/eos-suid-helper && "
        "chown 0:0 /tmp/eos-suid-helper && chmod u+s,go+rx /tmp/eos-suid-helper && "
        "test -u /tmp/eos-suid-helper'",
    )
    assert stage.get("status") == "ok", f"no_new_privs neutralizes setuid | could not stage a setuid-root helper: {stage}"

    run = exec_cmd(fresh_sandbox, "/tmp/eos-suid-helper privinfo")
    assert run.get("status") == "ok", f"no_new_privs neutralizes setuid | helper did not run: {run}"
    report = parse_probe_output(run.get("output", ""))
    assert report["nnp"] == "1", f"no_new_privs neutralizes setuid | setuid-root execve not under NNP: {report}"
    assert report["euid"] == report["ruid"], f"no_new_privs neutralizes setuid | setuid changed euid {report.get('ruid')}->{report.get('euid')}"
    assert not has_cap(report["capeff"], CAP_SYS_ADMIN), f"no_new_privs neutralizes setuid | setuid-root regained SYS_ADMIN: capeff={report.get('capeff')}"


def test_ss_h12_file_caps_defeated(fresh_sandbox):
    # setcap ships in libcap2-bin, which ubuntu:24.04 lacks by default — install it
    # (needs package egress), else skip cleanly.
    if exec_cmd(fresh_sandbox, "sh -lc 'command -v setcap'").get("status") != "ok":
        install = run_to_completion(
            fresh_sandbox, apt_install_command(["libcap2-bin"]), timeout_s=180
        )
        if exec_cmd(fresh_sandbox, "sh -lc 'command -v setcap'").get("status") != "ok":
            pytest.skip(f"setcap unavailable (no package egress for libcap2-bin): {install}")

    staged = exec_cmd(
        fresh_sandbox,
        "sh -lc 'cp eos_shell_security_probe /tmp/eos-cap-helper && "
        "setcap cap_sys_admin+ep /tmp/eos-cap-helper && echo SETCAP_OK && "
        "{ /tmp/eos-cap-helper privinfo || echo EXEC_DEFEATED=$?; }'",
    )
    output = staged.get("output", "")
    assert "SETCAP_OK" in output, f"NNP + bounding-set drop defeat file caps | setcap cap_sys_admin+ep did not succeed: {staged}"
    if "EXEC_DEFEATED" in output:
        # The +e file cap names SYS_ADMIN, which was dropped from the bounding set,
        # so the effective cap can never be honored and the kernel refuses the
        # execve (EPERM) — the file cap is defeated before the child even starts.
        return
    report = parse_probe_output(output)
    assert report["nnp"] == "1", f"NNP + bounding-set drop defeat file caps | {report}"
    assert not has_cap(report["capeff"], CAP_SYS_ADMIN), f"NNP + bounding-set drop defeat file caps | capeff={report.get('capeff')}"


def test_ss_h13_setns_denied_distinctly(probe):
    assert probe["setns"] == "EPERM", f"setns denied distinctly from unshare | setns={probe.get('setns')}"


def test_ss_h14_resource_dos_family_denied(probe):
    for name in _RESOURCE_DOS:
        assert probe.get(name) == "EPERM", f"resource/DoS family (T7) — belt-and-suspenders (cap + seccomp) | {name}={probe.get(name)}"


def test_ss_h15_same_pgid_subtree_all_denied(fresh_sandbox):
    subtree = run_to_completion(
        fresh_sandbox,
        "sh -lc 'for i in 1 2 3 4; do ./eos_shell_security_probe > /tmp/eos-h15-$i.out 2>&1 & done; "
        "wait; grep -h \"^mount=\" /tmp/eos-h15-*.out'",
        timeout_s=120,
    )
    assert subtree.get("status") == "ok", f"scope-wait + policy scoped to the shell-exec child only | subtree {subtree}"
    mount_lines = [line for line in subtree.get("output", "").splitlines() if line.startswith("mount=")]
    assert len(mount_lines) == 4, f"scope-wait + policy scoped to the shell-exec child only | subtree probes: {mount_lines}"
    assert all(line == "mount=EPERM" for line in mount_lines), f"scope-wait + policy scoped to the shell-exec child only | {mount_lines}"

    alive = exec_cmd(fresh_sandbox, "id -u")
    assert alive.get("status") == "ok", f"scope-wait + policy scoped to the shell-exec child only | daemon/ns-runner not alive: {alive}"
    overlay = file_write(fresh_sandbox, "h15.txt", "ss-h15\n")
    assert not is_error(overlay), f"scope-wait + policy scoped to the shell-exec child only | overlay write path lost privilege: {overlay}"
