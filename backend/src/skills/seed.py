"""Seed bundled skills into the database.

Reads skill definitions from the bundled content directory and persists
them as ``SkillDefinitionRecord`` rows.  Idempotent — existing skills
are skipped.
"""

from __future__ import annotations

import logging
from uuid import uuid4

from ephemeralos.skills.bundled import get_bundled_skills
from ephemeralos.skills.db.model import SkillDefinitionRecord
from ephemeralos.skills.db.store import SkillDefinitionStore

logger = logging.getLogger(__name__)


def seed_bundled_skills(store: SkillDefinitionStore) -> tuple[int, int]:
    """Persist bundled skills into the skill_definitions table.

    Returns
    -------
    tuple[int, int]
        ``(created, skipped)`` counts.
    """
    created, skipped = 0, 0

    for skill in get_bundled_skills():
        if store.get_by_name(skill.name) is not None:
            skipped += 1
            continue

        record = SkillDefinitionRecord(
            id=str(uuid4()),
            name=skill.name,
            description=skill.description,
            content=skill.content,
            source="bundled",
        )
        store.create(record)
        logger.debug("Seeded skill %r", skill.name)
        created += 1

    return created, skipped
