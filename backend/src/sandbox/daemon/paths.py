"""Shared in-sandbox EphemeralOS runtime paths."""

from __future__ import annotations

EOS_REMOTE_ROOT = "/eos"
"""Single in-sandbox root for EphemeralOS-managed runtime state."""

BUNDLE_REMOTE_DIR = f"{EOS_REMOTE_ROOT}/runtime"
"""Remote directory where the in-sandbox runtime bundle is extracted."""

LAYER_STACK_REMOTE_DIR = f"{EOS_REMOTE_ROOT}/layer-stack"
"""Remote directory for overlay-compatible LayerStack storage."""

SCRATCH_REMOTE_DIR = f"{EOS_REMOTE_ROOT}/scratch"
"""Remote directory for overlay upper/work and transient command state."""

BUNDLE_HASH_MARKER = f"{BUNDLE_REMOTE_DIR}/.bundle-hash"
BUNDLE_REMOTE_TARBALL = f"{BUNDLE_REMOTE_DIR}/bundle.tar.gz"

DAEMON_SOCKET_PATH = f"{BUNDLE_REMOTE_DIR}/runtime.sock"
DAEMON_PID_PATH = f"{BUNDLE_REMOTE_DIR}/runtime.pid"
DAEMON_LOG_PATH = f"{BUNDLE_REMOTE_DIR}/runtime.log"
DAEMON_ENV_SIGNATURE_PATH = f"{BUNDLE_REMOTE_DIR}/runtime.env"
DEFAULT_LAYER_STACK_ROOT = LAYER_STACK_REMOTE_DIR

RUNTIME_SCRIPT_DIR = f"{BUNDLE_REMOTE_DIR}/sandbox/daemon/scripts"
DAEMON_THIN_CLIENT_PATH = f"{RUNTIME_SCRIPT_DIR}/thin_client.py"
DAEMON_LAUNCH_SCRIPT_PATH = f"{RUNTIME_SCRIPT_DIR}/launch_daemon.sh"

__all__ = [
    "BUNDLE_HASH_MARKER",
    "BUNDLE_REMOTE_DIR",
    "BUNDLE_REMOTE_TARBALL",
    "DAEMON_ENV_SIGNATURE_PATH",
    "DAEMON_LAUNCH_SCRIPT_PATH",
    "DAEMON_LOG_PATH",
    "DAEMON_PID_PATH",
    "DAEMON_SOCKET_PATH",
    "DAEMON_THIN_CLIENT_PATH",
    "DEFAULT_LAYER_STACK_ROOT",
    "EOS_REMOTE_ROOT",
    "LAYER_STACK_REMOTE_DIR",
    "RUNTIME_SCRIPT_DIR",
    "SCRATCH_REMOTE_DIR",
]
