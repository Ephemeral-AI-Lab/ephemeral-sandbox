"""Gateway lifecycle: reuse a running gateway, else start it via the sh script."""

import logging
import subprocess
import time

from . import cli as climod
from .config import REBUILD_BINARY, REPO_ROOT, START_GATEWAY

_log = logging.getLogger("e2e.gateway")


def _responding():
    try:
        return not climod.is_error(climod.manager("list_sandboxes", timeout=15))
    except (climod.CliError, subprocess.SubprocessError):
        return False


def ensure_up(timeout=180):
    """Ensure a gateway is answering; start one if needed (idempotent).

    A cold start runs ``bin/start-sandbox-docker-gateway`` (with
    ``--rebuild-binary`` when ``E2E_REBUILD_BINARY=1``), which daemonizes the
    gateway in the background. Warm runs reuse the existing gateway.
    """
    if _responding():
        _log.info("gateway already running — reusing")
        return
    cmd = [str(START_GATEWAY)]
    if REBUILD_BINARY == "1":
        cmd.append("--rebuild-binary")
    _log.info("starting gateway (rebuild=%s); run pytest with -s to stream build output", REBUILD_BINARY)
    started = time.monotonic()
    subprocess.run(cmd, cwd=str(REPO_ROOT), check=True)

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if _responding():
            _log.info("gateway ready (%.1fs)", time.monotonic() - started)
            return
        time.sleep(1)
    raise RuntimeError("gateway did not become ready")
