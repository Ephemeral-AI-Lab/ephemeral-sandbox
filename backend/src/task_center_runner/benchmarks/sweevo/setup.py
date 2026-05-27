"""Stage 1 — preflight + persistent-sandbox provisioning.

Folds the legacy ``dataset.py`` and ``prompt.py`` loaders directly into the
stage that uses them, since they have no other callers after the migration.
"""

from __future__ import annotations

import argparse
import csv
import functools
import json
import logging
import os
import sys
import time
from pathlib import Path
from typing import Any

from task_center_runner.benchmarks.sweevo._provision import (
    _create_sandbox,
    _find_existing_sandbox_by_name,
    _resume_sandbox,
    _service,
    setup_sweevo_sandbox,
)
from task_center_runner.benchmarks.sweevo._snapshot import (
    SnapshotNotRegisteredError,
    verify_sweevo_snapshot_exists,
)
from task_center_runner.benchmarks.sweevo.models import (
    PreContext,
    SWEEvoInstance,
    _DEFAULT_DATASET_SOURCE,
    _DEFAULT_TARGET_BULLETS,
    _REPO_DIR,
    _has_explicit_sweevo_image_version,
    _sweevo_sandbox_name,
)

logger = logging.getLogger(__name__)

_PROJECT_ROOT = Path(__file__).resolve().parents[4]
_PR_DESCRIPTION_CSV_ENV = "SWEEVO_PR_DESCRIPTIONS_CSV"
_PR_DESCRIPTION_CSV_PATH = (
    _PROJECT_ROOT
    / "backend"
    / "config"
    / "benchmarks"
    / "sweevo_gpt5_2025_08_07_pr_descriptions.csv"
)

_RUN_T0 = time.monotonic()


def _step(msg: str) -> None:
    elapsed = time.monotonic() - _RUN_T0
    print(f"[sweevo +{elapsed:7.2f}s] {msg}", file=sys.stderr, flush=True)


# ---------------------------------------------------------------------------
# Dataset loaders (folded from dataset.py)
# ---------------------------------------------------------------------------


def _parse_test_list(raw: Any) -> list[str]:
    if isinstance(raw, list):
        return raw
    if isinstance(raw, str):
        try:
            parsed = json.loads(raw)
            if isinstance(parsed, list):
                return parsed
        except (json.JSONDecodeError, TypeError):
            pass
        return [line.strip() for line in raw.splitlines() if line.strip()]
    return []


def _load_cached_arrow_rows(source: str, split: str) -> tuple[dict[str, Any], ...] | None:
    try:
        from datasets import Dataset
        from datasets.config import HF_DATASETS_CACHE
    except Exception:
        return None

    cache_root = Path(HF_DATASETS_CACHE)
    if not cache_root.exists():
        return None

    source_key = source.replace("/", "___").lower()
    candidates: list[Path] = []
    for entry in cache_root.iterdir():
        if not entry.is_dir() or entry.name.lower() != source_key:
            continue
        candidates.extend(path for path in entry.rglob(f"*{split}.arrow") if path.is_file())

    if not candidates:
        return None

    arrow_path = max(candidates, key=lambda path: path.stat().st_mtime)
    ds = Dataset.from_file(str(arrow_path))
    logger.info("Loaded SWE-EVO split %s from cached Arrow dataset %s", split, arrow_path)
    return tuple(dict(row) for row in ds)


@functools.lru_cache(maxsize=4)
def _load_sweevo_rows(source: str, split: str) -> tuple[dict[str, Any], ...]:
    if source.endswith(".parquet"):
        import pandas as pd

        df = pd.read_parquet(source)
        return tuple(df.to_dict("records"))

    cached_rows = _load_cached_arrow_rows(source, split)
    if cached_rows is not None:
        return cached_rows

    from datasets import load_dataset

    try:
        ds = load_dataset(source, split=split)
        return tuple(dict(row) for row in ds)
    except Exception:
        cached_rows = _load_cached_arrow_rows(source, split)
        if cached_rows is not None:
            logger.warning(
                "Falling back to cached SWE-EVO dataset after remote load failure for %s[%s]",
                source,
                split,
                exc_info=True,
            )
            return cached_rows
        raise


