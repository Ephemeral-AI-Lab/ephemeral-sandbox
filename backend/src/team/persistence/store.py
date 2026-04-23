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
    def _normalize_roster(raw_roster: object) -> dict[str, list[str]]:
        if not isinstance(raw_roster, dict):
            return {}
        roster: dict[str, list[str]] = {}
        for role, agents in raw_roster.items():
            names = [str(agent) for agent in (agents or []) if str(agent)]
            if names:
                roster[str(role)] = names
        return roster

    @classmethod
    def _worker_agents_from_roster(
        cls,
        *,
        entry_planner: str,
        roster: dict[str, list[str]],
    ) -> list[str]:
        worker_agents: list[str] = []
        seen: set[str] = set()
        for agents in cls._normalize_roster(roster).values():
            for agent in agents:
                if agent == entry_planner or agent in seen:
                    continue
                seen.add(agent)
                worker_agents.append(agent)
        return worker_agents

    @classmethod
    def _record_to_definition(cls, record: TeamDefinitionRecord) -> TeamDefinition:
        raw_roster = record.roster if isinstance(record.roster, dict) else None
        roster = cls._normalize_roster(raw_roster) if raw_roster is not None else {}
        entry_planner = str(
            record.entry_planner
            or record.planner_agent
            or next(iter(roster.get("planner", [])), "")
        )
        if raw_roster is None:
            if entry_planner:
                roster["planner"] = [entry_planner]
            if record.worker_agents:
                roster["worker"] = [str(agent) for agent in record.worker_agents if str(agent)]
        return TeamDefinition(
            id=record.id,
            name=record.name,
            description=record.description or "",
            entry_planner=entry_planner,
            roster=roster,
        )

    # ---- CRUD ------------------------------------------------------------

    def create(
        self,
        *,
        name: str,
        entry_planner: str,
        roster: dict[str, list[str]],
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
            normalized_roster = self._normalize_roster(roster)
            record = TeamDefinitionRecord(
                id=str(uuid4()),
                name=name,
                description=description,
                planner_agent=entry_planner,
                worker_agents=self._worker_agents_from_roster(
                    entry_planner=entry_planner,
                    roster=normalized_roster,
                ),
                entry_planner=entry_planner,
                roster=normalized_roster,
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

    def get_by_id(self, team_id: str) -> TeamDefinition | None:
        with self._sf() as db:
            record = (
                db.query(TeamDefinitionRecord)
                .filter(TeamDefinitionRecord.id == team_id)
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

    def seed_builtin(self, defn: TeamDefinition) -> TeamDefinition:
        """Insert a team definition if it doesn't already exist.

        Existing records are left untouched so user customisations via the
        CRUD API are never overwritten by a restart.
        """
        with self._sf() as db:
            existing = (
                db.query(TeamDefinitionRecord)
                .filter(TeamDefinitionRecord.name == defn.name)
                .first()
            )
            if existing is not None:
                return self._record_to_definition(existing)
            normalized_roster = self._normalize_roster(defn.roster)
            normalized_terminal_tools = self._normalize_terminal_tools(defn.terminal_tools)
            record = TeamDefinitionRecord(
                id=str(uuid4()),
                name=defn.name,
                description=defn.description,
                planner_agent=defn.entry_planner,
                worker_agents=self._worker_agents_from_roster(
                    entry_planner=defn.entry_planner,
                    roster=normalized_roster,
                ),
                entry_planner=defn.entry_planner,
                roster=normalized_roster,
                terminal_tools={k: sorted(v) for k, v in normalized_terminal_tools.items()},
            )
            db.add(record)
            db.commit()
            db.refresh(record)
            return self._record_to_definition(record)

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
