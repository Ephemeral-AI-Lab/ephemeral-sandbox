"""Agent builder service — bridges DB records and runtime agent definitions."""

from __future__ import annotations

from datetime import datetime, UTC
from uuid import uuid4

from agents.types import AgentDefinition
from agents.db.model import AgentDefinitionRecord
from agents.db.store import AgentDefinitionStore
from agents.builder.validation import AgentDefinitionValidator
from agents.api.schemas import (
    AgentDefinitionCreate,
    AgentDefinitionResponse,
    AgentDefinitionUpdate,
    AgentValidationResult,
)

class AgentBuilderService:
    """Converts DB records to/from runtime AgentDefinition with validation."""

    def __init__(self, store: AgentDefinitionStore, validator: AgentDefinitionValidator) -> None:
        self._store = store
        self._validator = validator

    @staticmethod
    def record_to_definition(record: AgentDefinitionRecord) -> AgentDefinition:
        return AgentDefinition(
            name=record.name,
            description=record.description,
            system_prompt=record.system_prompt,
            model=record.model,
            effort=record.effort,
            tool_call_limit=record.tool_call_limit,
            skills=record.skills or [],
            toolkits=record.toolkits or [],
            hooks=record.hooks,
            background=record.background,
            initial_prompt=record.initial_prompt,
            role=record.role,
            agent_type=record.agent_type or "agent",
            supported_kinds=record.supported_kinds or ["atomic", "expandable"],
            source=record.source or "user",
            can_spawn_subagents=record.can_spawn_subagents,
            require_fresh_client=record.require_fresh_client,
            include_skills=record.include_skills,
            dispatchable_via_run_subagent=record.dispatchable_via_run_subagent,
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
            tool_call_limit=record.tool_call_limit,
            toolkits=record.toolkits,
            skills=record.skills or [],
            hooks=record.hooks,
            background=record.background,
            initial_prompt=record.initial_prompt,
            version=record.version,
            is_active=record.is_active,
            created_by=record.created_by,
            tags=record.tags,
            metadata=record.metadata_json,
            created_at=record.created_at,
            updated_at=record.updated_at,
        )

    @staticmethod
    def _record_payload_from_request(data: AgentDefinitionCreate) -> dict[str, object]:
        return {
            "description": data.description,
            "system_prompt": data.system_prompt,
            "model": data.model,
            "effort": data.effort,
            "tool_call_limit": data.tool_call_limit,
            "toolkits": data.toolkits,
            "skills": data.skills or [],
            "hooks": data.hooks,
            "background": data.background,
            "initial_prompt": data.initial_prompt,
            "tags": data.tags,
            "metadata_json": data.metadata,
            "created_by": data.created_by,
        }

    def create_agent(self, data: AgentDefinitionCreate) -> AgentDefinitionResponse:
        result = self._validator.validate(data)
        if not result.valid:
            raise ValueError(f"Validation failed: {'; '.join(result.errors)}")

        from agents.registry import get_definition

        existing = get_definition(data.name)
        if existing is not None and existing.source == "builtin":
            raise ValueError(f"Cannot overwrite built-in agent '{data.name}'")

        # Check for inactive record with the same name — reactivate instead of inserting
        inactive = self._store.get_by_name(data.name, active_only=False)
        if inactive is not None:
            if inactive.is_active:
                raise ValueError(f"Agent '{data.name}' already exists")
            # Reactivate with new data
            updates = self._record_payload_from_request(data)
            updates["is_active"] = True
            record = self._store.update(data.name, updates)
            self._register(self.record_to_definition(record))
            return self._record_to_response(record)

        now = datetime.now(UTC)
        record = AgentDefinitionRecord(
            id=str(uuid4()),
            name=data.name,
            **self._record_payload_from_request(data),
            created_at=now,
            updated_at=now,
        )
        record = self._store.create(record)
        self._register(self.record_to_definition(record))
        return self._record_to_response(record)

    def update_agent(self, name: str, data: AgentDefinitionUpdate) -> AgentDefinitionResponse:
        updates = data.model_dump(exclude_unset=True)
        if not updates:
            raise ValueError("No fields to update")
        result = self._validator.validate(data)
        if not result.valid:
            raise ValueError(f"Validation failed: {'; '.join(result.errors)}")
        if "metadata" in updates:
            updates["metadata_json"] = updates.pop("metadata")
        record = self._store.update(name, updates)
        self._register(self.record_to_definition(record))
        return self._record_to_response(record)

    def delete_agent(self, name: str) -> bool:
        ok = self._store.soft_delete(name)
        if ok:
            from agents.registry import unregister_definition

            unregister_definition(name)
        return ok

    def clone_agent(self, source_name: str, new_name: str) -> AgentDefinitionResponse:
        record = self._store.clone(source_name, new_name)
        self._register(self.record_to_definition(record))
        return self._record_to_response(record)

    def validate_agent(
        self, data: AgentDefinitionCreate | AgentDefinitionUpdate
    ) -> AgentValidationResult:
        return self._validator.validate(data)

    def load_all_from_db(self) -> list[AgentDefinition]:
        records = self._store.list_active(limit=1000)
        definitions: list[AgentDefinition] = []
        for rec in records:
            defn = self.record_to_definition(rec)
            self._register(defn)
            definitions.append(defn)
        return definitions

    @staticmethod
    def _register(defn: AgentDefinition) -> None:
        from agents.registry import register_definition

        register_definition(defn)
