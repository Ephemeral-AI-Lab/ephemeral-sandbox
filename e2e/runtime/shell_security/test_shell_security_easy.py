"""SS-E01..SS-E10 — easy tier.

One deterministic assertion per case: a single syscall verdict or status field
from the in-process probe, or a single command through the daemon path. Each test
is named after its SS-ID and carries the catalog's Guards text as the assert
message so a failure traces straight back to the case.

Image: ubuntu:24.04 (pinned via ``core.config.IMAGE`` / ``E2E_IMAGE``).
"""

import pytest

from core.cli import is_error
from runtime.shell_security.helpers import exec_cmd, file_read, file_write

pytestmark = pytest.mark.easy


def test_ss_e01_normal_commands_still_work(module_sandbox):
    for command in ("id -u", "uname -m", "sh -lc 'echo ok'"):
        result = exec_cmd(module_sandbox, command)
        assert not is_error(result), f"policy doesn't break normal commands; daemon path intact | {command}: {result}"
        assert result.get("status") == "ok", f"policy doesn't break normal commands; daemon path intact | {command}: {result}"
        assert result.get("output", "").strip(), f"policy doesn't break normal commands; daemon path intact | {command}: {result}"


def test_ss_e02_workspace_write_path_unaffected(module_sandbox):
    content = "ss-e02-roundtrip"
    written = file_write(module_sandbox, "e02.txt", content)
    assert not is_error(written), f"overlay/workspace write path unaffected | {written}"
    read = file_read(module_sandbox, "e02.txt")
    assert not is_error(read), f"overlay/workspace write path unaffected | {read}"
    assert read.get("content") == content, f"overlay/workspace write path unaffected | {read}"


def test_ss_e03_no_new_privs_installed(probe):
    assert probe["nnp"] == "1", f"NNP installed before seccomp | nnp={probe.get('nnp')}"


def test_ss_e04_seccomp_filter_mode_active(probe):
    assert probe["seccomp"] == "2", f"filter mode active | seccomp={probe.get('seccomp')}"


def test_ss_e05_mount_mutation_denied(probe):
    assert probe["mount"] == "EPERM", f"mount-mutation denial (T1) | mount={probe.get('mount')}"


def test_ss_e06_namespace_mutation_denied(probe):
    assert probe["unshare_newns"] == "EPERM", f"namespace-mutation denial (T2) — not cap-gated | unshare_newns={probe.get('unshare_newns')}"
    assert probe["unshare_zero"] == "EPERM", f"namespace-mutation denial (T2) — not cap-gated | unshare_zero={probe.get('unshare_zero')}"


def test_ss_e07_device_node_char_denied(probe):
    assert probe["mknod_char"] == "EPERM", f"device-node denial (T5); MKNOD cap kept, seccomp is the barrier | mknod_char={probe.get('mknod_char')}"


def test_ss_e08_device_node_fifo_allowed(probe):
    assert probe["mknod_fifo"] == "OK", f"mode-filter allows non-device nodes (pkg postinst) | mknod_fifo={probe.get('mknod_fifo')}"


def test_ss_e09_kernel_surface_denied(probe):
    assert probe["bpf"] == "EPERM", f"kernel-surface denial (T4) — not cap-gated | bpf={probe.get('bpf')}"
    assert probe["io_uring"] == "EPERM", f"kernel-surface denial (T4) — not cap-gated | io_uring={probe.get('io_uring')}"


def test_ss_e10_clone3_forced_enosys(probe):
    assert probe["clone3"] == "ENOSYS", f"forces glibc/musl clone(2) fallback the flag-mask inspects | clone3={probe.get('clone3')}"
