"""Process-local advisory writer locks for layer-stack storage roots."""

from __future__ import annotations

import fcntl
import os
import threading
import weakref
from contextlib import AbstractContextManager
from dataclasses import dataclass
from pathlib import Path

_STORAGE_WRITER_LOCK_FILE = ".storage-writer.lock"
_STORAGE_WRITER_LOCKS: dict[str, _StorageWriterLock] = {}
_STORAGE_WRITER_LOCKS_LOCK = threading.Lock()


@dataclass
class _StorageWriterLock:
    fd: int
    refcount: int
    mutex: threading.RLock


class StorageWriterLockLease:
    def __init__(self, key: str, mutex: threading.RLock) -> None:
        self._mutex = mutex
        # weakref.finalize avoids the __del__ trap of running during
        # interpreter teardown after module globals (the dict and its
        # lock) have been cleared.
        self._finalizer = weakref.finalize(self, _release_storage_lock, key)

    def exclusive(self) -> AbstractContextManager[object]:
        """Return the process-local write mutex for this storage root.

        The fcntl lease prevents another daemon process from owning the same
        root. The mutex serializes multiple in-process LayerStack managers that
        may coexist after cache drops or overlay lifecycle resets.
        """
        return self._mutex

    def close(self) -> None:
        self._finalizer()


def _release_storage_lock(key: str) -> None:
    with _STORAGE_WRITER_LOCKS_LOCK:
        record = _STORAGE_WRITER_LOCKS.get(key)
        if record is None:
            return
        record.refcount -= 1
        if record.refcount > 0:
            return
        _STORAGE_WRITER_LOCKS.pop(key, None)
        fcntl.flock(record.fd, fcntl.LOCK_UN)
        os.close(record.fd)


def acquire_storage_writer_lock(storage_root: Path) -> StorageWriterLockLease:
    """Hold a process-wide advisory writer lock for this storage root."""
    key = str(storage_root.resolve())
    with _STORAGE_WRITER_LOCKS_LOCK:
        record = _STORAGE_WRITER_LOCKS.get(key)
        if record is not None:
            record.refcount += 1
            return StorageWriterLockLease(key, record.mutex)

        lock_path = storage_root / _STORAGE_WRITER_LOCK_FILE
        fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o644)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            os.close(fd)
            raise RuntimeError(
                "layer-stack storage root is already owned by another process: "
                f"{storage_root}"
            ) from exc
        mutex = threading.RLock()
        _STORAGE_WRITER_LOCKS[key] = _StorageWriterLock(
            fd=fd,
            refcount=1,
            mutex=mutex,
        )
        return StorageWriterLockLease(key, mutex)


__all__ = ["StorageWriterLockLease", "acquire_storage_writer_lock"]
