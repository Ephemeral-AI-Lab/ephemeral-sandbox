"""Agent definition persistence store."""

from __future__ import annotations

from datetime import datetime, UTC
from typing import TYPE_CHECKING, Any
from uuid import uuid4

from agents.db.model import AgentDefinitionRecord
from db.stores.definition_store import DefinitionStoreBase

if TYPE_CHECKING:
    from agents.types import AgentDefinition


class AgentDefinitionStore(DefinitionStoreBase[AgentDefinitionRecord]):
    """CRUD operations for agent definition records."""

    record_type = AgentDefinitionRecord

    def get_by_name(self, name: str, *, active_only: bool = True) -> AgentDefinitionRecord | None:
        return self._get_by_name(name, active_only=active_only)

    def seed_builtin(self, defn: "AgentDefinition") -> AgentDefinitionRecord:
        """Insert a builtin agent definition if it doesn't already exist.

        Existing records (even inactive) are left untouched so user
        customisations are never overwritten by a restart.
        """
        from dataclasses import asdict as _dc_asdict

        with self._sf() as db:
            existing = self._get_by_name_with_session(db, defn.name, active_only=False)
            if existing is not None:
                return existing

            now = datetime.now(UTC)
            record = AgentDefinitionRecord(
                id=str(uuid4()),
                name=defn.name,
                description=defn.description,
                system_prompt=defn.system_prompt,
                model=defn.model or "inherit",
                effort=str(defn.effort) if defn.effort else None,
                tool_call_limit=defn.tool_call_limit,
                toolkits=defn.toolkits or [],
                skills=defn.skills or [],
                hooks=defn.hooks,
                background=defn.background,
                initial_prompt=defn.initial_prompt,
                role=defn.role,
                agent_type=defn.agent_type,
                supported_kinds=defn.supported_kinds,
                source=defn.source,
                can_spawn_subagents=defn.can_spawn_subagents,
                require_fresh_client=defn.require_fresh_client,
                include_skills=defn.include_skills,
                dispatchable_via_run_subagent=defn.dispatchable_via_run_subagent,
                version=1,
                is_active=True,
                created_at=now,
                updated_at=now,
            )
            db.add(record)
            db.commit()
            db.refresh(record)
            return record

    def list_active(
        self, *, tags: list[str] | None = None, limit: int = 50, offset: int = 0
    ) -> list[AgentDefinitionRecord]:
        with self._sf() as db:
            q = (
                db.query(AgentDefinitionRecord)
                .filter(AgentDefinitionRecord.is_active.is_(True))
                .order_by(AgentDefinitionRecord.name)
            )
            if tags:
                for tag in tags:
                    q = q.filter(AgentDefinitionRecord.tags.contains([tag]))
            return list(q.offset(offset).limit(limit).all())

    def update(self, name: str, updates: dict[str, Any]) -> AgentDefinitionRecord:
        return self._update_by_name(
            name,
            updates,
            active_only=False,
            missing_message=f"Agent definition '{name}' not found",
        )

    def soft_delete(self, name: str) -> bool:
        return self._soft_delete_by_name(name)


    def clone(self, source_name: str, new_name: str) -> AgentDefinitionRecord:
        with self._sf() as db:
            source = self._get_by_name_with_session(db, source_name)
            if source is None:
                raise KeyError(f"Source agent '{source_name}' not found")
            # Check if new_name already exists (including inactive)
            existing = self._get_by_name_with_session(db, new_name, active_only=False)
            if existing is not None:
                if existing.is_active:
                    raise KeyError(f"Agent '{new_name}' already exists")
                # Reactivate inactive record with cloned data
                self._apply_updates(existing, self._clone_payload(source))
                existing.is_active = True
                existing.version += 1
                existing.updated_at = datetime.now(UTC)
                db.commit()
                db.refresh(existing)
                return existing
            now = datetime.now(UTC)
            clone_record = AgentDefinitionRecord(
                id=str(uuid4()),
                name=new_name,
                **self._clone_payload(source),
                version=1,
                is_active=True,
                created_at=now,
                updated_at=now,
            )
            db.add(clone_record)
            db.commit()
            db.refresh(clone_record)
            return clone_record

    @staticmethod
    def _clone_payload(source: AgentDefinitionRecord) -> dict[str, Any]:
        return {
            "description": source.description,
            "system_prompt": source.system_prompt,
            "model": source.model,
            "effort": source.effort,
            "tool_call_limit": source.tool_call_limit,
            "toolkits": source.toolkits,
            "skills": source.skills or [],
            "hooks": source.hooks,
            "background": source.background,
            "initial_prompt": source.initial_prompt,
            "role": source.role,
            "agent_type": source.agent_type,
            "supported_kinds": source.supported_kinds,
            "source": source.source,
            "can_spawn_subagents": source.can_spawn_subagents,
            "require_fresh_client": source.require_fresh_client,
            "include_skills": source.include_skills,
            "dispatchable_via_run_subagent": source.dispatchable_via_run_subagent,
            "created_by": source.created_by,
            "tags": source.tags,
            "metadata_json": source.metadata_json,
        }
