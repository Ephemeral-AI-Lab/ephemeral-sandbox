"""Skill definition persistence store."""

from __future__ import annotations

import logging
from datetime import datetime, timezone
from uuid import uuid4

from sqlalchemy.orm import Session, sessionmaker

from ephemeralos.skills.db.model import SkillDefinitionRecord

logger = logging.getLogger(__name__)


class SkillDefinitionStore:
    """CRUD operations for skill definition records."""

    def __init__(self) -> None:
        self._session_factory: sessionmaker[Session] | None = None

    def initialize(self, session_factory: sessionmaker[Session]) -> None:
        self._session_factory = session_factory
        logger.info("SkillDefinitionStore initialised")

    @property
    def _sf(self) -> sessionmaker[Session]:
        assert self._session_factory is not None, "SkillDefinitionStore not initialised"
        return self._session_factory

    def create(self, record: SkillDefinitionRecord) -> SkillDefinitionRecord:
        with self._sf() as db:
            db.add(record)
            db.commit()
            db.refresh(record)
            return record

    def get_by_name(self, name: str) -> SkillDefinitionRecord | None:
        with self._sf() as db:
            return (
                db.query(SkillDefinitionRecord)
                .filter(SkillDefinitionRecord.name == name, SkillDefinitionRecord.is_active.is_(True))
                .first()
            )

    def list_active(self, *, limit: int = 200, offset: int = 0) -> list[SkillDefinitionRecord]:
        with self._sf() as db:
            return list(
                db.query(SkillDefinitionRecord)
                .filter(SkillDefinitionRecord.is_active.is_(True))
                .order_by(SkillDefinitionRecord.name)
                .offset(offset)
                .limit(limit)
                .all()
            )

    def update(self, name: str, updates: dict) -> SkillDefinitionRecord:
        with self._sf() as db:
            record = (
                db.query(SkillDefinitionRecord)
                .filter(SkillDefinitionRecord.name == name, SkillDefinitionRecord.is_active.is_(True))
                .first()
            )
            if record is None:
                raise KeyError(f"Skill '{name}' not found")
            for key, value in updates.items():
                if hasattr(record, key) and key not in ("id", "name", "created_at", "version"):
                    setattr(record, key, value)
            record.version += 1
            record.updated_at = datetime.now(timezone.utc)
            db.commit()
            db.refresh(record)
            return record

    def soft_delete(self, name: str) -> bool:
        with self._sf() as db:
            record = (
                db.query(SkillDefinitionRecord)
                .filter(SkillDefinitionRecord.name == name, SkillDefinitionRecord.is_active.is_(True))
                .first()
            )
            if record is None:
                return False
            record.is_active = False
            record.updated_at = datetime.now(timezone.utc)
            db.commit()
            return True
