"""Orchestrator-side host package for sandbox operations.

- :mod:`sandbox.host.runtime_bundle` — build and upload the daemon bundle.
- :mod:`sandbox.host.daemon_client` — client for the bundled in-sandbox daemon.
- :mod:`sandbox.host.setup`, :mod:`sandbox.host.recovery`, :mod:`sandbox.host.git`,
  and :mod:`sandbox.host.context` — host-side operations against a sandbox.

Layer rule: host modules may import provider registry surfaces and foundation
modules, but not the public ``sandbox.api`` facade.
"""

from __future__ import annotations

__all__: list[str] = []
