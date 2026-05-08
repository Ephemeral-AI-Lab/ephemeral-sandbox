"""Handler modules for daemon ``api.*`` ops.

Worker-level shell scaffolding (mount, capture, OCC apply) lives in
:mod:`sandbox.runtime.daemon.service.shell_runner`; the dispatcher registers it
directly because there is no shell-specific handler code left here.
"""

from . import edit, health, metrics, read, workspace, write

__all__ = ["edit", "health", "metrics", "read", "workspace", "write"]
