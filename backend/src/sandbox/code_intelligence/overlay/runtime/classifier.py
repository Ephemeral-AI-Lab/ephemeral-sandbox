"""Pure overlay upperdir classifier."""

from __future__ import annotations

import os
import stat
from collections.abc import Callable, Iterable

from .direct_routes import DirectRouteApplier
from .overlay_kinds import is_opaque_dir, is_symlink, is_whiteout
from .policy import (
    REJECT_GITIGNORE_WHITEOUT,
    REJECT_NON_UTF8_GITINCLUDE,
    REJECT_UNSUPPORTED_OPAQUE_DIR,
    REJECT_UNSUPPORTED_SYMLINK,
    filter_ignorable_dotgit_writes,
    reject_dotgit_writes,
)
from .types import (
    ClassificationPlan,
    ClassifierIOError,
    ClassifyOutcome,
    DirectMergeOp,
    GitincludeChange,
    OpaquePruneOp,
    PolicyRejectOutcome,
    UpperEntry,
)


class Classifier:
    """Classify one upperdir walk into gitinclude / gitignore / rejects."""

    def __init__(
        self,
        *,
        read_upper_bytes: Callable[[str], bytes],
        git_show_base: Callable[[str], bytes | None],
        check_ignore: Callable[[list[str]], set[str]],
        direct_merge: Callable[[str, str, os.stat_result], int] | None = None,
        prune_opaque_narrow: Callable[[str, str], int] | None = None,
    ) -> None:
        self._read_upper_bytes = read_upper_bytes
        self._git_show_base = git_show_base
        self._check_ignore = check_ignore
        self._direct_route_applier = (
            DirectRouteApplier(
                direct_merge=direct_merge,
                prune_opaque_narrow=prune_opaque_narrow,
            )
            if direct_merge is not None
            else None
        )

    def classify(
        self, entries: Iterable[UpperEntry]
    ) -> ClassifyOutcome | PolicyRejectOutcome:
        plan = self.classify_plan(entries)
        if isinstance(plan, PolicyRejectOutcome):
            return plan
        if self._direct_route_applier is None:
            if not plan.direct_merge_ops and not plan.opaque_prune_ops:
                return plan.to_outcome(direct_merged_bytes=0)
            raise RuntimeError(
                "Classifier.classify requires a direct_merge callback; "
                "use classify_plan() for side-effect-free classification"
            )
        direct_merged_bytes = self._direct_route_applier.apply(plan)
        return plan.to_outcome(direct_merged_bytes=direct_merged_bytes)

    def classify_plan(
        self, entries: Iterable[UpperEntry]
    ) -> ClassificationPlan | PolicyRejectOutcome:
        entries = list(entries)

        dotgit_reject = reject_dotgit_writes(entries)
        if dotgit_reject is not None:
            return dotgit_reject
        entries = filter_ignorable_dotgit_writes(entries)

        whiteouts: list[UpperEntry] = []
        regular: list[UpperEntry] = []
        opaque_dirs: list[UpperEntry] = []
        symlinks: list[UpperEntry] = []

        for entry in entries:
            if is_whiteout(entry.st, entry.xattrs):
                whiteouts.append(entry)
            elif is_symlink(entry.st):
                symlinks.append(entry)
            elif is_opaque_dir(entry.st, entry.xattrs):
                opaque_dirs.append(entry)
            elif stat.S_ISREG(entry.st.st_mode):
                regular.append(entry)

        candidate_entries = whiteouts + regular + opaque_dirs + symlinks
        candidates_wire = [
            (entry.rel + "/") if stat.S_ISDIR(entry.st.st_mode) else entry.rel
            for entry in candidate_entries
        ]
        ignored_wire = (
            self._check_ignore(candidates_wire) if candidates_wire else set()
        )
        ignored = {path.rstrip("/") for path in ignored_wire}

        bad_symlinks = [entry.rel for entry in symlinks if entry.rel not in ignored]
        if bad_symlinks:
            return PolicyRejectOutcome(
                reason=REJECT_UNSUPPORTED_SYMLINK,
                paths=tuple(sorted(bad_symlinks)),
            )
        bad_opaque = [entry.rel for entry in opaque_dirs if entry.rel not in ignored]
        if bad_opaque:
            return PolicyRejectOutcome(
                reason=REJECT_UNSUPPORTED_OPAQUE_DIR,
                paths=tuple(sorted(bad_opaque)),
            )
        bad_gitignore_whiteout = [
            entry.rel for entry in whiteouts if entry.rel in ignored
        ]
        if bad_gitignore_whiteout:
            return PolicyRejectOutcome(
                reason=REJECT_GITIGNORE_WHITEOUT,
                paths=tuple(sorted(bad_gitignore_whiteout)),
            )

        gitinclude: list[GitincludeChange] = []
        gitignore_paths: list[str] = []
        direct_merge_ops: list[DirectMergeOp] = []
        opaque_prune_ops: list[OpaquePruneOp] = []
        whiteouts_gitinclude = 0

        for entry in opaque_dirs:
            if entry.rel not in ignored:
                continue
            opaque_prune_ops.append(
                OpaquePruneOp(rel=entry.rel, upper_path=entry.upper_path)
            )
            gitignore_paths.append(entry.rel)

        for entry in whiteouts:
            if entry.rel in ignored:
                continue
            whiteouts_gitinclude += 1
            base_bytes = self._git_show_base(entry.rel)
            base_existed = base_bytes is not None
            try:
                base_text = (base_bytes or b"").decode("utf-8")
            except UnicodeDecodeError:
                return PolicyRejectOutcome(
                    reason=REJECT_NON_UTF8_GITINCLUDE, paths=(entry.rel,)
                )
            gitinclude.append(
                GitincludeChange(
                    path=entry.rel,
                    kind="delete",
                    base_content=base_text,
                    base_existed=base_existed,
                    final_content=None,
                )
            )

        for entry in regular:
            if entry.rel in ignored:
                gitignore_paths.append(entry.rel)
                direct_merge_ops.append(
                    DirectMergeOp(
                        rel=entry.rel,
                        upper_path=entry.upper_path,
                        st=entry.st,
                    )
                )
                continue

            try:
                upper_bytes = self._read_upper_bytes(entry.rel)
            except OSError as exc:
                raise ClassifierIOError(
                    f"upperdir read failed for {entry.rel!r}: {exc}"
                ) from exc
            base_bytes = self._git_show_base(entry.rel)
            base_existed = base_bytes is not None
            if base_existed and upper_bytes == base_bytes:
                continue

            try:
                final_text = upper_bytes.decode("utf-8")
            except UnicodeDecodeError:
                return PolicyRejectOutcome(
                    reason=REJECT_NON_UTF8_GITINCLUDE, paths=(entry.rel,)
                )
            try:
                base_text = (base_bytes or b"").decode("utf-8")
            except UnicodeDecodeError:
                return PolicyRejectOutcome(
                    reason=REJECT_NON_UTF8_GITINCLUDE, paths=(entry.rel,)
                )
            kind = "modify" if base_existed else "create"
            gitinclude.append(
                GitincludeChange(
                    path=entry.rel,
                    kind=kind,
                    base_content=base_text,
                    base_existed=base_existed,
                    final_content=final_text,
                )
            )

        return ClassificationPlan(
            gitinclude=tuple(gitinclude),
            gitignore_paths=tuple(sorted(gitignore_paths)),
            direct_merge_ops=tuple(direct_merge_ops),
            opaque_prune_ops=tuple(opaque_prune_ops),
            whiteouts_gitinclude=whiteouts_gitinclude,
            whiteouts_gitignore_refused=0,
            dotgit_rejects=0,
        )


__all__ = ["Classifier"]
