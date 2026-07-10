"""SS-M01..SS-M15 — medium tier.

Full deny/allow set sweeps, capability decode, real ``util-linux`` / ``apt``
tooling, and multi-step scoping checks. Read-only cases share the module probe;
package installs and mutation take a dedicated ``fresh_sandbox``. Each test is
named after its SS-ID and carries the catalog's Guards text as the assert
message.

Image: ubuntu:24.04 (pinned via ``core.config.IMAGE`` / ``E2E_IMAGE``).
"""

import pytest

from core.cli import is_error
from runtime.shell_security.helpers import (
    ALLOWED_SYSCALLS,
    CAP_CHOWN,
    CAP_DAC_OVERRIDE,
    CAP_FOWNER,
    CAP_NET_ADMIN,
    CAP_SETFCAP,
    CAP_SYS_ADMIN,
    CAP_SYS_MODULE,
    DENIED_SYSCALLS,
    apt_install_command,
    exec_cmd,
    file_read,
    file_write,
    has_cap,
    run_probe,
    run_to_completion,
)

pytestmark = pytest.mark.medium


def test_ss_m01_full_deny_set(probe):
    for name in DENIED_SYSCALLS:
        assert probe.get(name) == "EPERM", f"full current deny set in one run | {name}={probe.get(name)}"


def test_ss_m02_full_allow_set(probe):
    for name in ALLOWED_SYSCALLS:
        if name == "fchmodat2":
            assert probe.get(name) in ("OK", "ENOSYS"), f"usability refinements not swept up | {name}={probe.get(name)}"
            continue
        assert probe.get(name) == "OK", f"usability refinements not swept up | {name}={probe.get(name)}"


def test_ss_m03_system_power_caps_dropped(probe):
    for cap in (CAP_SYS_ADMIN, CAP_NET_ADMIN, CAP_SYS_MODULE):
        assert not has_cap(probe["capeff"], cap), f"system-power caps dropped | capeff={probe.get('capeff')} bit={cap}"


def test_ss_m04_fs_identity_caps_kept(probe):
    for cap in (CAP_CHOWN, CAP_DAC_OVERRIDE, CAP_FOWNER, CAP_SETFCAP):
        assert has_cap(probe["capeff"], cap), f"FS/identity caps kept so pkg managers work | capeff={probe.get('capeff')} bit={cap}"


def test_ss_m05_sys_admin_dropped_from_bounding_set(probe):
    assert not has_cap(probe["capbnd"], CAP_SYS_ADMIN), f"dropped from the bounding set → not re-gainable via execve | capbnd={probe.get('capbnd')}"


def test_ss_m06_image_unshare_denied(module_sandbox):
    result = exec_cmd(
        module_sandbox,
        "sh -lc 'command -v unshare >/dev/null || exit 77; unshare -m true'",
    )
    if result.get("status") == "error" and result.get("exit_code") == 77:
        pytest.skip("image does not include unshare")
    assert result.get("status") != "ok", f"filter holds against real util-linux | {result}"


def test_ss_m07_image_mount_tools_denied(module_sandbox):
    mount_result = exec_cmd(
        module_sandbox,
        "sh -lc 'mkdir -p /tmp/eos-cs-tool-mount && mount -t tmpfs none /tmp/eos-cs-tool-mount'",
    )
    assert mount_result.get("status") != "ok", f"mount tools rejected | mount: {mount_result}"

    umount_result = exec_cmd(module_sandbox, "umount /workspace")
    assert umount_result.get("status") != "ok", f"mount tools rejected | umount: {umount_result}"


def test_ss_m08_package_manager_starts(module_sandbox):
    version = exec_cmd(module_sandbox, "apt-get --version")
    assert version.get("status") == "ok", f"pkg manager runs under reduced caps/seccomp | {version}"


def test_ss_m09_real_install_under_policy(fresh_sandbox):
    version = exec_cmd(fresh_sandbox, "apt-get --version")
    assert version.get("status") == "ok", f"real install under policy; poll the session | {version}"

    install = run_to_completion(
        fresh_sandbox,
        apt_install_command(["hello"], then=["hello >/tmp/eos-cs-hello.out"]),
        timeout_s=180,
    )
    if install.get("status") != "ok":
        output = repr(install).lower()
        for marker in ("operation not permitted", "setgroups", "seccomp", "capability"):
            assert marker not in output, f"real install under policy; poll the session | {install}"
        pytest.skip(f"apt install unavailable (no package egress): {install}")


def test_ss_m10_dac_override_kept(probe):
    assert probe["dac_override"] == "OK", f"DAC_OVERRIDE kept | dac_override={probe.get('dac_override')}"


def test_ss_m11_device_node_mode_filter_both_directions(probe):
    assert probe["mknod_char"] == "EPERM", f"mode-filter both directions | mknod_char={probe.get('mknod_char')}"
    assert probe["mknod_block"] == "EPERM", f"mode-filter both directions | mknod_block={probe.get('mknod_block')}"
    assert probe["mknod_fifo"] == "OK", f"mode-filter both directions | mknod_fifo={probe.get('mknod_fifo')}"
    assert probe["mknod_regular"] == "OK", f"mode-filter both directions | mknod_regular={probe.get('mknod_regular')}"


def test_ss_m12_rename_and_chmod_allowed(probe):
    assert probe["renameat"] == "OK", f"not caught by the deny table | renameat={probe.get('renameat')}"
    assert probe["renameat2"] == "OK", f"not caught by the deny table | renameat2={probe.get('renameat2')}"
    assert probe["fchmodat2"] in ("OK", "ENOSYS"), f"not caught by the deny table | fchmodat2={probe.get('fchmodat2')}"


def test_ss_m13_ptrace_kept_within_pid_namespace(probe):
    assert probe["ptrace"] == "OK", f"ptrace kept, confined to the PID namespace | ptrace(TRACEME)={probe.get('ptrace')}"
    assert probe["ptrace_attach"] == "OK", f"ptrace kept, confined to the PID namespace | ptrace(ATTACH)={probe.get('ptrace_attach')}"


def test_ss_m14_every_child_independently_hardened(module_sandbox):
    first = run_probe(module_sandbox)
    second = run_probe(module_sandbox)
    for label, report in (("run1", first), ("run2", second)):
        assert report["nnp"] == "1", f"every child independently hardened; no leak/persistence | {label} nnp={report.get('nnp')}"
        assert report["seccomp"] == "2", f"every child independently hardened; no leak/persistence | {label} seccomp={report.get('seccomp')}"


def test_ss_m15_policy_scoped_to_shell_exec_child(module_sandbox, probe):
    content = "ss-m15-overlay"
    written = file_write(module_sandbox, "m15.txt", content)
    assert not is_error(written), f"policy scoped to the shell-exec child, not setup | write {written}"
    read = file_read(module_sandbox, "m15.txt")
    assert not is_error(read) and read.get("content") == content, f"policy scoped to the shell-exec child, not setup | read {read}"

    assert probe["nnp"] == "1", f"policy scoped to the shell-exec child, not setup | probe nnp={probe.get('nnp')}"
    assert probe["mount"] == "EPERM", f"policy scoped to the shell-exec child, not setup | probe mount={probe.get('mount')}"
