"""SWE-EVO benchmark adapter (dataset, sandbox, evaluation)."""

from benchmarks.sweevo.dataset import (
    classify_sweevo_instance_size,
    count_sweevo_changelog_items,
    default_sweevo_snapshot_name,
    load_sweevo_dataset,
    load_sweevo_instance,
    select_sweevo_instance,
    summarize_sweevo_instance,
)
from benchmarks.sweevo.evaluation import evaluate_sweevo_result
from benchmarks.sweevo.models import SWEEvoInstance, SWEEvoResult
from benchmarks.sweevo.sandbox import (
    create_sweevo_test_sandbox,
    prepare_sweevo_test_run,
    provision_sweevo_sandbox,
    register_sweevo_snapshot,
    resolve_sweevo_snapshot,
    run_sweevo_required_test,
    setup_sweevo_sandbox,
)

__all__ = [
    "SWEEvoInstance",
    "SWEEvoResult",
    "classify_sweevo_instance_size",
    "count_sweevo_changelog_items",
    "create_sweevo_test_sandbox",
    "default_sweevo_snapshot_name",
    "evaluate_sweevo_result",
    "load_sweevo_dataset",
    "load_sweevo_instance",
    "prepare_sweevo_test_run",
    "provision_sweevo_sandbox",
    "register_sweevo_snapshot",
    "resolve_sweevo_snapshot",
    "run_sweevo_required_test",
    "select_sweevo_instance",
    "setup_sweevo_sandbox",
    "summarize_sweevo_instance",
]
