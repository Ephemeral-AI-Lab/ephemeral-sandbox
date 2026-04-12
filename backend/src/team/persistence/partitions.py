"""Per-run partition lifecycle for team coordination tables.

Creates and drops LIST partitions keyed on team_run_id for the
task_notes, file_changes, and tasks tables. Partition names use a
SHA-256 hash prefix of the run_id for safe DDL identifiers.

See Section 14.4 of the coordination redesign doc.
"""

from __future__ import annotations

import hashlib
import logging
import re

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncConnection

logger = logging.getLogger(__name__)

_VALID_RUN_ID = re.compile(r'^[a-zA-Z0-9_\-]+$')
_PARTITIONED_TABLES = ("task_notes", "file_changes", "tasks")


def _partition_suffix(run_id: str) -> str:
    """Deterministic, safe suffix from run_id."""
    return hashlib.sha256(run_id.encode()).hexdigest()[:12]


async def create_partitions(conn: AsyncConnection, run_id: str) -> None:
    """Create per-run partitions for all team coordination tables.

    Args:
        conn: An active async connection (caller manages transaction).
        run_id: The team run identifier. Must match [a-zA-Z0-9_\\-]+.

    Raises:
        ValueError: If run_id contains unsafe characters.
    """
    if not _VALID_RUN_ID.match(run_id):
        raise ValueError(f"Invalid run_id for partition: {run_id!r}")

    suffix = _partition_suffix(run_id)
    for table in _PARTITIONED_TABLES:
        partition_name = f"{table}_{suffix}"
        ddl = (
            f"CREATE TABLE IF NOT EXISTS {partition_name} "
            f"PARTITION OF {table} FOR VALUES IN ('{run_id}')"
        )
        await conn.execute(text(ddl))
        logger.debug("Created partition %s for run %s", partition_name, run_id)


async def drop_partitions(conn: AsyncConnection, run_id: str) -> None:
    """Drop per-run partitions. Instant cleanup, no vacuum needed.

    Args:
        conn: An active async connection (caller manages transaction).
        run_id: The team run identifier.

    Raises:
        ValueError: If run_id contains unsafe characters.
    """
    if not _VALID_RUN_ID.match(run_id):
        raise ValueError(f"Invalid run_id for partition: {run_id!r}")

    suffix = _partition_suffix(run_id)
    for table in _PARTITIONED_TABLES:
        partition_name = f"{table}_{suffix}"
        await conn.execute(text(f"DROP TABLE IF EXISTS {partition_name}"))
        logger.debug("Dropped partition %s for run %s", partition_name, run_id)
