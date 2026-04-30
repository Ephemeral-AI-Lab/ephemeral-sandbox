"""Sandbox-side overlay shell runner facade.

The implementation lives in ``runtime/`` so each responsibility has a
focused module. This facade is kept as the stable import point. The
orchestrator deploys this file into the sandbox as ``overlay_run.py``.
"""

# ruff: noqa: E402

from __future__ import annotations

import sys

_SCRIPT_DIR = ""
if not __package__:  # Avoid sibling types.py shadowing the stdlib types module.
    _SCRIPT_DIR = sys.path[0]
    sys.path = [path for path in sys.path if path != _SCRIPT_DIR]

import importlib
import importlib.util
import os
from typing import Any

if __package__:
    _runtime: Any = importlib.import_module(".runtime", __package__)
else:  # pragma: no cover - exercised when uploaded and run as a script
    _runtime_dir = os.path.join(_SCRIPT_DIR, "overlay_runtime")
    if not os.path.isdir(_runtime_dir):
        _runtime_dir = os.path.join(_SCRIPT_DIR, "runtime")
    _spec = importlib.util.spec_from_file_location(
        "overlay_runtime",
        os.path.join(_runtime_dir, "__init__.py"),
        submodule_search_locations=[_runtime_dir],
    )
    if _spec is None or _spec.loader is None:
        raise RuntimeError(f"failed to load overlay runtime from {_runtime_dir}")
    _runtime = importlib.util.module_from_spec(_spec)
    sys.modules["overlay_runtime"] = _runtime
    _spec.loader.exec_module(_runtime)

__all__ = list(_runtime.__all__)
globals().update({name: getattr(_runtime, name) for name in __all__})


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(_runtime.main())
