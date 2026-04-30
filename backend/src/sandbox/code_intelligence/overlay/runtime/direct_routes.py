"""Direct gitignore-route live workspace mutations."""

from __future__ import annotations

import os
import shutil
import stat
import tempfile
from collections.abc import Callable

from .types import ClassificationPlan, ClassifierIOError


class DirectRouteApplier:
    """Apply gitignore-route mutations after classification succeeds."""

    def __init__(
        self,
        *,
        direct_merge: Callable[[str, str, os.stat_result], int],
        prune_opaque_narrow: Callable[[str, str], int] | None = None,
    ) -> None:
        self._direct_merge = direct_merge
        self._prune_opaque_narrow = prune_opaque_narrow or (lambda _rel, _up: 0)

    def apply(self, plan: ClassificationPlan) -> int:
        direct_merged_bytes = 0
        for prune_op in plan.opaque_prune_ops:
            self._prune_opaque_narrow(prune_op.rel, prune_op.upper_path)
        for merge_op in plan.direct_merge_ops:
            direct_merged_bytes += self._direct_merge(
                merge_op.rel, merge_op.upper_path, merge_op.st
            )
        return direct_merged_bytes


def direct_merge_factory(
    *, live_root: str
) -> Callable[[str, str, os.stat_result], int]:
    """Return a callable that atomically merges one upperdir file into live."""

    def _merge(rel: str, upper_path: str, upper_st: os.stat_result) -> int:
        del upper_st
        live_target = os.path.join(live_root, rel)
        parent = os.path.dirname(live_target)
        if parent:
            os.makedirs(parent, exist_ok=True)
        fd, tmp_path = tempfile.mkstemp(
            dir=parent or ".",
            prefix=os.path.basename(live_target) + ".",
            suffix=".overlay-merge",
        )
        os.close(fd)
        try:
            shutil.copyfile(upper_path, tmp_path)
            os.rename(tmp_path, live_target)
        except OSError:
            try:
                os.unlink(tmp_path)
            except OSError:
                pass
            raise
        try:
            return os.path.getsize(live_target)
        except OSError:
            return 0

    return _merge


def narrow_prune_opaque_factory(*, live_root: str) -> Callable[[str, str], int]:
    """Return a callable that narrow-prunes a gitignored opaque directory."""

    def _prune(rel: str, upper_dir: str) -> int:
        live_path = os.path.join(live_root, rel)
        try:
            live_st = os.lstat(live_path)
        except FileNotFoundError:
            return 0
        if not stat.S_ISDIR(live_st.st_mode) or stat.S_ISLNK(live_st.st_mode):
            return 0
        try:
            upper_children = set(os.listdir(upper_dir))
        except FileNotFoundError:
            upper_children = set()
        try:
            live_children = os.listdir(live_path)
        except FileNotFoundError:
            return 0
        pruned = 0
        for name in live_children:
            if name in upper_children:
                continue
            child = os.path.join(live_path, name)
            try:
                child_st = os.lstat(child)
            except FileNotFoundError:
                continue
            try:
                if stat.S_ISLNK(child_st.st_mode) or not stat.S_ISDIR(
                    child_st.st_mode
                ):
                    os.unlink(child)
                else:
                    shutil.rmtree(child)
            except OSError as exc:
                raise ClassifierIOError(
                    f"narrow-prune failed for {child!r}: {exc}"
                ) from exc
            pruned += 1
        return pruned

    return _prune


__all__ = [
    "DirectRouteApplier",
    "direct_merge_factory",
    "narrow_prune_opaque_factory",
]
