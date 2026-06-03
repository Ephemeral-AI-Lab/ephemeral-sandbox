"""Host-owned in-sandbox runtime paths for the Rust sandbox daemon."""

from __future__ import annotations

EOS_REMOTE_ROOT = "/eos"
"""Single in-sandbox root for EphemeralOS-managed runtime state."""

BUNDLE_REMOTE_DIR = f"{EOS_REMOTE_ROOT}/daemon"
"""Remote directory where host-uploaded Rust daemon payloads live."""

LAYER_STACK_REMOTE_DIR = f"{EOS_REMOTE_ROOT}/layer-stack"
"""Remote directory for overlay-compatible LayerStack storage."""

SCRATCH_REMOTE_DIR = f"{EOS_REMOTE_ROOT}/mount"
"""Remote directory for overlay upper/work and transient command state."""

BUNDLE_HASH_MARKER = f"{BUNDLE_REMOTE_DIR}/.bundle-hash"
BUNDLE_REMOTE_TARBALL = f"{BUNDLE_REMOTE_DIR}/bundle.tar.gz"

DAEMON_SOCKET_PATH = f"{BUNDLE_REMOTE_DIR}/runtime.sock"
DAEMON_PID_PATH = f"{BUNDLE_REMOTE_DIR}/runtime.pid"
DAEMON_LOG_PATH = f"{BUNDLE_REMOTE_DIR}/runtime.log"
DAEMON_ENV_SIGNATURE_PATH = f"{BUNDLE_REMOTE_DIR}/runtime.env"
DEFAULT_LAYER_STACK_ROOT = LAYER_STACK_REMOTE_DIR

EOSD_REMOTE_PATH = f"{BUNDLE_REMOTE_DIR}/eosd"
EOSD_SHA_MARKER = f"{BUNDLE_REMOTE_DIR}/.eosd-sha256"

__all__ = [
    "BUNDLE_HASH_MARKER",
    "BUNDLE_REMOTE_DIR",
    "BUNDLE_REMOTE_TARBALL",
    "DAEMON_ENV_SIGNATURE_PATH",
    "DAEMON_LOG_PATH",
    "DAEMON_PID_PATH",
    "DAEMON_SOCKET_PATH",
    "DEFAULT_LAYER_STACK_ROOT",
    "EOS_REMOTE_ROOT",
    "EOSD_REMOTE_PATH",
    "EOSD_SHA_MARKER",
    "LAYER_STACK_REMOTE_DIR",
    "SCRATCH_REMOTE_DIR",
]