def load_sweevo_dataset(
    source: str = _DEFAULT_DATASET_SOURCE,
    *,
    split: str = "test",
) -> list[SWEEvoInstance]:
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
                pr_description=row.get("pr_description", ""),
            )
        )
    logger.info("Loaded %d SWE-EVO instances from %s", len(instances), source)
    return instances


def load_sweevo_instance(
    source: str = _DEFAULT_DATASET_SOURCE,
    instance_id: str = "",
) -> SWEEvoInstance:
    instances = load_sweevo_dataset(source)
    for inst in instances:
        if inst.instance_id == instance_id:
            return inst
    available = [i.instance_id for i in instances]
    raise ValueError(f"Instance '{instance_id}' not found. Available: {available}")


_BULLET_RE = __import__("re").compile(r"^\s*(?:[-*+]\s+|\d+[.)]\s+)")


def count_sweevo_changelog_items(instance: SWEEvoInstance) -> int:
    if not instance.problem_statement:
        return 0
    return sum(1 for line in instance.problem_statement.splitlines() if _BULLET_RE.match(line))


def classify_sweevo_instance_size(bullet_count: int) -> str:
    if bullet_count >= 20:
        return "large"
    if bullet_count >= 5:
        return "medium"
    return "small"


def summarize_sweevo_instance(instance: SWEEvoInstance) -> dict[str, Any]:
    from task_center_runner.benchmarks.sweevo.models import default_sweevo_snapshot_name

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


# ---------------------------------------------------------------------------
# PR description loader (folded from prompt.py)
# ---------------------------------------------------------------------------


@functools.lru_cache(maxsize=8)
def load_pr_description_overrides(csv_path: str) -> dict[str, str]:
    path = Path(csv_path)
    if not path.exists():
        return {}

    descriptions: dict[str, str] = {}
    try:
        with path.open(encoding="utf-8", newline="") as handle:
            for row in csv.DictReader(handle):
                instance_id = str(row.get("test_folder") or "").strip()
                if not instance_id:
                    continue
                descriptions[instance_id] = str(row.get("pr_description") or "")
    except OSError:
        logger.debug("Unable to load SWE-EVO PR descriptions from %s", path, exc_info=True)
        return {}
    return descriptions


def pr_description_for_instance(
    instance: SWEEvoInstance,
    *,
    csv_path: str | os.PathLike[str] | None = None,
) -> str:
    resolved_csv = os.fspath(
        csv_path
        or os.environ.get(_PR_DESCRIPTION_CSV_ENV)
        or _PR_DESCRIPTION_CSV_PATH
    )
    overrides = load_pr_description_overrides(resolved_csv)
    for instance_id in (instance.instance_id, instance.instance_id_swe):
        if instance_id and (description := overrides.get(instance_id, "")).strip():
            return description

    explicit = getattr(instance, "pr_description", "")
    if explicit:
        return explicit
    return instance.problem_statement


def load_pr_description(
    instance_id: str,
    *,
    csv_path: str | os.PathLike[str] | None = None,
) -> str:
    """Strict variant — raises on missing CSV / row / empty value."""
    resolved_csv = os.fspath(
        csv_path
        or os.environ.get(_PR_DESCRIPTION_CSV_ENV)
        or _PR_DESCRIPTION_CSV_PATH
    )
    if not Path(resolved_csv).exists():
        raise FileNotFoundError(f"PR descriptions CSV not found: {resolved_csv}")
    overrides = load_pr_description_overrides(resolved_csv)
    if instance_id not in overrides:
        raise KeyError(f"instance_id {instance_id!r} not found in {resolved_csv}")
    value = overrides[instance_id]
    if not value or value.isspace():
        raise ValueError(f"row for {instance_id!r} has empty pr_description")
    return value


