"""Compatibility shim for ``python -m sandbox.overlay.cli``."""

from __future__ import annotations

from sandbox.overlay.worker import main

__all__ = ["main"]


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
