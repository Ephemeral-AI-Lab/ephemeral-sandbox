"""Agent builder service — bridges DB records and runtime agent definitions."""

from __future__ import annotations

import logging
from datetime import datetime, timezone
from uuid import uuid4

from ephemeralos.coordinator.agent_definitions import AgentDefinition
from ephemeralos.db.models.agent_definition import AgentDefinitionRecord
from ephemeralos.db.stores.agent_definition_store import AgentDefinitionStore
from ephemeralos.services.agent_builder.validation import AgentDefinitionValidator
from ephemeralos.ui.schemas.agent_schemas import (
    AgentDefinitionCreate,
    AgentDefinitionResponse,
    AgentDefinitionUpdate,
    AgentValidationResult,
)

logger = logging.getLogger(__name__)


class AgentBuilderService:
    """Converts DB records ↔ runtime AgentDefinition with validation."""

    def __init__(
        self,
        store: AgentDefinitionStore,
        validator: AgentDefinitionValidator,
    ) -> None:
        self._store = store
        self._validator = validator

    # -- conversion ------------------------------------------------------------

    @staticmethod
    def record_to_definition(record: AgentDefinitionRecord) -> AgentDefinition:
        """Convert a DB record to the existing AgentDefinition Pydantic model."""
        return AgentDefinition(
            name=record.name,
            description=record.description,
            system_prompt=record.system_prompt,
            tools=record.tools,
            disallowed_tools=record.disallowed_tools,
            model=record.model,
            effort=record.effort,
            permission_mode=record.permission_mode,
            max_turns=record.max_turns,
            skills=record.skills or [],
            mcp_servers=record.mcp_servers,
            hooks=record.hooks,
            color=record.color,
            background=record.background,
            initial_prompt=record.initial_prompt,
            memory=record.memory,
            isolation=record.isolation,
            subagent_type=record.subagent_type,
            source="user",
        )

    @staticmethod
    def _record_to_response(record: AgentDefinitionRecord) -> AgentDefinitionResponse:
        return AgentDefinitionResponse(
            id=record.id,
            name=record.name,
            description=record.description,
            system_prompt=record.system_prompt,
            model=record.model,
            effort=record.effort,
            permission_mode=record.permission_mode,
            max_turns=record.max_turns,
            tools=record.tools,
            disallowed_tools=record.disallowed_tools,
            toolkits=record.toolkits,
            skills=record.skills or [],
            mcp_servers=record.mcp_servers,
            hooks=record.hooks,
            color=record.color,
            background=record.background,
            initial_prompt=record.initial_prompt,
            memory=record.memory,
            isolation=record.isolation,
            subagent_type=record.subagent_type,
            version=record.version,
            is_active=record.is_active,
            created_by=record.created_by,
            tags=record.tags,
            metadata=record.metadata_json,
            created_at=record.created_at,
            updated_at=record.updated_at,
        )

    # -- CRUD ------------------------------------------------------------------

    def create_agent(self, data: AgentDefinitionCreate) -> AgentDefinitionResponse:
        """Validate, persist, and register a new agent definition."""
        result = self._validator.validate(data)
        if not result.valid:
            raise ValueError(f"Validation failed: {'; '.join(result.errors)}")

        # Check name collision with built-in agents
        from ephemeralos.coordinator.agent_definitions import get_definition

        existing = get_definition(data.name)
        if existing is not None and existing.source == "builtin":
            raise ValueError(f"Cannot overwrite built-in agent '{data.name}'")

        now = datetime.now(timezone.utc)
        record = AgentDefinitionRecord(
            id=str(uuid4()),
            name=data.name,
            description=data.description,
            system_prompt=data.system_prompt,
            model=data.model,
            effort=data.effort,
            permission_mode=data.permission_mode,
            max_turns=data.max_turns,
            tools=data.tools,
            disallowed_tools=data.disallowed_tools,
            toolkits=data.toolkits,
            skills=data.skills or [],
            mcp_servers=data.mcp_servers,
            hooks=data.hooks,
            color=data.color,
            background=data.background,
            initial_prompt=data.initial_prompt,
            memory=data.memory,
            isolation=data.isolation,
            subagent_type=data.subagent_type,
            tags=data.tags,
            metadata_json=data.metadata,
            created_by=data.created_by,
            created_at=now,
            updated_at=now,
        )
        record = self._store.create(record)

        # Register into runtime
        defn = self.record_to_definition(record)
        self._register_definition(defn)

        return self._record_to_response(record)

    def update_agent(self, name: str, data: AgentDefinitionUpdate) -> AgentDefinitionResponse:
        """Validate, update, and re-register."""
        updates = data.model_dump(exclude_unset=True)
        if not updates:
            raise ValueError("No fields to update")

        result = self._validator.validate(data)
        if not result.valid:
            raise ValueError(f"Validation failed: {'; '.join(result.errors)}")

        # Remap 'metadata' key to 'metadata_json' for the DB column
        if "metadata" in updates:
            updates["metadata_json"] = updates.pop("metadata")

        record = self._store.update(name, updates)
        defn = self.record_to_definition(record)
        self._register_definition(defn)
        return self._record_to_response(record)

    def delete_agent(self, name: str) -> bool:
        """Soft-delete and unregister from runtime."""
        ok = self._store.soft_delete(name)
        if ok:
            self._unregister_definition(name)
        return ok

    def clone_agent(self, source_name: str, new_name: str) -> AgentDefinitionResponse:
        """Clone an existing definition under a new name."""
        record = self._store.clone(source_name, new_name)
        defn = self.record_to_definition(record)
        self._register_definition(defn)
        return self._record_to_response(record)

    def validate_agent(
        self, data: AgentDefinitionCreate | AgentDefinitionUpdate
    ) -> AgentValidationResult:
        """Dry-run validation without persisting."""
        return self._validator.validate(data)

    # -- bulk load -------------------------------------------------------------

    def load_all_from_db(self) -> list[AgentDefinition]:
        """Load all active DB agents into the runtime registry (called at startup)."""
        records = self._store.list_active(limit=1000)
        definitions: list[AgentDefinition] = []
        for rec in records:
            defn = self.record_to_definition(rec)
            self._register_definition(defn)
            definitions.append(defn)
        return definitions

    # -- registry helpers ------------------------------------------------------

    @staticmethod
    def _register_definition(defn: AgentDefinition) -> None:
        from ephemeralos.coordinator.agent_definitions import register_definition

        register_definition(defn)

    @staticmethod
    def _unregister_definition(name: str) -> None:
        from ephemeralos.coordinator.agent_definitions import unregister_definition

        unregister_definition(name)
