"""Facade for OCC merge engines used by runtime probes and diagnostics."""

from __future__ import annotations

from sandbox.occ.direct.merge import DirectMerge
from sandbox.occ.gated.merge import GatedMerge


__all__ = ["DirectMerge", "GatedMerge"]
