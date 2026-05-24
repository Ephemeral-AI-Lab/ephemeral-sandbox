"""Plugin adapter — host-side ``call_plugin`` and in-sandbox op registry.

This package is the *only* sandbox-side surface plugin authors are allowed to
import (per ``docs/architecture/plugins-refactor.md`` §2). It must remain
plugin-agnostic — no LSP-specific or language-specific code, no plugin-name
string switches.

Principle 10 boundary: plugins execute only through the ephemeral workspace
pipeline. Isolated workspaces keep their separate RPC surface and do not load
this plugin surface.
"""

from __future__ import annotations

from typing import Any

__all__ = ["call_plugin"]


def __getattr__(name: str) -> Any:
    if name == "call_plugin":
        from sandbox.ephemeral_workspace.plugin.session import call_plugin

        return call_plugin
    raise AttributeError(name)
