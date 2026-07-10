"""Suite customization knobs and resolved paths.

Every value is overridable from the environment, e.g.::

    E2E_IMAGE=debian:12 E2E_WORKSPACE_ROOT=/work pytest manager
"""

import os

from .root import REPO_ROOT

SUITE_DIR = REPO_ROOT / "e2e"
BIN_DIR = REPO_ROOT / "bin"

SANDBOX_MANAGER_CLI = BIN_DIR / "sandbox-manager-cli"
SANDBOX_RUNTIME_CLI = BIN_DIR / "sandbox-runtime-cli"
SANDBOX_OBSERVABILITY_CLI = BIN_DIR / "sandbox-observability-cli"
START_GATEWAY = BIN_DIR / "start-sandbox-docker-gateway"

# Docker image used for every sandbox (manager create_sandbox --image).
IMAGE = os.environ.get("E2E_IMAGE", "ubuntu:24.04")

# Workspace-root variants. repo/ holds one subfolder per variant (e.g. testbed,
# special_case_b); each is a HOST directory the Docker backend bind-mounts into
# the sandbox as its workspace root (--workspace-root is a host path).
REPO_DIR = SUITE_DIR / "repo"
WORKSPACE_VARIANT = os.environ.get("E2E_WORKSPACE_VARIANT", "testbed")


def workspace_variant(name=None):
    """Absolute host path of a workspace variant under repo/."""
    return str(REPO_DIR / (name or WORKSPACE_VARIANT))


# Default workspace root = the selected variant. Override with E2E_WORKSPACE_ROOT
# to point at any absolute host directory directly.
WORKSPACE_ROOT = os.environ.get("E2E_WORKSPACE_ROOT", workspace_variant())

# Daemon/sandbox config YAML used by the gateway start script.
CONFIG_YAML = os.environ.get(
    "SANDBOX_GATEWAY_CONFIG_YAML", str(REPO_ROOT / "config" / "prd.yml")
)

# "1" -> cold-start the gateway with --rebuild-binary (the documented path).
REBUILD_BINARY = os.environ.get("E2E_REBUILD_BINARY", "1")

# "1" -> pass the manager CLI's global --progress flag and stream daemon-side
# progress lines (e.g. workspace base copy/hash) live. Off by default. Runtime
# and observability operations have no --progress flag, so they never stream.
PROGRESS = os.environ.get("E2E_PROGRESS", "0") == "1"
