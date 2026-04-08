"""SWE-EVO dataset loading and instance selection."""

from __future__ import annotations

import functools
import json
import logging
from typing import Any

from benchmarks.sweevo.models import (
    SWEEvoInstance,
    _DEFAULT_DATASET_SOURCE,
    _DEFAULT_TARGET_BULLETS,
)

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Dataset loader
# ---------------------------------------------------------------------------


def _parse_test_list(raw: Any) -> list[str]:
    """Parse fail_to_pass / pass_to_pass which may be list or JSON string."""
    if isinstance(raw, list):
        return raw
    if isinstance(raw, str):
        try:
            parsed = json.loads(raw)
            if isinstance(parsed, list):
                return parsed
        except (json.JSONDecodeError, TypeError):
            pass
        # Fallback: split by newline
        return [line.strip() for line in raw.splitlines() if line.strip()]
    return []


@functools.lru_cache(maxsize=4)
def _load_sweevo_rows(source: str, split: str) -> tuple[dict[str, Any], ...]:
    """Cache-friendly loader that returns raw rows as a hashable tuple."""
    if source.endswith(".parquet"):
        import pandas as pd

        df = pd.read_parquet(source)
        return tuple(df.to_dict("records"))
    else:
        from datasets import load_dataset

        ds = load_dataset(source, split=split)
        return tuple(dict(row) for row in ds)


def load_sweevo_dataset(
    source: str = "Fsoft-AIC/SWE-EVO",
    *,
    split: str = "test",
) -> list[SWEEvoInstance]:
    """Load SWE-EVO instances from HuggingFace or local Parquet."""
    rows = _load_sweevo_rows(source, split)

    instances = []
    for row in rows:
        instances.append(
            SWEEvoInstance(
                instance_id=row["instance_id"],
                repo=row["repo"],
                base_commit=row["base_commit"],
                problem_statement=row["problem_statement"],
                patch=row.get("patch", ""),
                fail_to_pass=_parse_test_list(row.get("FAIL_TO_PASS", [])),
                pass_to_pass=_parse_test_list(row.get("PASS_TO_PASS", [])),
                docker_image=row.get("image", ""),
                test_cmds=row.get("test_cmds", "pytest --continue-on-collection-errors -rA"),
                environment_setup_commit=row.get("environment_setup_commit", ""),
                test_patch=row.get("test_patch", ""),
                start_version=row.get("start_version", ""),
                end_version=row.get("end_version", ""),
                instance_id_swe=row.get("instance_id_swe", ""),
            )
        )

    logger.info("Loaded %d SWE-EVO instances from %s", len(instances), source)
    return instances


def load_sweevo_instance(
    source: str = _DEFAULT_DATASET_SOURCE,
    instance_id: str = "",
) -> SWEEvoInstance:
    """Load a single SWE-EVO instance by ID."""
    instances = load_sweevo_dataset(source)
    for inst in instances:
        if inst.instance_id == instance_id:
            return inst
    available = [i.instance_id for i in instances]
    raise ValueError(f"Instance '{instance_id}' not found. Available: {available}")


def count_sweevo_changelog_items(instance: SWEEvoInstance) -> int:
    """Count markdown bullet items in a SWE-EVO problem statement."""
    return sum(
        1 for line in instance.problem_statement.splitlines() if line.lstrip().startswith("-")
    )


def classify_sweevo_instance_size(bullet_count: int) -> str:
    """Classify a SWE-EVO instance by changelog size."""
    if bullet_count >= 20:
        return "large"
    if bullet_count >= 5:
        return "medium"
    return "small"


def default_sweevo_snapshot_name(instance: SWEEvoInstance) -> str:
    """Return a stable Daytona snapshot name for a SWE-EVO instance."""
    name = f"sweevo-{instance.instance_id_swe or instance.instance_id}"
    return name[:63]


def summarize_sweevo_instance(instance: SWEEvoInstance) -> dict[str, Any]:
    """Return a compact metadata dict for one SWE-EVO instance."""
    bullet_count = count_sweevo_changelog_items(instance)
    return {
        "instance_id": instance.instance_id,
        "repo": instance.repo,
        "start_version": instance.start_version,
        "end_version": instance.end_version,
        "docker_image": instance.docker_image,
        "test_cmds": instance.test_cmds,
        "bullet_count": bullet_count,
        "size": classify_sweevo_instance_size(bullet_count),
        "fail_to_pass_count": len(instance.fail_to_pass),
        "pass_to_pass_count": len(instance.pass_to_pass),
        "default_snapshot_name": default_sweevo_snapshot_name(instance),
    }


def select_sweevo_instance(
    *,
    source: str = _DEFAULT_DATASET_SOURCE,
    instance_id: str | None = None,
    size: str = "medium",
    target_bullets: int = _DEFAULT_TARGET_BULLETS,
) -> SWEEvoInstance:
    """Select a SWE-EVO instance, defaulting to a medium one near target bullets."""
    if instance_id:
        return load_sweevo_instance(source, instance_id)

    normalized_size = (size or "medium").strip().lower()
    if normalized_size not in {"small", "medium", "large", "any"}:
        raise ValueError(
            f"Invalid SWE-EVO size '{size}'. Expected one of: small, medium, large, any."
        )

    instances = load_sweevo_dataset(source)
    candidates: list[tuple[SWEEvoInstance, int]] = []
    for inst in instances:
        bullets = count_sweevo_changelog_items(inst)
        if normalized_size != "any" and classify_sweevo_instance_size(bullets) != normalized_size:
            continue
        candidates.append((inst, bullets))

    if not candidates:
        raise ValueError(f"No SWE-EVO instances matched size '{normalized_size}'.")

    target = max(0, int(target_bullets))
    selected, _ = min(
        candidates,
        key=lambda item: (
            abs(item[1] - target),
            item[1],
            item[0].instance_id,
        ),
    )
    return selected
