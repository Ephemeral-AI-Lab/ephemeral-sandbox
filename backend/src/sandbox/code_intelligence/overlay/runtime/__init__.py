"""Sandbox-side overlay runtime package."""

from __future__ import annotations

from .classifier import Classifier
from .command import run_user_command
from .direct_routes import (
    DirectRouteApplier,
    direct_merge_factory,
    narrow_prune_opaque_factory,
)
from .git_adapters import (
    build_live_snapshot_in_namespace,
    check_ignore_factory,
    git_show_base_factory,
)
from .namespace import (
    OverlayMountError,
    _NS_LOWER,
    _NS_MERGED,
    _NS_ROOT,
    _NS_TMP,
    _NS_UPPER,
    _NS_WORK,
    setup_mounts,
)
from .ndjson import write_diff_ndjson, write_reject_ndjson
from .overlay_kinds import is_opaque_dir, is_symlink, is_whiteout
from .policy import (
    REJECT_DOTGIT,
    REJECT_GITIGNORE_WHITEOUT,
    REJECT_NON_UTF8_GITINCLUDE,
    REJECT_UNSUPPORTED_OPAQUE_DIR,
    REJECT_UNSUPPORTED_SYMLINK,
    REJECT_UPPER_FULL,
    reject_exit_code,
)
from .runner import _parse_args, main
from .types import (
    ClassificationPlan,
    ClassifyOutcome,
    DirectMergeOp,
    GitincludeChange,
    OpaquePruneOp,
    PolicyRejectOutcome,
    UpperEntry,
)
from .upperdir import walk_upperdir

__all__ = [
    "ClassificationPlan",
    "Classifier",
    "ClassifyOutcome",
    "DirectMergeOp",
    "DirectRouteApplier",
    "GitincludeChange",
    "OpaquePruneOp",
    "OverlayMountError",
    "PolicyRejectOutcome",
    "REJECT_DOTGIT",
    "REJECT_GITIGNORE_WHITEOUT",
    "REJECT_NON_UTF8_GITINCLUDE",
    "REJECT_UNSUPPORTED_OPAQUE_DIR",
    "REJECT_UNSUPPORTED_SYMLINK",
    "REJECT_UPPER_FULL",
    "UpperEntry",
    "_NS_LOWER",
    "_NS_MERGED",
    "_NS_ROOT",
    "_NS_TMP",
    "_NS_UPPER",
    "_NS_WORK",
    "_parse_args",
    "build_live_snapshot_in_namespace",
    "check_ignore_factory",
    "direct_merge_factory",
    "git_show_base_factory",
    "is_opaque_dir",
    "is_symlink",
    "is_whiteout",
    "main",
    "narrow_prune_opaque_factory",
    "reject_exit_code",
    "run_user_command",
    "setup_mounts",
    "walk_upperdir",
    "write_diff_ndjson",
    "write_reject_ndjson",
]
