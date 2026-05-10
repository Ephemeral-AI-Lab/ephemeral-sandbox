"""Fixture definitions for the complex_project_build scenario.

Each fixture is a stdlib-only Python file written under ``/ephemeral-os`` by
the scenario probe. Files are stored here as final-form text plus a paired
``(skeleton, patches)`` representation so the probe can drive the OCC stack
through ``write_file`` (skeleton seed) and many ``edit_file`` calls (patches),
matching the §6 phase plan and §13.6 edit-bias rule.

The host-side test ``test_complex_project_build_fixtures`` re-applies
``patches`` to ``skeleton`` and checks byte-equality with ``final``; that
guards against fixture drift over time.
"""

from __future__ import annotations

from live_e2e.scenarios.sandbox._fixtures.scheduler_demo_data import (
    FixtureFile,
    Patch,
    SCHEDULER_DEMO_FILES,
    SMOKE_FILE_PATHS,
)

__all__ = [
    "FixtureFile",
    "Patch",
    "SCHEDULER_DEMO_FILES",
    "SMOKE_FILE_PATHS",
]
