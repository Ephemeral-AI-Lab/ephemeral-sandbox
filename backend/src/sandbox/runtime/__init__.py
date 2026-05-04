"""In-sandbox dispatcher modules that ship inside the runtime bundle.

Strictly the bytes that execute INSIDE a sandbox: ``server.py`` (the JSON
dispatcher) and ``overlay_shell/`` (overlay-aware shell pipeline). Host-side
plumbing lives under :mod:`sandbox.control.daemon` (bundle build/upload, peer
install, command client) and :mod:`sandbox.control.ops` (operations against a
sandbox).

``async_bridge.py`` remains here pending a separate refactor that lifts
``run_sync`` into a neutral host-side location.
"""

from __future__ import annotations

__all__: list[str] = []
