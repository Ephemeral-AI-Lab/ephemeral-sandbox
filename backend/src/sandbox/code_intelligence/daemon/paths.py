"""State-directory resolution and confinement guards for in-sandbox CI."""

from __future__ import annotations

import errno
import hashlib
import logging
import os
import pickle
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)


class StorageUnavailable(Exception):
    """Raised when ``$HOME/.cache/eos-ci/...`` cannot be created or written."""

    def __init__(self, errno: int, path: str, message: str) -> None:
        super().__init__(message)
        self.errno = errno
        self.path = path
        self.message = message


class StoragePathEscape(Exception):
    """Raised when a write target escapes the state-dir confinement."""


def workspace_root_hash(workspace_root: str) -> str:
    """Stable 16-hex digest of ``realpath(workspace_root)``."""
    real = os.path.realpath(workspace_root)
    return hashlib.sha256(real.encode("utf-8")).hexdigest()[:16]


def state_dir(workspace_root: str) -> Path:
    """Resolve ``$HOME/.cache/eos-ci/<workspace_hash>/v1/`` and create it."""
    home = Path(os.path.expanduser("~"))
    base = home / ".cache" / "eos-ci" / workspace_root_hash(workspace_root) / "v1"
    try:
        base.mkdir(parents=True, exist_ok=True)
    except PermissionError as exc:
        raise StorageUnavailable(
            errno=exc.errno or errno.EACCES,
            path=str(base),
            message=(
                f"Cannot create CI state dir at {base} (errno={exc.errno}); "
                f"running as user={os.getenv('USER')}, HOME={home}"
            ),
        ) from exc
    except OSError as exc:
        raise StorageUnavailable(
            errno=exc.errno or errno.EACCES,
            path=str(base),
            message=(
                f"Cannot create CI state dir at {base} "
                f"(errno={exc.errno}, {exc.strerror}); HOME={home}"
            ),
        ) from exc
    return base


def _confine(state: Path, name: str) -> Path:
    """Resolve ``name`` under ``state`` and reject traversal or symlink escapes."""
    state_real = state.resolve()
    target = (state / name).resolve()
    if target == state_real:
        raise StoragePathEscape(
            f"target {target} resolves to the state dir itself"
        )
    if state_real not in target.parents:
        raise StoragePathEscape(
            f"path {target} escapes state dir {state_real}"
        )
    return target


def _read_pickle_snapshot(state: Path, name: str) -> Any | None:
    """Load a one-shot legacy pickle snapshot, unlinking corrupt files."""
    target = _confine(state, name)
    if not target.exists():
        return None
    try:
        with open(target, "rb") as f:
            return pickle.load(f)
    except (EOFError, pickle.UnpicklingError, OSError) as exc:
        logger.warning(
            "storage: corrupt snapshot at %s (%s); unlinking",
            target,
            exc,
        )
        try:
            target.unlink()
        except OSError:
            pass
        return None