def build_sweevo_user_prompt(
    instance: SWEEvoInstance,
    repo_dir: str = _REPO_DIR,
    *,
    csv_path: str | os.PathLike[str] | None = None,
) -> str:
    pr_description = pr_description_for_instance(instance, csv_path=csv_path).strip()
    return (
        f"<Workspace Root>\n"
        f"{repo_dir}\n"
        f"<Workspace Root>\n\n"
        f"I've uploaded a python code repository in the directory {repo_dir}. "
        f"Consider the following PR description:\n"
        f"<pr_description>\n"
        f"{pr_description}\n"
        f"</pr_description>\n\n"
        f"Can you help me implement the necessary changes to the repository so that "
        f"the requirements specified in the <pr_description> are met?\n"
        f"I've already taken care of all changes to any of the test files described "
        f"in the <pr_description>. This means you DON'T have to modify the testing "
        f"logic or any of the tests in any way!\n"
        f"Your task is to make the minimal changes to non-tests files in the "
        f"{repo_dir} directory to ensure the <pr_description> is satisfied."
    )


# ---------------------------------------------------------------------------
# Stage 1 — preflight + provision_sandbox
# ---------------------------------------------------------------------------


def bootstrap_sandbox_provider() -> None:
    from sandbox.provider.bootstrap import bootstrap_sandbox_provider as _bootstrap

    _step("bootstrap: selecting sandbox provider via EOS_SANDBOX_PROVIDER")
    _bootstrap()
    _step("bootstrap: sandbox provider ready")


async def preflight(args: argparse.Namespace) -> PreContext:
    """Load instance, verify snapshot, return PreContext for provisioning."""
    _step(f"preflight: instance_id={args.instance_id!r}")
    goal = load_pr_description(args.instance_id, csv_path=args.csv_path)
    _step(f"preflight: pr_description loaded ({len(goal)} chars)")

    instance = load_sweevo_instance(source=args.source, instance_id=args.instance_id)
    _step(f"preflight: instance loaded — repo={instance.repo}")

    bootstrap_sandbox_provider()

    snapshot_name = ""
    if _has_explicit_sweevo_image_version(instance.docker_image):
        _step("preflight: verifying snapshot is registered")
        snapshot_name = verify_sweevo_snapshot_exists(instance)
        _step(f"preflight: snapshot ok — name={snapshot_name}")
    else:
        _step("preflight: snapshot check skipped — no explicit version; using image directly")

    audit_dir = (
        Path(args.audit_dir) if args.audit_dir
        else Path(os.getenv("EOS_SWEEVO_AUDIT_DIR", ".sweevo_runs")).resolve()
    )
    _step(f"preflight: audit_dir={audit_dir} max_duration_s={args.max_duration_s}")
    return PreContext(
        instance=instance,
        repo_dir=args.repo_dir,
        snapshot_name=snapshot_name,
        goal=goal,
        audit_dir=audit_dir,
        max_duration_s=args.max_duration_s,
    )


async def provision_sandbox(ctx: PreContext) -> str:
    """Provision (or resume) the persistent docker container for this instance.

    Naming is deterministic (``sweevo-<instance_id>``) so a second invocation
    for the same instance reuses the same container. Docker's name uniqueness
    rejects parallel duplicates.
    """
    service = _service()
    name = _sweevo_sandbox_name(ctx.instance)
    existing = _find_existing_sandbox_by_name(service, name)

    if existing is None:
        _step(f"provision: creating fresh sandbox name={name}")
        sandbox_id = await _create_sandbox(ctx.instance, name, ctx.repo_dir)
    else:
        _step(f"provision: resuming existing sandbox name={name} id={existing.get('id')}")
        sandbox_id = await _resume_sandbox(existing, name, ctx.instance, ctx.repo_dir)

    _step(f"provision: setup_sweevo_sandbox sandbox_id={sandbox_id}")
    await setup_sweevo_sandbox(
        ctx.instance,
        sandbox_id,
        ctx.repo_dir,
        install_lsp=True,
    )
    _step(f"provision: ready sandbox_id={sandbox_id}")
    return sandbox_id


SnapshotNotRegisteredError = SnapshotNotRegisteredError  # re-export for callers

__all__ = [
    "SnapshotNotRegisteredError",
    "_step",
    "bootstrap_sandbox_provider",
    "build_sweevo_user_prompt",
    "classify_sweevo_instance_size",
    "count_sweevo_changelog_items",
    "load_pr_description",
    "load_pr_description_overrides",
    "load_sweevo_dataset",
    "load_sweevo_instance",
    "pr_description_for_instance",
    "preflight",
    "provision_sandbox",
    "select_sweevo_instance",
    "summarize_sweevo_instance",
]
