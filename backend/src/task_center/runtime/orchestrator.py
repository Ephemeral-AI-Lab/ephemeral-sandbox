"""Orchestrator — graph-scoped facade for the four-role / recursive design.

Every :class:`HarnessGraph` has exactly one orchestrator. The orchestrator is
a transient frozen-dataclass view bound to a ``graph_id`` and a
:class:`TaskCenter` reference; it has no state of its own.

There are two ways to obtain an orchestrator:

1. :meth:`Orchestrator.spawn` — opens a *new* graph + planner, returns the
   orchestrator for it. Side-effecting.
2. ``Orchestrator(graph_id, tc)`` — pure view of an *existing* graph.

Stage 1 of the four-role / recursive-orchestrator restructure introduced the
class with ``spawn`` and the read accessors. Stage 3 adds
``materialize_full_plan`` / ``materialize_partial_plan`` with validation;
``MaterializationFailure`` is the structured rejection payload.

Backward compatibility: the prior location of :class:`TaskCenter` was this
module. Stage 1 keeps a re-export so existing imports
(``from task_center.runtime.orchestrator import TaskCenter``) keep working.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, get_args

from task_center.model import (
    GeneratorRole,
    HarnessGraph,
    HarnessGraphId,
    Status,
    Task,
    TaskId,
)
from task_center.runtime.task_center import SpawnFunc, TaskCenter


# ---------------------------------------------------------------------------- #
# Materialization failure — structured rejection of submit_full_plan /         #
# submit_partial_plan. Returned (not raised) so the runtime dispatcher can     #
# forward the failure to the agent as a tool-result failure for retry.        #
# ---------------------------------------------------------------------------- #


@dataclass(frozen=True)
class MaterializationFailure:
    """Why a planner DAG was rejected.

    ``code`` is one of:
      - ``empty_dag`` — DAG must contain at least one generator
      - ``duplicate_ids`` — duplicate node ids in the DAG
      - ``missing_details`` — task_details keys must match DAG ids exactly
      - ``unknown_role`` — role is not a generator role
      - ``unknown_dep`` — node references an unknown dep id
      - ``cycle`` — DAG contains a cycle
      - ``verifier_sink`` — a verifier node sits at a DAG sink
    """

    code: str
    message: str


@dataclass(frozen=True)
class Orchestrator:
    """Graph-scoped facade for one :class:`HarnessGraph`."""

    graph_id: HarnessGraphId
    tc: TaskCenter

    # ------------------------------------------------------------------ #
    # Construction                                                       #
    # ------------------------------------------------------------------ #

    @classmethod
    def spawn(
        cls,
        tc: TaskCenter,
        *,
        root_task_id: TaskId,
        request_plan_note: str,
        prior_graph_id: HarnessGraphId | None = None,
    ) -> "Orchestrator":
        """Open a new HarnessGraph + spawn its planner READY."""
        planner_id = tc._new_id()
        graph = tc._open_graph(
            root_task_id=root_task_id,
            planner_id=planner_id,
            request_plan_note=request_plan_note,
            prior_graph_id=prior_graph_id,
        )
        tc._create_planner(
            input=request_plan_note,
            harness_graph_id=graph.id,
            id=planner_id,
        )
        return cls(graph_id=graph.id, tc=tc)

    # ------------------------------------------------------------------ #
    # Read accessors                                                     #
    # ------------------------------------------------------------------ #

    @property
    def graph(self) -> HarnessGraph:
        return self.tc.graph.get_harness_graph(self.graph_id)

    @property
    def root_task(self) -> Task:
        return self.tc.graph.get(self.graph.root_task_id)

    @property
    def planner(self) -> Task:
        return self.tc.graph.get(self.graph.planner)

    @property
    def evaluator(self) -> Task | None:
        eid = self.graph.evaluator
        if eid is None:
            return None
        return self.tc.graph.get(eid)

    @property
    def dag_nodes(self) -> list[Task]:
        return [self.tc.graph.get(nid) for nid in self.graph.dag_nodes]

    # ------------------------------------------------------------------ #
    # Stage 3 — DAG materialization                                      #
    # ------------------------------------------------------------------ #

    def materialize_full_plan(
        self,
        task_dep_graphs: list[dict[str, Any]],
        task_details: dict[str, str],
        evaluation_specification: str,
    ) -> MaterializationFailure | None:
        """Validate a full-plan DAG and create its generator children + evaluator.

        Returns ``None`` on success and a :class:`MaterializationFailure`
        on validation failure (no graph mutation occurs in that case).
        """
        err = _validate_plan_dag(task_dep_graphs, task_details)
        if err is not None:
            return err
        self._materialize_dag(
            task_dep_graphs, task_details, evaluation_specification
        )
        graph = self.graph
        graph.plan_shape = "full"
        self.tc._persist_all()
        self.tc._wakeup.set()
        return None

    def materialize_partial_plan(
        self,
        task_dep_graphs: list[dict[str, Any]],
        task_details: dict[str, str],
        what_to_do_next: str,
        evaluation_specification: str,
    ) -> MaterializationFailure | None:
        """Validate a partial-plan DAG and create its children + evaluator.

        Same as :meth:`materialize_full_plan` plus stores ``what_to_do_next``
        on the harness graph and marks ``plan_shape='partial'`` for the
        Stage 5 continuation chain to pick up.
        """
        err = _validate_plan_dag(task_dep_graphs, task_details)
        if err is not None:
            return err
        self._materialize_dag(
            task_dep_graphs, task_details, evaluation_specification
        )
        graph = self.graph
        graph.plan_shape = "partial"
        graph.what_to_do_next = what_to_do_next
        self.tc._persist_all()
        self.tc._wakeup.set()
        return None

    def _materialize_dag(
        self,
        task_dep_graphs: list[dict[str, Any]],
        task_details: dict[str, str],
        evaluation_specification: str,
    ) -> None:
        """Common body for both materialization paths.

        Assumes the DAG has already been validated by ``_validate_plan_dag``.
        Creates one Task per generator entry, then auto-spawns the evaluator
        with ``needs = sinks(DAG)``. The planner transitions to HANDOFF.
        """
        graph = self.graph
        planner = self.tc.graph.get(graph.planner)
        # Stage 3 scope: keep the legacy ``handoff_plan_note`` /
        # ``evaluator_note`` slots in sync with the new spec where
        # callers want them. The new terminal does not pass a separate
        # plan note; the evaluator's input is the evaluation_specification.
        graph.evaluator_note = evaluation_specification
        self.tc.graph.transition(planner.id, Status.HANDOFF)

        # Per-node creation in topological order so ``needs`` resolves
        # against already-existing Task ids.
        deps_map: dict[str, frozenset[TaskId]] = {
            entry["id"]: frozenset(entry.get("deps", []))
            for entry in task_dep_graphs
        }
        sink_ids = _compute_sinks(task_dep_graphs)

        for nid in _topo_sort(task_dep_graphs):
            entry = next(e for e in task_dep_graphs if e["id"] == nid)
            role = entry.get("role", "executor")
            child_status = Status.READY if not deps_map[nid] else Status.PENDING
            primitive = (
                self.tc._create_executor
                if role == "executor"
                else self.tc._create_verifier
            )
            child = primitive(
                input=task_details[nid],
                harness_graph_id=graph.id,
                needs=deps_map[nid],
                status=child_status,
                id=nid,
            )
            graph.dag_nodes.append(child.id)
            graph.executor_task_ids.append(child.id)

        evaluator_id = f"{planner.id}-eval"
        evaluator = self.tc._create_evaluator(
            input=evaluation_specification,
            harness_graph_id=graph.id,
            needs=frozenset(sink_ids),
            id=evaluator_id,
        )
        graph.evaluator = evaluator.id
        graph.evaluator_task_id = evaluator.id

    # ------------------------------------------------------------------ #
    # Stage 5 — Partial-plan continuation chain                          #
    # ------------------------------------------------------------------ #

    def close_partial_success(self, summary: str) -> "Orchestrator":
        """Close a partial-plan graph successfully and spawn the continuation.

        Pre-condition: the evaluator's terminal handler has already marked
        the evaluator DONE. This method:

        - marks the planner DONE,
        - appends a ``segment_success`` summary to the graph's root task
          (which **stays in HANDOFF** — the chain is not yet terminal),
        - spawns the continuation graph rooted at the same root task,
          carrying ``prior_graph_id=self.graph_id``.

        Returns the new continuation orchestrator.
        """
        from task_center.model import TaskSummary  # local: avoid module cycle

        graph = self.graph
        root_task = self.root_task
        planner = self.planner

        self.tc._mark_terminal(planner, Status.DONE)
        root_task.summaries.append(
            TaskSummary(
                kind="segment_success",
                text=summary,
                source_task_id=graph.evaluator if graph.evaluator else planner.id,
            )
        )
        # root_task stays in HANDOFF — explicitly NOT transitioned. The
        # continuation graph that follows will eventually close the chain
        # via ``close_harness_graph_success`` (Stage 5 reuses the existing
        # full-plan closure for that final hop).
        continuation = Orchestrator.spawn(
            self.tc,
            root_task_id=graph.root_task_id,
            request_plan_note=self.build_continuation_note(),
            prior_graph_id=self.graph_id,
        )
        self.tc._persist_all()
        self.tc._wakeup.set()
        return continuation

    def build_continuation_note(self) -> str:
        """Walk the chain via ``prior_graph_id`` and assemble the note.

        Format mirrors design doc §8.4::

            ROOT_GOAL: {root_task.input}
            PRIOR SEGMENTS:
              [each prior graph's what_to_do_next + evaluator success summary]
            CURRENT REQUEST:
              {graph.what_to_do_next}
        """
        graph = self.graph
        root_task = self.root_task

        chain: list[HarnessGraph] = []
        current_id = graph.prior_graph_id
        while current_id is not None:
            prior = self.tc.graph.get_harness_graph(current_id)
            chain.append(prior)
            current_id = prior.prior_graph_id
        chain.reverse()  # oldest first

        parts = [f"ROOT_GOAL: {root_task.input}"]
        if chain:
            parts.append("PRIOR SEGMENTS:")
            for i, prior in enumerate(chain, start=1):
                eval_summary = ""
                if prior.evaluator is not None:
                    ev = self.tc.graph.get(prior.evaluator)
                    if ev.summaries:
                        eval_summary = ev.summaries[-1].text
                directive = prior.what_to_do_next or "(no follow-up directive)"
                parts.append(
                    f"  Segment {i}: {directive}\n"
                    f"    Evaluator summary: {eval_summary}"
                )
        parts.append(
            f"CURRENT REQUEST: {graph.what_to_do_next or '(no directive)'}"
        )
        return "\n".join(parts)

    # ------------------------------------------------------------------ #
    # Stage 6/7 stubs (filled by later stages)                           #
    # ------------------------------------------------------------------ #

    def create_harness_fix_executor(
        self,
        verifier_id: TaskId,
        failure_summary: str,
    ) -> None:
        raise NotImplementedError(
            "create_harness_fix_executor lands in Stage 6 (fix-executor)."
        )

    def close_success(self, summary: str) -> None:
        raise NotImplementedError(
            "Orchestrator.close_success lands in Stage 7 (evaluator narrows). "
            "Until then, evaluator_lifecycle.close_harness_graph_success handles closure."
        )

    def close_failure(self, summary: str) -> None:
        raise NotImplementedError(
            "Orchestrator.close_failure lands in Stage 7 (closure rewire)."
        )


# ---------------------------------------------------------------------------- #
# Validation helpers                                                            #
# ---------------------------------------------------------------------------- #


def _validate_plan_dag(
    task_dep_graphs: list[dict[str, Any]],
    task_details: dict[str, str],
) -> MaterializationFailure | None:
    """Stage 3 validation matrix per design doc §9.3."""
    ids = [entry.get("id") for entry in task_dep_graphs]

    if not ids:
        return MaterializationFailure(
            code="empty_dag",
            message="DAG must contain at least one generator",
        )
    if len(set(ids)) != len(ids):
        return MaterializationFailure(
            code="duplicate_ids",
            message="duplicate node ids in DAG",
        )
    if set(ids) != set(task_details.keys()):
        return MaterializationFailure(
            code="missing_details",
            message="task_details keys must match DAG ids exactly",
        )

    id_set = set(ids)
    allowed_roles = set(get_args(GeneratorRole))
    for entry in task_dep_graphs:
        role = entry.get("role", "executor")
        if role not in allowed_roles:
            return MaterializationFailure(
                code="unknown_role",
                message=(
                    f"node {entry['id']!r} role={role!r} is not a generator "
                    f"role (allowed: {sorted(allowed_roles)})"
                ),
            )
        deps = entry.get("deps", [])
        if not set(deps).issubset(id_set):
            unknown = sorted(set(deps) - id_set)
            return MaterializationFailure(
                code="unknown_dep",
                message=(
                    f"node {entry['id']!r} references unknown dep ids "
                    f"{unknown!r}"
                ),
            )

    if _has_cycle(task_dep_graphs):
        return MaterializationFailure(
            code="cycle",
            message="DAG contains a cycle",
        )

    sinks = _compute_sinks(task_dep_graphs)
    bad = [
        nid
        for nid in sinks
        if next(e for e in task_dep_graphs if e["id"] == nid).get("role")
        == "verifier"
    ]
    if bad:
        return MaterializationFailure(
            code="verifier_sink",
            message=(
                f"verifier nodes cannot be DAG sinks: {sorted(bad)!r} — "
                "verifier success must unblock something downstream, otherwise "
                "the auto-spawned evaluator gates the same scope twice"
            ),
        )

    return None


def _has_cycle(task_dep_graphs: list[dict[str, Any]]) -> bool:
    white, gray, black = 0, 1, 2
    color: dict[str, int] = {entry["id"]: white for entry in task_dep_graphs}
    deps_map: dict[str, list[str]] = {
        entry["id"]: list(entry.get("deps", [])) for entry in task_dep_graphs
    }

    def visit(nid: str) -> bool:
        color[nid] = gray
        for dep in deps_map.get(nid, []):
            if dep not in color:
                # Filtered out by unknown_dep validation already; treat as
                # absent here.
                continue
            if color[dep] == gray:
                return True
            if color[dep] == white and visit(dep):
                return True
        color[nid] = black
        return False

    return any(visit(nid) for nid in list(color) if color[nid] == white)


def _compute_sinks(task_dep_graphs: list[dict[str, Any]]) -> list[str]:
    """Sinks are nodes that no other node depends on."""
    depended_upon: set[str] = set()
    for entry in task_dep_graphs:
        depended_upon.update(entry.get("deps", []))
    return [
        entry["id"]
        for entry in task_dep_graphs
        if entry["id"] not in depended_upon
    ]


def _topo_sort(task_dep_graphs: list[dict[str, Any]]) -> list[str]:
    """Kahn's algorithm — assumes acyclic input (guaranteed by validation)."""
    deps_map: dict[str, set[str]] = {
        entry["id"]: set(entry.get("deps", [])) for entry in task_dep_graphs
    }
    out: list[str] = []
    ready = [nid for nid, deps in deps_map.items() if not deps]
    remaining = {nid: set(deps) for nid, deps in deps_map.items()}
    while ready:
        ready.sort()  # stable order for tests
        nid = ready.pop(0)
        out.append(nid)
        for other_nid, other_deps in remaining.items():
            if nid in other_deps:
                other_deps.discard(nid)
                if not other_deps and other_nid not in out and other_nid not in ready:
                    ready.append(other_nid)
    return out


__all__ = [
    "MaterializationFailure",
    "Orchestrator",
    "SpawnFunc",
    "TaskCenter",
]
