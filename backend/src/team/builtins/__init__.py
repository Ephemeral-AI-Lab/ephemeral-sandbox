"""Builtin team-mode agent and team definitions.

Definitions live as Markdown+YAML-frontmatter files in
``backend/src/prompt/agents/`` and ``backend/config/teams/``.
``register_all()`` loads them at boot, optionally seeds the database,
and populates the in-memory registries.
"""

from __future__ import annotations

import logging
from pathlib import Path
from typing import TYPE_CHECKING

from agents.loader import load_agents_dir
from agents.registry import register_definition
from agents.types import AgentDefinition
from team.models import TeamDefinition

# Side-effect import: registers per-agent run_subagent dispatch validators
# at module load so the validator is available regardless of whether
# ``register_all()`` is ultimately called (tests and some boot paths may
# skip the call when builtins are already seeded).
from team import scout_dispatch as _scout_dispatch  # noqa: F401

if TYPE_CHECKING:
    from agents.db.store import AgentDefinitionStore
    from team.persistence.store import TeamDefinitionStore

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Canonical name constants — imported across the codebase for dispatch logic.
# ---------------------------------------------------------------------------
ROOT_PLANNER = "root_planner"
TEAM_PLANNER = "team_planner"
DEVELOPER = "developer"
VALIDATOR = "validator"
SCOUT = "scout"
TEAM_REPLANNER = "team_replanner"
PARENT_SUMMARIZER = "parent_summarizer"

_BACKEND_SRC = Path(__file__).resolve().parents[2]
_CONFIG_ROOT = Path(__file__).resolve().parents[3] / "config"
_BUILTINS_DIR = _BACKEND_SRC / "prompt" / "agents"
_TEAMS_BUILTIN_DIR = _CONFIG_ROOT / "teams"

# Expected number of builtin agents.  If a seed file fails to parse,
# ``load_agents_dir`` silently skips it — this constant lets us detect
# that early rather than discovering a missing agent at dispatch time.
_EXPECTED_BUILTIN_COUNT = 7
_EXPECTED_BUILTIN_TEAM_COUNT = 1


def _load_builtin_definitions() -> list[AgentDefinition]:
    """Load all builtin agent definitions from the seed files."""
    defs = load_agents_dir(_BUILTINS_DIR)
    # Override source to "builtin" — load_agents_dir defaults to "user".
    for d in defs:
        d.source = "builtin"
    if len(defs) != _EXPECTED_BUILTIN_COUNT:
        logger.error(
            "Expected %d builtin agents but loaded %d from %s — "
            "check seed files for parse errors",
            _EXPECTED_BUILTIN_COUNT,
            len(defs),
            _BUILTINS_DIR,
        )
    return defs


def _load_builtin_team_definitions() -> list[TeamDefinition]:
    """Load all builtin team definitions from the seed files."""
    from team.loader import load_teams_dir

    defs = load_teams_dir(_TEAMS_BUILTIN_DIR)
    if len(defs) != _EXPECTED_BUILTIN_TEAM_COUNT:
        logger.error(
            "Expected %d builtin teams but loaded %d from %s — "
            "check seed files for parse errors",
            _EXPECTED_BUILTIN_TEAM_COUNT,
            len(defs),
            _TEAMS_BUILTIN_DIR,
        )
    return defs


def register_all(
    *,
    store: "AgentDefinitionStore | None" = None,
    team_store: "TeamDefinitionStore | None" = None,
) -> None:
    """Register all builtin team agents and team definitions.

    When *store* / *team_store* are provided, each definition is seeded
    into the database first (skipped if already present), then loaded
    from DB into the in-memory registry.  This lets users customise
    builtins via the DB while keeping a code-level fallback for
    environments without a DB.
    """
    from team.registry import register_team_definition

    # --- agent definitions ---
    defaults = _load_builtin_definitions()

    if store is not None:
        from agents.builder.service import AgentBuilderService

        for defn in defaults:
            record = store.seed_builtin(defn)
            loaded = AgentBuilderService.record_to_definition(record)
            register_definition(loaded)
    else:
        for defn in defaults:
            register_definition(defn)

    # --- team definitions ---
    team_defaults = _load_builtin_team_definitions()

    if team_store is not None:
        for tdefn in team_defaults:
            live = team_store.seed_builtin(tdefn)
            register_team_definition(live)
    else:
        for tdefn in team_defaults:
            register_team_definition(tdefn)

    logger.info(
        "team builtins registered (%d agents, %d teams, db=%s)",
        len(defaults),
        len(team_defaults),
        store is not None,
    )
