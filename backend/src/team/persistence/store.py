"""Team definition CRUD store."""

from __future__ import annotations

import logging
from uuid import uuid4

from sqlalchemy.orm import Session, sessionmaker

from team.models import TeamDefinition
from team.persistence.model import TeamDefinitionRecord

logger = logging.getLogger(__name__)


class TeamDefinitionStore:
    """CRUD operations for team definition records.

    Initialised once by the application factory with a SQLAlchemy session
    factory. Tests use an in-memory SQLite session factory via the same
    ``initialize()`` contract.
    """

    def __init__(self) -> None:
        self._session_factory: sessionmaker[Session] | None = None

    def initialize(self, session_factory: sessionmaker[Session]) -> None:
        self._session_factory = session_factory
        logger.info("TeamDefinitionStore initialised")

    @property
    def _sf(self) -> sessionmaker[Session]:
        assert self._session_factory is not None, "TeamDefinitionStore not initialised"
        return self._session_factory

    # ---- conversions -----------------------------------------------------

    @staticmethod
    def _record_to_definition(record: TeamDefinitionRecord) -> TeamDefinition:
        return TeamDefinition(
            id=record.id,
            name=record.name,
            description=record.description or "",
            planner_agent=record.planner_agent,
            worker_agents=list(record.worker_agents or []),
        )

    # ---- CRUD ------------------------------------------------------------

    def create(
        self,
        *,
        name: str,
        planner_agent: str,
        worker_agents: list[str] | None = None,
        description: str = "",
    ) -> TeamDefinition:
        """Insert a new team definition. Raises if the name already exists."""
        with self._sf() as db:
            existing = (
                db.query(TeamDefinitionRecord)
                .filter(TeamDefinitionRecord.name == name)
                .first()
            )
            if existing is not None:
                raise ValueError(f"team_definition '{name}' already exists")
            record = TeamDefinitionRecord(
                id=str(uuid4()),
                name=name,
                description=description,
                planner_agent=planner_agent,
                worker_agents=list(worker_agents or []),
            )
            db.add(record)
            db.commit()
            db.refresh(record)
            return self._record_to_definition(record)

    def get_by_name(self, name: str) -> TeamDefinition | None:
        with self._sf() as db:
            record = (
                db.query(TeamDefinitionRecord)
                .filter(TeamDefinitionRecord.name == name)
                .first()
            )
            return self._record_to_definition(record) if record is not None else None

    def list_all(self) -> list[TeamDefinition]:
        with self._sf() as db:
            records = (
                db.query(TeamDefinitionRecord)
                .order_by(TeamDefinitionRecord.name)
                .all()
            )
            return [self._record_to_definition(r) for r in records]

    def delete(self, name: str) -> bool:
        """Hard delete by name. Returns True if a row was removed."""
        with self._sf() as db:
            record = (
                db.query(TeamDefinitionRecord)
                .filter(TeamDefinitionRecord.name == name)
                .first()
            )
            if record is None:
                return False
            db.delete(record)
            db.commit()
            return True

    def get_or_create(
        self,
        *,
        name: str,
        planner_agent: str,
        worker_agents: list[str] | None = None,
        description: str = "",
    ) -> TeamDefinition:
        """Idempotent seed helper: return existing row or create one.

        Does NOT update an existing row if its fields drift from the
        defaults — seeding is one-shot. Callers that want to overwrite an
        existing row should delete + create explicitly.
        """
        existing = self.get_by_name(name)
        if existing is not None:
            return existing
        return self.create(
            name=name,
            planner_agent=planner_agent,
            worker_agents=worker_agents,
            description=description,
        )
