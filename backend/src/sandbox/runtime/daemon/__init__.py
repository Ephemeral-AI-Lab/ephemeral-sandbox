"""In-sandbox daemon modules that ship inside the deployed sandbox bundle.

Strictly the bytes that execute INSIDE a sandbox: ``rpc/`` for the AF_UNIX
server and dispatcher, ``handler/`` for OP_TABLE entries, and ``service/`` for
in-process dependencies. Host-side plumbing lives under :mod:`sandbox.host`.
"""

from __future__ import annotations

__all__: list[str] = []
