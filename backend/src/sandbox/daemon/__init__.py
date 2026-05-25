"""In-sandbox daemon modules that ship inside the deployed sandbox bundle.

Strictly the bytes that execute INSIDE a sandbox: ``rpc/`` for the AF_UNIX
server and dispatcher, plus ``operation_handlers.py`` for built-in OP_TABLE
entries. Shared runtime dependencies live in sibling sandbox packages.
Host-side plumbing lives under :mod:`sandbox.host`.
"""

from __future__ import annotations

__all__: list[str] = []
