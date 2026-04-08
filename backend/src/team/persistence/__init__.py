"""Team definition persistence module.

Mirrors ``agents/db/`` — a SQLAlchemy model plus a thin CRUD store. The
store is initialised by the application factory (alongside the existing
stores) with a session factory; tests use an in-memory SQLite session
factory via the same ``initialize()`` contract.
"""

from team.persistence.model import TeamDefinitionRecord
from team.persistence.store import TeamDefinitionStore

__all__ = ["TeamDefinitionRecord", "TeamDefinitionStore"]
