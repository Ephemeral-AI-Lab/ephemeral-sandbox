"""Agent definition persistence store."""

from __future__ import annotations

import logging
from datetime import datetime, timezone
from uuid import uuid4

from sqlalchemy.orm import Session, sessionmaker

from ephemeralos.agents.db.model import AgentDefinitionRecord

logger = logging.getLogger(__name__)


class AgentDefinitionStore:
    """CRUD operations for agent definition records."""

    def __init__(self) -> None:
        self._session_factory: sessionmaker[Session] | None = None

    def initialize(self, session_factory: sessionmaker[Session]) -> None:
        self._session_factory = session_factory
        logger.info("AgentDefinitionStore initialised")

    @property
    def _sf(self) -> sessionmaker[Session]:
        assert self._session_factory is not None, "AgentDefinitionStore not initialised"
        return self._session_factory

    def create(self, record: AgentDefinitionRecord) -> AgentDefinitionRecord:
        with self._sf() as db:
            db.add(record)
            db.commit()
            db.refresh(record)
            return record

    def get_by_name(self, name: str) -> AgentDefinitionRecord | None:
        with self._sf() as db:
            return (
                db.query(AgentDefinitionRecord)
                .filter(AgentDefinitionRecord.name == name, AgentDefinitionRecord.is_active.is_(True))
                .first()
            )

    def get_by_id(self, record_id: str) -> AgentDefinitionRecord | None:
        with self._sf() as db:
            return db.get(AgentDefinitionRecord, record_id)

    def list_active(self, *, tags: list[str] | None = None, limit: int = 50, offset: int = 0) -> list[AgentDefinitionRecord]:
        with self._sf() as db:
            q = db.query(AgentDefinitionRecord).filter(AgentDefinitionRecord.is_active.is_(True)).order_by(AgentDefinitionRecord.name)
            if tags:
                for tag in tags:
                    q = q.filter(AgentDefinitionRecord.tags.contains([tag]))
            return list(q.offset(offset).limit(limit).all())

    def update(self, name: str, updates: dict) -> AgentDefinitionRecord:
        with self._sf() as db:
            record = db.query(AgentDefinitionRecord).filter(AgentDefinitionRecord.name == name, AgentDefinitionRecord.is_active.is_(True)).first()
            if record is None:
                raise KeyError(f"Agent definition '{name}' not found")
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
            record = db.query(AgentDefinitionRecord).filter(AgentDefinitionRecord.name == name, AgentDefinitionRecord.is_active.is_(True)).first()
            if record is None:
                return False
            record.is_active = False
            record.updated_at = datetime.now(timezone.utc)
            db.commit()
            return True

    def hard_delete(self, name: str) -> bool:
        with self._sf() as db:
            record = db.query(AgentDefinitionRecord).filter(AgentDefinitionRecord.name == name).first()
            if record is None:
                return False
            db.delete(record)
            db.commit()
            return True

    def clone(self, source_name: str, new_name: str) -> AgentDefinitionRecord:
        with self._sf() as db:
            source = db.query(AgentDefinitionRecord).filter(AgentDefinitionRecord.name == source_name, AgentDefinitionRecord.is_active.is_(True)).first()
            if source is None:
                raise KeyError(f"Source agent '{source_name}' not found")
            now = datetime.now(timezone.utc)
            clone_record = AgentDefinitionRecord(
                id=str(uuid4()), name=new_name, description=source.description,
                system_prompt=source.system_prompt, model=source.model, effort=source.effort,
                max_turns=source.max_turns,
                toolkits=source.toolkits, skills=source.skills or [],
                hooks=source.hooks,
                background=source.background,
                initial_prompt=source.initial_prompt, subagent_type=new_name,
                version=1, is_active=True, created_by=source.created_by,
                tags=source.tags, metadata_json=source.metadata_json,
                created_at=now, updated_at=now,
            )
            db.add(clone_record)
            db.commit()
            db.refresh(clone_record)
            return clone_record
