"""Seed SuperCocoa specialist definitions into the database.

Reads JSON specialist files from the SuperCocoa agents directory and
persists them as ``AgentDefinitionRecord`` rows.  The operation is
idempotent — agents that already exist (by name) are skipped.
"""

from __future__ import annotations

import json
import logging
from pathlib import Path
from uuid import uuid4

from ephemeralos.agents.db.model import AgentDefinitionRecord
from ephemeralos.agents.db.store import AgentDefinitionStore

logger = logging.getLogger(__name__)


def seed_specialists_from_supercocoa(
    store: AgentDefinitionStore,
    source_dir: Path,
) -> tuple[int, int]:
    """Import specialist JSON files into the agent_definitions table.

    Parameters
    ----------
    store:
        Initialised ``AgentDefinitionStore`` instance.
    source_dir:
        Path to the directory containing ``*.json`` specialist files
        (e.g. ``synthetic-os/.super-cocoa-agents/specialist/``).

    Returns
    -------
    tuple[int, int]
        ``(created, skipped)`` counts.
    """
    created, skipped = 0, 0

    for json_path in sorted(source_dir.glob("*.json")):
        try:
            data = json.loads(json_path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError) as exc:
            logger.warning("Skipping %s: %s", json_path.name, exc)
            continue

        name: str = data.get("name", "")
        if not name:
            logger.warning("Skipping %s: missing 'name' field", json_path.name)
            continue

        # Idempotent — skip if already seeded (check inactive too to avoid unique constraint violation)
        if store.get_by_name(name, active_only=False) is not None:
            skipped += 1
            continue

        # Build merged metadata (original metadata + response_format)
        metadata: dict = dict(data.get("metadata") or {})
        if data.get("response_format"):
            metadata["response_format"] = data["response_format"]

        # Join instructions array into a single system prompt
        instructions = data.get("instructions", [])
        system_prompt = "\n\n".join(instructions) if instructions else None

        record = AgentDefinitionRecord(
            id=str(uuid4()),
            name=name,
            description=data.get("description", ""),
            system_prompt=system_prompt,
            model=data.get("model_key"),
            toolkits=data.get("toolkits", []),
            skills=data.get("skills", []),
            subagent_type=name,
            tags=["supercocoa", "specialist"],
            metadata_json=metadata or None,
            created_by="supercocoa-migration",
        )
        store.create(record)
        logger.debug("Seeded specialist %r from %s", name, json_path.name)
        created += 1

    return created, skipped
