"""Fixtures for the shell-exec security live e2e suite.

Every sandbox is driven only through the two purpose-built CLIs: lifecycle
(create/destroy) via ``manager.management.helpers`` → ``sandbox-manager-cli``, and
commands/files via ``core.cli.runtime`` → ``sandbox-runtime-cli``. The static
musl probe (``helpers.PROBE_SOURCE``) is compiled once per session and copied
into each sandbox's bind-mounted workspace.

Read-only cases share a module-scoped sandbox and a single ``probe`` run; cases
that mutate container state, install packages, or assert scoping isolation take a
dedicated ``fresh_sandbox``.
"""

import shutil
import stat

import pytest

from core.config import IMAGE
from manager.management import helpers as mgmt
from runtime.shell_security.helpers import compile_probe, run_probe


@pytest.fixture(scope="session")
def probe_binary(tmp_path_factory):
    build = tmp_path_factory.mktemp("shell_security_build")
    return compile_probe(build)


def _provision_workspace(probe_binary, workspace):
    workspace.mkdir(parents=True, exist_ok=True)
    dest = workspace / probe_binary.name
    shutil.copy2(probe_binary, dest)
    dest.chmod(dest.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return workspace


def _create_sandbox(workspace):
    created = mgmt.create_sandbox(image=IMAGE, workspace_root=str(workspace))
    sandbox_id = created.get("id")
    assert sandbox_id, f"create_sandbox failed: {created}"
    return sandbox_id


@pytest.fixture(scope="module")
def module_sandbox(probe_binary, tmp_path_factory):
    workspace = _provision_workspace(
        probe_binary, tmp_path_factory.mktemp("module_ws") / "workspace"
    )
    sandbox_id = _create_sandbox(workspace)
    try:
        yield sandbox_id
    finally:
        mgmt.destroy_sandbox(sandbox_id)


@pytest.fixture(scope="module")
def probe(module_sandbox):
    return run_probe(module_sandbox)


@pytest.fixture
def fresh_sandbox(probe_binary, tmp_path):
    workspace = _provision_workspace(probe_binary, tmp_path / "workspace")
    sandbox_id = _create_sandbox(workspace)
    try:
        yield sandbox_id
    finally:
        mgmt.destroy_sandbox(sandbox_id)
