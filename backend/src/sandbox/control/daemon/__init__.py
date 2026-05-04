"""Host-side daemon plumbing: deploy and talk to the in-sandbox runtime.

- :mod:`sandbox.control.daemon.bundle` — build the runtime bundle and upload
  it via chunked exec.
- :mod:`sandbox.control.daemon.install` — register and run bundled peer setup
  scripts.
- :mod:`sandbox.control.daemon.command` — client of the bundled runtime
  dispatcher.
"""

from __future__ import annotations

__all__: list[str] = []
