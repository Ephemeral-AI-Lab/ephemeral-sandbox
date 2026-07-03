import os

import pytest

from core.cli import is_error
from core.config import IMAGE
from manager.management import helpers as mgmt
from runtime.command_security.helpers import (
    ALLOWED_SYSCALLS,
    CAP_CHOWN,
    CAP_DAC_OVERRIDE,
    CAP_FOWNER,
    CAP_NET_ADMIN,
    CAP_SETFCAP,
    CAP_SYS_ADMIN,
    CAP_SYS_MODULE,
    DENIED_SYSCALLS,
    compile_probe,
    exec_cmd,
    has_cap,
    run_probe,
    wait_command,
)

COMMAND_SECURITY_MODE = os.environ.get("E2E_COMMAND_SECURITY_MODE", "enforce")
assert COMMAND_SECURITY_MODE in {"enforce", "relaxed", "off"}


@pytest.fixture
def command_security_sandbox(tmp_path):
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    compile_probe(workspace)
    created = mgmt.create_sandbox(image=IMAGE, workspace_root=str(workspace))
    sandbox_id = created.get("id")
    assert sandbox_id, f"create_sandbox failed: {created}"
    try:
        yield sandbox_id
    finally:
        mgmt.destroy_sandbox(sandbox_id)


@pytest.fixture
def probe(command_security_sandbox):
    return run_probe(command_security_sandbox)


@pytest.mark.smoke
def test_cs01_allowed_commands_still_work(command_security_sandbox):
    for command in (
        "id -u",
        "uname -m",
        "sh -lc 'printf ok > cs01.txt && cat cs01.txt'",
    ):
        result = exec_cmd(command_security_sandbox, command)
        assert not is_error(result), result
        assert result.get("status") == "ok", result


@pytest.mark.smoke
def test_cs02_dangerous_syscalls_are_denied(probe):
    if COMMAND_SECURITY_MODE == "off":
        assert probe["unshare_zero"] == "OK", probe
        return

    for name in DENIED_SYSCALLS:
        if COMMAND_SECURITY_MODE == "relaxed" and name == "unshare_zero":
            assert probe[name] == "OK", {name: probe[name], "probe": probe}
        elif COMMAND_SECURITY_MODE == "relaxed" and name == "unshare_newns":
            assert probe[name] in ("OK", "EPERM"), {name: probe[name], "probe": probe}
        else:
            assert probe[name] == "EPERM", {name: probe[name], "probe": probe}

    if COMMAND_SECURITY_MODE != "off":
        assert probe["clone3"] == "ENOSYS", probe


@pytest.mark.smoke
def test_cs03_usability_refinements_remain_allowed(probe):
    for name in ALLOWED_SYSCALLS:
        if name == "fchmodat2":
            assert probe[name] in ("OK", "ENOSYS"), {name: probe[name], "probe": probe}
            continue
        assert probe[name] == "OK", {name: probe[name], "probe": probe}


@pytest.mark.smoke
def test_cs04_status_and_capability_state(probe):
    assert probe["nnp"] == "1", probe
    if COMMAND_SECURITY_MODE == "off":
        assert probe["seccomp"] == "0", probe
    else:
        assert probe["seccomp"] == "2", probe

    for cap in (CAP_CHOWN, CAP_DAC_OVERRIDE, CAP_FOWNER, CAP_SETFCAP):
        assert has_cap(probe["capeff"], cap), probe
    for cap in (CAP_SYS_ADMIN, CAP_NET_ADMIN, CAP_SYS_MODULE):
        assert not has_cap(probe["capeff"], cap), probe
    assert not has_cap(probe["capbnd"], CAP_SYS_ADMIN), probe


@pytest.mark.medium
def test_cs05_image_tools_are_denied(command_security_sandbox):
    unshare_probe = exec_cmd(
        command_security_sandbox,
        "sh -lc 'command -v unshare >/dev/null || exit 77; unshare -m true'",
    )
    if unshare_probe.get("status") == "error" and unshare_probe.get("exit_code") == 77:
        pytest.skip("image does not include unshare")
    assert unshare_probe.get("status") != "ok", unshare_probe

    mount_probe = exec_cmd(
        command_security_sandbox,
        "sh -lc 'mkdir -p /tmp/eos-cs-tool-mount && mount -t tmpfs none /tmp/eos-cs-tool-mount'",
    )
    assert mount_probe.get("status") != "ok", mount_probe

    umount_probe = exec_cmd(command_security_sandbox, "umount /workspace")
    assert umount_probe.get("status") != "ok", umount_probe


@pytest.mark.medium
def test_cs06_package_manager_still_starts(command_security_sandbox):
    version = exec_cmd(command_security_sandbox, "apt-get --version")
    assert version.get("status") == "ok", version

    archive_sources = (
        'printf "'
        "Types: deb\\n"
        "URIs: http://archive.ubuntu.com/ubuntu/\\n"
        "Suites: noble noble-updates noble-backports\\n"
        "Components: main universe restricted multiverse\\n"
        "Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\\n"
        '" > /etc/apt/sources.list.d/ubuntu.sources'
    )
    apt_options = (
        "-o APT::Sandbox::User=root "
        "-o Dir::Cache::archives=/tmp/eos-apt-archives"
    )
    package_command = (
        "sh -lc '"
        f"{archive_sources} && "
        "mkdir -p /tmp/eos-apt-archives/partial && "
        "chmod 0700 /tmp/eos-apt-archives/partial && "
        f"apt-get {apt_options} update && "
        f"apt-get {apt_options} install -y --no-install-recommends hello && "
        "hello >/tmp/eos-cs-hello.out'"
    )
    install = exec_cmd(
        command_security_sandbox,
        package_command,
        yield_ms=0,
        timeout_ms=180_000,
        timeout=60,
    )
    if install.get("status") == "running":
        command_session_id = install.get("command_session_id")
        assert command_session_id, install
        install = wait_command(command_security_sandbox, command_session_id, timeout_s=180)
    if install.get("status") != "ok":
        output = repr(install).lower()
        for marker in ("operation not permitted", "setgroups", "seccomp", "capability"):
            assert marker not in output, install
        pytest.skip(f"apt install unavailable in this environment: {install}")
