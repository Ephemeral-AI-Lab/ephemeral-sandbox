"""TaskCenter request/run/task/harness-graph persistence models."""

from __future__ import annotations

from datetime import UTC, datetime
from typing import TYPE_CHECKING

from sqlalchemy import DateTime, ForeignKey, String, Text
from sqlalchemy.dialects.postgresql import JSON
from sqlalchemy.orm import Mapped, mapped_column, relationship

from db.base import Base

if TYPE_CHECKING:
    from db.models.agent_run import AgentRunRecord


class TaskCenterRequestRecord(Base):
    __tablename__ = "task_center_requests"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)
    cwd: Mapped[str] = mapped_column(String(1024))
    sandbox_id: Mapped[str | None] = mapped_column(String(128), nullable=True)
    request_prompt: Mapped[str] = mapped_column(Text)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=lambda: datetime.now(UTC)
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True),
        default=lambda: datetime.now(UTC),
        onupdate=lambda: datetime.now(UTC),
    )

    runs: Mapped[list["TaskCenterRunRecord"]] = relationship(
        "TaskCenterRunRecord",
        back_populates="request",
        cascade="all, delete-orphan",
    )

    def __repr__(self) -> str:
        return f"<TaskCenterRequestRecord id={self.id!r}>"


class TaskCenterRunRecord(Base):
    __tablename__ = "task_center_runs"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)
    request_id: Mapped[str] = mapped_column(
        String(36),
        ForeignKey("task_center_requests.id", ondelete="CASCADE"),
        index=True,
    )
    root_task_id: Mapped[str | None] = mapped_column(String(96), nullable=True)
    status: Mapped[str] = mapped_column(String(32), default="running")
    started_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=lambda: datetime.now(UTC)
    )
    finished_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)

    request: Mapped[TaskCenterRequestRecord] = relationship(back_populates="runs")
    tasks: Mapped[list["TaskCenterTaskRecord"]] = relationship(
        "TaskCenterTaskRecord",
        back_populates="run",
        cascade="all, delete-orphan",
    )
    harness_graphs: Mapped[list["TaskCenterHarnessGraphRecord"]] = relationship(
        "TaskCenterHarnessGraphRecord",
        back_populates="run",
        cascade="all, delete-orphan",
    )

    def __repr__(self) -> str:
        return f"<TaskCenterRunRecord id={self.id!r} status={self.status!r}>"


class TaskCenterTaskRecord(Base):
    __tablename__ = "task_center_tasks"

    id: Mapped[str] = mapped_column(String(96), primary_key=True)
    run_id: Mapped[str] = mapped_column(
        String(36),
        ForeignKey("task_center_runs.id", ondelete="CASCADE"),
        index=True,
    )
    role: Mapped[str] = mapped_column(String(32))
    task_input: Mapped[str] = mapped_column(Text)
    status: Mapped[str] = mapped_column(String(32))
    summaries: Mapped[list[dict]] = mapped_column(JSON, default=list)
    needs: Mapped[list[str]] = mapped_column(JSON, default=list)
    task_center_harness_graph_id: Mapped[str | None] = mapped_column(
        String(96), nullable=True
    )
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=lambda: datetime.now(UTC)
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True),
        default=lambda: datetime.now(UTC),
        onupdate=lambda: datetime.now(UTC),
    )

    run: Mapped[TaskCenterRunRecord] = relationship(back_populates="tasks")
    agent_run: Mapped["AgentRunRecord | None"] = relationship(
        "AgentRunRecord",
        back_populates="task",
        uselist=False,
    )

    def __repr__(self) -> str:
        return f"<TaskCenterTaskRecord id={self.id!r} status={self.status!r}>"


class TaskCenterHarnessGraphRecord(Base):
    """Persisted harness graph (planner + executor children + evaluator)."""

    __tablename__ = "task_center_harness_graph"

    id: Mapped[str] = mapped_column(String(96), primary_key=True)
    run_id: Mapped[str] = mapped_column(
        String(36),
        ForeignKey("task_center_runs.id", ondelete="CASCADE"),
        index=True,
    )
    root_task_id: Mapped[str] = mapped_column(String(96))
    planner_task_id: Mapped[str] = mapped_column(String(96))
    evaluator_task_id: Mapped[str | None] = mapped_column(String(96), nullable=True)
    executor_task_ids: Mapped[list[str]] = mapped_column(JSON, default=list)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=lambda: datetime.now(UTC)
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True),
        default=lambda: datetime.now(UTC),
        onupdate=lambda: datetime.now(UTC),
    )

    run: Mapped[TaskCenterRunRecord] = relationship(back_populates="harness_graphs")

    def __repr__(self) -> str:
        return f"<TaskCenterHarnessGraphRecord id={self.id!r}>"
