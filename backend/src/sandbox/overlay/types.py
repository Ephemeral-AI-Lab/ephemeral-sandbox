"""Dataclasses and exceptions for overlay upperdir capture."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Literal


UpperChangeKind = Literal["regular", "whiteout", "symlink", "opaque_dir"]


class OverlayError(RuntimeError):
    """Base error for overlay auditing failures."""


class OverlayRunError(OverlayError):
    """Raised when the sandbox-side overlay runtime transport fails."""


class OverlayPolicyReject(OverlayError):
    """Raised when the sandbox-side script refused the run via policy.

    ``reason`` is one of the overlay structural reject reasons, e.g.
    ``overlay_upper_full``.
    ``paths`` is the (optional) offending path list.
    """

    def __init__(
        self,
        reason: str,
        paths: tuple[str, ...] = (),
        *,
        run_timings: dict[str, float] | None = None,
    ) -> None:
        super().__init__(reason if not paths else f"{reason}: {','.join(paths)}")
        self.reason = reason
        self.paths = paths
        self.run_timings = dict(run_timings or {})


@dataclass(frozen=True)
class OverlayLease:
    """One per-op overlay lease.

    The overlay model (see plan §0, "Mount model") has no pool — each
    ``svc.cmd`` builds a fresh unshare namespace with fresh mounts and
    tears it all down on exit. The lease is just the per-op run
    directory on the container filesystem (outside the overlay so it
    survives ns exit) that holds ``diff.ndjson``.
    """

    run_dir: str


@dataclass(frozen=True)
class UpperChange:
    """One raw upperdir change emitted by the overlay runtime for OCC."""

    rel: str
    kind: UpperChangeKind
    base_bytes: bytes | None
    upper_bytes: bytes | None
    base_existed: bool


@dataclass(frozen=True)
class OverlayCapture:
    """Parsed ``diff.ndjson`` payload after one overlay op."""

    exit_code: int
    upper_bytes: int
    upper_files: int
    upper_changes: tuple[UpperChange, ...]
    run_timings: dict[str, float] = field(default_factory=dict)
    warnings: tuple[str, ...] = ()


@dataclass(frozen=True)
class ConflictInfo:
    """Structured failure surface for the overlay-to-caller boundary."""

    reason: str
    conflict_file: str | None = None
    message: str = ""


@dataclass
class OverlayRunOutcome:
    """Capture-run handoff between overlay and its caller.

    Overlay produces raw :attr:`upper_changes`; the caller drives merge
    policy. Not ``frozen`` because ``overlay_stage_timings`` is set after
    lease cleanup.
    """

    exit_code: int
    stdout: str
    upper_changes: tuple[UpperChange, ...]
    overlay_rejected: bool
    conflict: ConflictInfo | None
    warnings: tuple[str, ...] = ()
    overlay_run_timings: dict[str, float] = field(default_factory=dict)
    overlay_stage_timings: dict[str, float] = field(default_factory=dict)
    policy_reject: OverlayPolicyReject | None = None


@dataclass(frozen=True)
class ShellResult:
    """Runtime shell result after overlay capture and OCC projection."""

    result: str
    exit_code: int
    changed_paths: tuple[str, ...] = ()
    warnings: tuple[str, ...] = ()
    overlay_run_timings: dict[str, float] = field(default_factory=dict)
    overlay_stage_timings: dict[str, float] = field(default_factory=dict)
    conflict: ConflictInfo | None = None


__all__ = [
    "ConflictInfo",
    "OverlayCapture",
    "OverlayError",
    "OverlayLease",
    "OverlayPolicyReject",
    "OverlayRunError",
    "OverlayRunOutcome",
    "ShellResult",
    "UpperChange",
    "UpperChangeKind",
]
