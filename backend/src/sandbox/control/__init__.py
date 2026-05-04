"""Orchestrator-side control plane for sandboxes.

- :mod:`sandbox.control.daemon` — host-side bundle build, install, and runtime
  command client (deploy + talk to the in-box runtime).
- :mod:`sandbox.control.ops` — host-side operations against a sandbox
  (setup sequencing, recovery, git, workspace, context).

Layer rule: ``control/ops`` may import ``control/daemon``; never the reverse.
"""

from __future__ import annotations

__all__: list[str] = []
