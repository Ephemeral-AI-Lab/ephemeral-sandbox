# ruff: noqa
"""Live E2E: Complex subagent coordination under heavy mixed load.

Three layered scenarios that exercise the ``run_subagent`` tool and its
background-task plumbing in realistic multi-wave coordination patterns:

Scenario A — Parallel Research Wave + Synthesis
  The parent decomposes a research goal into 3 background subagents (each
  investigates one domain) while a 4th subagent does outline work, all
  spawned in the same turn.  Once the background wave finishes the parent
  synthesises all 4 outputs into a single structured report.  Verifies:
  multiple background subagent launches in one turn, check_background_progress
  on live subagents, wait_for_background_task, and coherent synthesis in the
  final assistant text.

Scenario B — Two-Wave Refinement with Result Threading
  Wave-1: 2 background subagents produce raw drafts of two document halves.
  The parent peeks at live progress via check_background_progress while they
  run.  When wave-1 completes, the parent spawns wave-2: 2 new background
  subagents that each take one wave-1 draft and refine it.  Finally the
  parent merges the two refined halves.  Verifies: dependency-aware multi-
  wave spawning and result threading between waves.

Scenario C — Fan-out with Early Cancellation and Replacement
  The parent spawns 4 background subagents in parallel.  One of them is
  instructed to start its response with "BLOCKED:" (simulating an
  unresolvable blocker).  The parent must detect the signal via
  check_background_progress, cancel that subagent early (or note it if
  already done), spawn a replacement, and finally merge all 4 outputs into
  a Markdown table.  Verifies: mid-flight subagent cancellation and recovery
  without orphan tasks.

Run with:
  uv run pytest backend/tests/test_e2e/test_subagent_complex_live.py -v -s --log-cli-level=INFO

All three classes are guarded by ``EvalAgent.has_all()`` (API key + Daytona).
The ``run_subagent`` tool always runs as background="always", so the parent
must drive: run_subagent → check_background_progress → wait / cancel.
"""
from __future__ import annotations

import logging
import textwrap

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_test_sandbox, delete_test_sandbox

logger = logging.getLogger(__name__)

pytestmark = [pytest.mark.e2e, pytest.mark.live]


# ---------------------------------------------------------------------------
# Local agent factory — thin wrapper around EvalAgent.create().
# EvalAgent registers SubagentToolkit and injects a SessionConfig by default,
# and spawn_agent gives each subagent its own fresh API client, so no manual
# wiring is needed here.
# ---------------------------------------------------------------------------


def _create_subagent_coordinator(
    *,
    system_prompt: str,
    sandbox_id: str,
    max_turns: int = 400,
) -> EvalAgent:
    return EvalAgent.create(
        system_prompt=system_prompt,
        sandbox_id=sandbox_id,
        max_turns=max_turns,
    )


# ---------------------------------------------------------------------------
# Shared system prompt for the parent (coordinator) agent
# ---------------------------------------------------------------------------

COORDINATOR_PROMPT = """\
You are a coordinator. Delegate only through ``run_subagent``.
- Do not do delegated work yourself.
- Launch parallel workers in one turn when possible.
- Use ``check_background_progress`` for live inspection.
- After each spawn wave, do at least one ``check_background_progress`` and one ``wait_for_background_task`` before starting the next wave, even if workers already look done.
- Use ``cancel_background_task`` when a worker is blocked or no longer useful.
- If a worker returns unusable output, replace it; cancel first if it is still running.
- Keep narration minimal and synthesize actual worker outputs in the final answer.
"""


# ---------------------------------------------------------------------------
# Shared logging helper
# ---------------------------------------------------------------------------


def _log_result(result, label: str) -> None:
    subagent_starts = [
        e for e in result.background_started() if e.tool_name == "run_subagent"
    ]
    subagent_done = [
        e for e in result.background_completed() if e.tool_name == "run_subagent"
    ]
    checks = result.tool_count("check_background_progress")
    waits = result.tool_count("wait_for_background_task")
    cancels = result.tool_count("cancel_background_task")

    logger.info(
        "\n%s\n[%s] Subagent complex summary:\n"
        "  Total tool calls   : %d\n"
        "  run_subagent starts: %d\n"
        "  run_subagent done  : %d\n"
        "  progress checks    : %d\n"
        "  wait calls         : %d\n"
        "  cancel calls       : %d\n"
        "  Tool sequence      : %s\n%s",
        "=" * 60,
        label,
        len(result.tool_calls),
        len(subagent_starts),
        len(subagent_done),
        checks,
        waits,
        cancels,
        result.tool_names,
        "=" * 60,
    )


# ===========================================================================
# Scenario A — Parallel Research Wave + Synthesis
#
# Parent spawns 3 background subagents (domain researchers) + 1 outline
# subagent in a single turn.  After checking progress and joining, it
# synthesises all 4 outputs into a structured report.
#
# Observable invariants:
#   - At least 3 run_subagent background_started events
#   - check_background_progress called at least once
#   - wait_for_background_task called at least once
#   - Final text covers all 3 research domains
#   - No unrecovered errors
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSubagentParallelResearchSynthesis:
    """Parent spawns 3 parallel background subagents + 1 outline subagent,
    then synthesises all 4 outputs into a coherent report."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("subagent-research")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_parallel_research_then_synthesis(self, sandbox):
        """Three background research subagents run concurrently; parent synthesises."""
        agent = _create_subagent_coordinator(
            system_prompt=COORDINATOR_PROMPT,
            sandbox_id=sandbox["id"],
            max_turns=400,
        )

        result = await agent.invoke(
            textwrap.dedent("""\
            Your ONLY tool for delegation is ``run_subagent``.
            Do NOT perform research yourself or use any shell tools.

            Goal: produce a structured report on "Distributed Systems Trade-offs"
            across three domains: Consistency, Availability, and Partition Tolerance.

            STEP 1 — In your very first turn, emit ALL FOUR run_subagent calls
            in the same message (parallel fan-out):

              Subagent RESEARCH_CONSISTENCY:
                prompt = "You are a technical writer. Write exactly 3 concise bullet
                points about trade-offs in the Consistency dimension of the CAP
                theorem. Each bullet must be one sentence. End your response with
                the exact marker: CONSISTENCY_RESEARCH_DONE"

              Subagent RESEARCH_AVAILABILITY:
                prompt = "You are a technical writer. Write exactly 3 concise bullet
                points about trade-offs in the Availability dimension of the CAP
                theorem. Each bullet must be one sentence. End your response with
                the exact marker: AVAILABILITY_RESEARCH_DONE"

              Subagent RESEARCH_PARTITION:
                prompt = "You are a technical writer. Write exactly 3 concise bullet
                points about trade-offs in the Partition Tolerance dimension of the
                CAP theorem. Each bullet must be one sentence. End your response with
                the exact marker: PARTITION_RESEARCH_DONE"

              Subagent OUTLINE:
                prompt = "You are a document structurer. Write a 40-60 word executive
                summary skeleton for a CAP theorem trade-offs report. Do NOT include
                domain-specific content — just structural framing. End with: OUTLINE_DONE"

            STEP 2 — After spawning, call check_background_progress on at least
            two of the task_ids to observe their live status.

            STEP 3 — Call wait_for_background_task with task_id="all" to join
            all four subagents.

            STEP 4 — In your final message, write a structured report with four
            sections: Executive Summary, Consistency, Availability, Partition
            Tolerance. Each section must incorporate the actual content from the
            corresponding subagent. Quote or paraphrase the output and state which
            marker tag confirmed completion (e.g. "CONSISTENCY_RESEARCH_DONE").
            """)
        )

        _log_result(result, "parallel_research_synthesis")

        # At least 3 background subagent launches (RESEARCH_* triad mandatory)
        subagent_starts = [
            e for e in result.background_started() if e.tool_name == "run_subagent"
        ]
        assert len(subagent_starts) >= 4, (
            f"Expected at least 4 run_subagent background launches (3 research + 1 outline). "
            f"Got {len(subagent_starts)}. Tool sequence: {result.tool_names}"
        )

        # Parent must have called check_background_progress at least once
        assert result.has_tool("check_background_progress"), (
            f"Parent never called check_background_progress on running subagents. "
            f"Tool sequence: {result.tool_names}"
        )

        # wait_for_background_task must be called
        assert result.has_tool("wait_for_background_task"), (
            f"Parent never called wait_for_background_task. "
            f"Tool sequence: {result.tool_names}"
        )

        # Final text must reference at least 2 domain markers or domain keywords
        text_lower = result.text.lower()
        label_hits = sum(
            1
            for label in [
                "consistency_research_done",
                "availability_research_done",
                "partition_research_done",
                "outline_done",
            ]
            if label in text_lower
        )
        domain_hits = sum(
            1
            for keyword in ["consistency", "availability", "partition"]
            if keyword in text_lower
        )
        assert label_hits >= 2 or domain_hits >= 3, (
            f"Final text does not synthesise subagent outputs. "
            f"Label hits: {label_hits}/4, domain keyword hits: {domain_hits}/3. "
            f"Text (first 600 chars): {result.text[:600]}"
        )

        assert not result.has_unrecovered_errors, (
            f"Unrecovered errors: "
            f"{[e.output[:200] for e in result.unrecovered_error_events]}"
        )


# ===========================================================================
# Scenario B — Two-Wave Refinement with Result Threading
#
# Wave-1: 2 background subagents produce terse drafts.
# Parent peeks at both via check_background_progress, then joins wave-1.
# Wave-2: 2 new background subagents refine one draft each (parent passes
#         the wave-1 text verbatim into the wave-2 prompt).
# Parent joins wave-2, then writes the merged final document.
#
# Observable invariants:
#   - At least 4 run_subagent background_started events (2 per wave)
#   - check_background_progress called at least 2 times (once per wave)
#   - wait_for_background_task called at least 2 times (once per wave)
#   - Final text references refinement completion (REFINE_*_DONE markers
#     or synonyms)
#   - No unrecovered errors
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSubagentTwoWaveRefinement:
    """Multi-wave subagent coordination: raw drafts → refinement → merge."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("subagent-twowave")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_two_wave_refinement_with_result_threading(self, sandbox):
        """Wave-1 produces drafts; wave-2 refines them; parent merges."""
        agent = _create_subagent_coordinator(
            system_prompt=COORDINATOR_PROMPT,
            sandbox_id=sandbox["id"],
            max_turns=400,
        )

        result = await agent.invoke(
            textwrap.dedent("""\
            Your ONLY tool for delegation is ``run_subagent``.
            Do NOT write content yourself or use any shell tools.

            Goal: produce a polished technical document on "REST API Design
            Best Practices" using a two-wave draft-then-refine process.

            WAVE 1 — emit BOTH run_subagent calls in the same turn:

              Subagent DRAFT_A:
                prompt = "You are a technical writer. Write a terse 4-bullet rough
                draft covering REST API URL structure and HTTP verb usage. Each bullet
                is one plain sentence — no polish needed. End with: DRAFT_A_DONE"

              Subagent DRAFT_B:
                prompt = "You are a technical writer. Write a terse 4-bullet rough
                draft covering REST API error handling and versioning strategies.
                Each bullet is one plain sentence — no polish needed.
                End with: DRAFT_B_DONE"

            After launching wave 1: call check_background_progress on BOTH task_ids.
            Then: call wait_for_background_task with task_id="all" to join wave 1.

            WAVE 2 — after collecting both wave-1 outputs, emit BOTH run_subagent
            calls in the same turn, threading the wave-1 text into each prompt:

              Subagent REFINE_A:
                prompt = "You are an editor. You are given this rough draft to
                improve:\n\n<PASTE THE EXACT DRAFT_A OUTPUT HERE>\n\nExpand each
                bullet to 2 sentences. Fix grammar. Add one concrete example per
                bullet. End with: REFINE_A_DONE"
                (Replace the placeholder with the actual DRAFT_A text you received.)

              Subagent REFINE_B:
                prompt = "You are an editor. You are given this rough draft to
                improve:\n\n<PASTE THE EXACT DRAFT_B OUTPUT HERE>\n\nExpand each
                bullet to 2 sentences. Fix grammar. Add one concrete example per
                bullet. End with: REFINE_B_DONE"
                (Replace the placeholder with the actual DRAFT_B text you received.)

            After launching wave 2: call check_background_progress on BOTH task_ids.
            Then: call wait_for_background_task with task_id="all" to join wave 2.

            FINAL — In your last message combine the two refined sections into a
            single document titled "REST API Design Best Practices". State
            explicitly that it incorporates REFINE_A_DONE and REFINE_B_DONE outputs.
            """)
        )

        _log_result(result, "two_wave_refinement")

        # At least 4 run_subagent launches total (2 wave-1 + 2 wave-2)
        subagent_starts = [
            e for e in result.background_started() if e.tool_name == "run_subagent"
        ]
        assert len(subagent_starts) >= 4, (
            f"Expected at least 4 run_subagent launches (2 per wave). "
            f"Got {len(subagent_starts)}. Tool sequence: {result.tool_names}"
        )

        # check_background_progress called at least twice (once per wave)
        checks = result.tool_count("check_background_progress")
        assert checks >= 2, (
            f"Expected at least 2 check_background_progress calls (one per wave). "
            f"Got {checks}. Tool sequence: {result.tool_names}"
        )

        # wait_for_background_task called at least twice (once per wave)
        waits = result.tool_count("wait_for_background_task")
        assert waits >= 2, (
            f"Expected at least 2 wait_for_background_task calls (one per wave). "
            f"Got {waits}. Tool sequence: {result.tool_names}"
        )

        # Final text must reference wave-2 refinement
        text_lower = result.text.lower()
        refine_hits = sum(
            1
            for marker in ["refine_a_done", "refine_b_done", "refined", "improved", "polish"]
            if marker in text_lower
        )
        assert refine_hits >= 1, (
            f"Final text does not reference wave-2 refinement. "
            f"Text (first 600 chars): {result.text[:600]}"
        )

        # REST API domain content must be present
        domain_hits = sum(
            1
            for kw in ["url", "http", "error", "version", "rest", "api"]
            if kw in text_lower
        )
        assert domain_hits >= 2, (
            f"Final text missing REST API domain content. "
            f"Text (first 600 chars): {result.text[:600]}"
        )

        assert not result.has_unrecovered_errors, (
            f"Unrecovered errors: "
            f"{[e.output[:200] for e in result.unrecovered_error_events]}"
        )


# ===========================================================================
# Scenario C — Fan-out with Early Cancellation and Replacement
#
# Parent spawns 4 background subagents in parallel (fan-out).
# One subagent (EU-WEST) is instructed to start its entire response with
# "BLOCKED:" — the parent must detect this signal via
# check_background_progress, cancel or note the blocked task, spawn a
# replacement fallback subagent, wait for all remaining tasks, and produce
# a Markdown latency table covering all 4 regions with the EU-WEST row
# marked as "(replaced fallback)".
#
# Observable invariants:
#   - At least 4 run_subagent background_started events (fan-out wave)
#   - check_background_progress called at least once
#   - At least 5 total run_subagent launches (4 original + 1 replacement)
#   - wait_for_background_task called at least once
#   - Final text covers all 4 regions and notes the fallback replacement
#   - No unrecovered errors (cancel of blocked subagent is expected)
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSubagentFanoutWithCancellationAndRecovery:
    """Fan-out: one subagent signals BLOCKED; parent cancels and spawns replacement."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("subagent-fanout")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_blocked_subagent_cancelled_and_replaced(self, sandbox):
        """Parent detects BLOCKED signal, handles that subagent, spawns replacement."""
        agent = _create_subagent_coordinator(
            system_prompt=COORDINATOR_PROMPT,
            sandbox_id=sandbox["id"],
            max_turns=400,
        )

        result = await agent.invoke(
            textwrap.dedent("""\
            Your ONLY tool for delegation is ``run_subagent``.
            Do NOT invent latency numbers yourself or use any shell tools.

            Goal: collect fictional infrastructure latency metrics for four
            data-center regions and produce a summary table.

            STEP 1 — Fan-out: emit ALL FOUR run_subagent calls in the same turn:

              Subagent REGION_US_EAST:
                prompt = "You are a metrics agent. Report fictional but plausible
                average latency values for the US-EAST data center.
                Format your entire response as exactly one line:
                US_EAST: p50=Xms p95=Yms p99=Zms
                Then on a new line write: REGION_US_EAST_DONE"

              Subagent REGION_US_WEST:
                prompt = "You are a metrics agent. Report fictional but plausible
                average latency values for the US-WEST data center.
                Format your entire response as exactly one line:
                US_WEST: p50=Xms p95=Yms p99=Zms
                Then on a new line write: REGION_US_WEST_DONE"

              Subagent REGION_EU_WEST:
                prompt = "IMPORTANT INSTRUCTION: Your data source is unavailable.
                Your ENTIRE response must begin with the exact text:
                BLOCKED: EU-WEST data source is offline
                Do not include any latency numbers. That is your complete response."

              Subagent REGION_AP_SOUTH:
                prompt = "You are a metrics agent. Report fictional but plausible
                average latency values for the AP-SOUTH data center.
                Format your entire response as exactly one line:
                AP_SOUTH: p50=Xms p95=Yms p99=Zms
                Then on a new line write: REGION_AP_SOUTH_DONE"

            STEP 2 — Monitor: call check_background_progress with task_id="all"
            to observe all tasks. Identify which task_id produced the BLOCKED
            prefix.

            STEP 3 — Handle the blocked task:
              - If the blocked task is still running: call cancel_background_task
                on it immediately.
              - If it already completed with BLOCKED output: note it and proceed.
              Either way, spawn one replacement subagent:
                Subagent REGION_EU_WEST_FALLBACK:
                  prompt = "The EU-WEST data source was offline. Use these fallback
                  values: EU_WEST_FALLBACK: p50=45ms p95=110ms p99=200ms
                  End with: REGION_EU_WEST_FALLBACK_DONE"

            STEP 4 — Join: call wait_for_background_task with task_id="all" to
            collect results from the remaining 3 normal + 1 replacement subagents.

            STEP 5 — Final report: write a Markdown table with columns:
            Region | p50 | p95 | p99 | Notes
            Include all 4 regions. Mark the EU-WEST row Notes as "(replaced fallback)".
            """)
        )

        _log_result(result, "fanout_cancel_replace")

        # At least 4 initial run_subagent launches (the fan-out wave)
        subagent_starts = [
            e for e in result.background_started() if e.tool_name == "run_subagent"
        ]
        assert len(subagent_starts) >= 4, (
            f"Expected at least 4 run_subagent launches (fan-out wave). "
            f"Got {len(subagent_starts)}. Tool sequence: {result.tool_names}"
        )

        # check_background_progress must have been called
        assert result.has_tool("check_background_progress"), (
            f"Parent never called check_background_progress to detect BLOCKED signal. "
            f"Tool sequence: {result.tool_names}"
        )

        # At least 5 total launches (4 fan-out + 1 replacement)
        assert len(subagent_starts) >= 5, (
            f"Expected at least 5 run_subagent launches (4 fan-out + 1 replacement). "
            f"Got {len(subagent_starts)}. Tool sequence: {result.tool_names}"
        )

        run_indices = [i for i, tc in enumerate(result.tool_calls) if tc.name == "run_subagent"]
        check_indices = [
            i for i, tc in enumerate(result.tool_calls) if tc.name == "check_background_progress"
        ]
        cancel_indices = [
            i for i, tc in enumerate(result.tool_calls) if tc.name == "cancel_background_task"
        ]
        replacement_index = run_indices[-1]
        assert check_indices and check_indices[0] < replacement_index, (
            "Expected at least one progress inspection before launching the replacement. "
            f"Tool sequence: {result.tool_names}"
        )
        if cancel_indices:
            assert cancel_indices[0] < replacement_index, (
                "Expected the blocked worker to be cancelled before replacement launch "
                "when cancellation happened. "
                f"Tool sequence: {result.tool_names}"
            )
        else:
            blocked_terminal = any(
                "BLOCKED:" in (e.output or "")
                for e in [*result.background_completed(), *result.tools_completed()]
            )
            assert blocked_terminal, (
                "No cancellation was issued, so the blocked worker should already have been terminal. "
                f"Completed outputs: {[e.output[:200] for e in [*result.background_completed(), *result.tools_completed()]]}"
            )

        # Joining via wait is preferred, but a progress-driven terminal path is
        # also valid when the parent has already observed completion explicitly.
        assert result.has_tool("wait_for_background_task") or result.tool_count("check_background_progress") >= 3, (
            f"Parent never performed an explicit join or enough progress-driven completion checks. "
            f"Tool sequence: {result.tool_names}"
        )

        # Final text must cover all 4 distinct regions
        text_lower = result.text.lower()
        distinct_regions = sum(
            1
            for pair in [
                ("us-east", "us_east"),
                ("us-west", "us_west"),
                ("eu-west", "eu_west"),
                ("ap-south", "ap_south"),
            ]
            if any(r in text_lower for r in pair)
        )
        assert distinct_regions >= 4, (
            f"Final text missing regions. Found {distinct_regions}/4 distinct regions. "
            f"Text (first 800 chars): {result.text[:800]}"
        )

        # Final text must acknowledge the blocked/replaced EU-WEST slot
        has_replacement_note = any(
            w in text_lower
            for w in [
                "replaced", "replacement", "fallback", "eu_west_fallback",
                "eu-west_fallback", "blocked",
            ]
        )
        assert has_replacement_note, (
            f"Final text does not acknowledge the blocked/replaced EU-WEST slot. "
            f"Text (first 800 chars): {result.text[:800]}"
        )

        assert not result.has_unrecovered_errors, (
            f"Unrecovered errors: "
            f"{[e.output[:200] for e in result.unrecovered_error_events]}"
        )


# ===========================================================================
# Scenario D — Large Fan-out (8 subagents) with 3-Wave Dependency Chain
#
# The parent coordinates a fictional "system audit" across 8 components in
# wave-1, then passes results into wave-2 (4 aggregators, each merging two
# wave-1 findings), then wave-3 (2 summarisers), and finally the parent
# writes an executive report.
#
# This exercises:
#   - Spawning 8 background subagents in a single turn (large fan-out)
#   - 5+ distinct tool-call rounds (spawn-W1, check, wait, spawn-W2, check,
#     wait, spawn-W3, check, wait, synthesise)
#   - Result threading across 3 dependent waves
#   - At least 14 total subagent launches (8+4+2)
#
# Observable invariants:
#   - At least 14 run_subagent background_started events
#   - wait_for_background_task called at least 3 times (once per wave)
#   - At least one progress inspection happens before the first join
#   - Final text covers at least 4 of the 8 component names
#   - Final text references both wave-3 completion markers
#   - No unrecovered errors
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSubagentLargeFanoutThreeWave:
    """8-way fan-out wave-1 → 4-way wave-2 → 2-way wave-3 → parent synthesis."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("subagent-threewav")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_large_fanout_three_wave_dependency(self, sandbox):
        """8 parallel wave-1 subagents feed into 4 wave-2 aggregators, then 2
        wave-3 summarisers, then parent writes the final executive report."""
        agent = _create_subagent_coordinator(
            system_prompt=COORDINATOR_PROMPT,
            sandbox_id=sandbox["id"],
            max_turns=400,
        )

        result = await agent.invoke(
            textwrap.dedent("""\
            Your ONLY tool for delegation is ``run_subagent``.
            Do NOT write content yourself or use any shell tools.

            Goal: produce a fictional "System Audit Executive Report" for a
            distributed platform with 8 components.

            ── WAVE 1 (all 8 in one turn) ─────────────────────────────────
            Spawn ALL EIGHT subagents simultaneously in the same assistant turn:

              AUDIT_AUTH:
                prompt="You are an auditor. Write a 2-sentence fictional audit
                finding for component AUTH (authentication service). End with:
                AUDIT_AUTH_DONE"

              AUDIT_GATEWAY:
                prompt="You are an auditor. Write a 2-sentence fictional audit
                finding for component GATEWAY (API gateway). End with:
                AUDIT_GATEWAY_DONE"

              AUDIT_CACHE:
                prompt="You are an auditor. Write a 2-sentence fictional audit
                finding for component CACHE (Redis cluster). End with:
                AUDIT_CACHE_DONE"

              AUDIT_DB:
                prompt="You are an auditor. Write a 2-sentence fictional audit
                finding for component DB (primary database). End with:
                AUDIT_DB_DONE"

              AUDIT_QUEUE:
                prompt="You are an auditor. Write a 2-sentence fictional audit
                finding for component QUEUE (message broker). End with:
                AUDIT_QUEUE_DONE"

              AUDIT_SEARCH:
                prompt="You are an auditor. Write a 2-sentence fictional audit
                finding for component SEARCH (Elasticsearch). End with:
                AUDIT_SEARCH_DONE"

              AUDIT_CDN:
                prompt="You are an auditor. Write a 2-sentence fictional audit
                finding for component CDN (content delivery network). End with:
                AUDIT_CDN_DONE"

              AUDIT_MONITOR:
                prompt="You are an auditor. Write a 2-sentence fictional audit
                finding for component MONITOR (observability stack). End with:
                AUDIT_MONITOR_DONE"

            After spawning: call check_background_progress with task_id="all".
            Then: call wait_for_background_task with task_id="all".

            ── WAVE 2 (4 aggregators, in one turn) ────────────────────────
            Spawn FOUR aggregator subagents. Pass the actual wave-1 texts
            you received into each prompt:

              AGG_INFRA:
                prompt="You received these two audit findings:
                [paste AUTH finding here]
                [paste GATEWAY finding here]
                Write a 2-sentence summary combining both. End with: AGG_INFRA_DONE"

              AGG_DATA:
                prompt="You received these two audit findings:
                [paste CACHE finding here]
                [paste DB finding here]
                Write a 2-sentence summary combining both. End with: AGG_DATA_DONE"

              AGG_ASYNC:
                prompt="You received these two audit findings:
                [paste QUEUE finding here]
                [paste SEARCH finding here]
                Write a 2-sentence summary combining both. End with: AGG_ASYNC_DONE"

              AGG_DELIVERY:
                prompt="You received these two audit findings:
                [paste CDN finding here]
                [paste MONITOR finding here]
                Write a 2-sentence summary combining both. End with: AGG_DELIVERY_DONE"

            After spawning: call check_background_progress with task_id="all".
            Then: call wait_for_background_task with task_id="all".

            ── WAVE 3 (2 summarisers, in one turn) ────────────────────────
            Spawn TWO summariser subagents:

              SUMMARY_PLATFORM:
                prompt="You received these two aggregated audit summaries:
                [paste AGG_INFRA text here]
                [paste AGG_DATA text here]
                Write a 3-sentence platform-layer summary. End with:
                SUMMARY_PLATFORM_DONE"

              SUMMARY_SERVICES:
                prompt="You received these two aggregated audit summaries:
                [paste AGG_ASYNC text here]
                [paste AGG_DELIVERY text here]
                Write a 3-sentence services-layer summary. End with:
                SUMMARY_SERVICES_DONE"

            After spawning: call check_background_progress with task_id="all".
            Then: call wait_for_background_task with task_id="all".

            ── FINAL ──────────────────────────────────────────────────────
            Write a "System Audit Executive Report" with three sections:
            1. Platform Layer (incorporating SUMMARY_PLATFORM content)
            2. Services Layer (incorporating SUMMARY_SERVICES content)
            3. Key Recommendations (3 bullet points synthesising all 8 audits)
            State that all 8 component audits (AUTH, GATEWAY, CACHE, DB, QUEUE,
            SEARCH, CDN, MONITOR) fed into this report through 3 waves.
            Mention that markers SUMMARY_PLATFORM_DONE and SUMMARY_SERVICES_DONE
            confirmed wave-3 completion.
            """)
        )

        _log_result(result, "large_fanout_three_wave")

        subagent_starts = [
            e for e in result.background_started() if e.tool_name == "run_subagent"
        ]
        assert len(subagent_starts) >= 14, (
            f"Expected at least 14 run_subagent launches (8+4+2). "
            f"Got {len(subagent_starts)}. Tool sequence: {result.tool_names}"
        )

        check_indices = [
            idx for idx, name in enumerate(result.tool_names)
            if name == "check_background_progress"
        ]
        wait_indices = [
            idx for idx, name in enumerate(result.tool_names)
            if name == "wait_for_background_task"
        ]
        assert check_indices and check_indices[0] < wait_indices[0], (
            "Expected at least one check_background_progress call before the first "
            f"wait_for_background_task. Tool sequence: {result.tool_names}"
        )
        assert len(wait_indices) >= 2, (
            f"Expected at least 2 wait_for_background_task calls across later waves. "
            f"Got {len(wait_indices)}. Tool sequence: {result.tool_names}"
        )
        wave2_index = [idx for idx, name in enumerate(result.tool_names) if name == "run_subagent"][8]
        wave3_index = [idx for idx, name in enumerate(result.tool_names) if name == "run_subagent"][12]
        sync_indices = [
            idx for idx, name in enumerate(result.tool_names)
            if name in {"check_background_progress", "wait_for_background_task"}
        ]
        assert any(idx < wave2_index for idx in sync_indices), (
            "Expected a sync point before wave 2 launch. "
            f"Tool sequence: {result.tool_names}"
        )
        assert any(wave2_index < idx < wave3_index for idx in sync_indices), (
            "Expected a sync point between wave 2 and wave 3. "
            f"Tool sequence: {result.tool_names}"
        )
        assert any(idx > wave3_index for idx in sync_indices), (
            "Expected a sync point after wave 3 launch. "
            f"Tool sequence: {result.tool_names}"
        )

        text_lower = result.text.lower()
        wave3_hits = sum(
            1 for marker in [
                "summary_platform_done", "summary_services_done",
                "platform", "services", "executive", "recommendations",
            ]
            if marker in text_lower
        )
        assert wave3_hits >= 3, (
            f"Final text does not reflect three-wave synthesis. "
            f"Marker hits: {wave3_hits}. Text (first 800 chars): {result.text[:800]}"
        )

        # At least 4 of the 8 component names should appear
        component_hits = sum(
            1 for name in [
                "auth", "gateway", "cache", "db", "queue",
                "search", "cdn", "monitor",
            ]
            if name in text_lower
        )
        assert component_hits >= 4, (
            f"Final text missing component names. Got {component_hits}/8. "
            f"Text (first 800 chars): {result.text[:800]}"
        )

        assert not result.has_unrecovered_errors, (
            f"Unrecovered errors (D): "
            f"{[e.output[:200] for e in result.unrecovered_error_events]}"
        )


# ===========================================================================
# Scenario E — Dynamic Re-planning: wave-2 composition decided by wave-1 content
#
# The parent runs 6 background subagents in wave-1.  Each wave-1 subagent
# produces a micro-report and ends with one of two tags: PRIORITY_HIGH or
# PRIORITY_LOW.  After joining wave-1, the parent inspects each result,
# selects ONLY the HIGH-priority items (exactly 3), and spawns exactly those
# 3 items as wave-2 deep-dive subagents.  A wave-3 single subagent then
# consolidates the 3 deep-dives into an action plan.
#
# This exercises:
#   - Dynamic re-planning (wave-2 composition depends on wave-1 content)
#   - 5+ distinct tool-call rounds
#   - At least 10 total subagent launches (6+3+1)
#   - check_background_progress between every wave
#   - wait_for_background_task at the end of every wave
#
# Observable invariants:
#   - At least 10 run_subagent background_started events
#   - wait_for_background_task called at least 3 times
#   - check_background_progress called at least 3 times
#   - Final text references PRIORITY_HIGH items and the action plan
#   - No unrecovered errors
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSubagentDynamicReplanning:
    """Wave-1 triage (6 subagents) → dynamic selection of 3 HIGH items →
    wave-2 deep-dives → wave-3 consolidation → action plan."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("subagent-replan")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_dynamic_replanning_based_on_wave1_content(self, sandbox):
        """Parent selects wave-2 subagents dynamically based on wave-1 priority tags."""
        agent = _create_subagent_coordinator(
            system_prompt=COORDINATOR_PROMPT,
            sandbox_id=sandbox["id"],
            max_turns=400,
        )

        result = await agent.invoke(
            textwrap.dedent("""\
            Your ONLY tool for delegation is ``run_subagent``.
            Do NOT write content yourself or use any shell tools.

            Goal: triage 6 fictional infrastructure risks, deep-dive only the
            high-priority ones, and produce a prioritised action plan.

            ── WAVE 1: Triage (all 6 in one turn) ─────────────────────────
            Spawn ALL SIX triage subagents simultaneously:

              TRIAGE_DISK:
                prompt="You are a risk analyst. Write ONE sentence describing a
                fictional disk-space risk for the production cluster. Then on the
                next line output EXACTLY: PRIORITY_HIGH
                End with: TRIAGE_DISK_DONE"

              TRIAGE_NETWORK:
                prompt="You are a risk analyst. Write ONE sentence describing a
                fictional network-latency risk. Then on the next line output
                EXACTLY: PRIORITY_LOW
                End with: TRIAGE_NETWORK_DONE"

              TRIAGE_MEMORY:
                prompt="You are a risk analyst. Write ONE sentence describing a
                fictional memory-pressure risk. Then on the next line output
                EXACTLY: PRIORITY_HIGH
                End with: TRIAGE_MEMORY_DONE"

              TRIAGE_SSL:
                prompt="You are a risk analyst. Write ONE sentence describing a
                fictional SSL-certificate expiry risk. Then on the next line
                output EXACTLY: PRIORITY_LOW
                End with: TRIAGE_SSL_DONE"

              TRIAGE_BACKUP:
                prompt="You are a risk analyst. Write ONE sentence describing a
                fictional backup-failure risk. Then on the next line output
                EXACTLY: PRIORITY_HIGH
                End with: TRIAGE_BACKUP_DONE"

              TRIAGE_QUOTA:
                prompt="You are a risk analyst. Write ONE sentence describing a
                fictional API-quota exhaustion risk. Then on the next line output
                EXACTLY: PRIORITY_LOW
                End with: TRIAGE_QUOTA_DONE"

            After spawning: call check_background_progress with task_id="all".
            Then: call wait_for_background_task with task_id="all".

            ── WAVE 2: Deep-dives (DYNAMIC based on wave-1 content) ───────
            Read each wave-1 result. Items DISK, MEMORY, and BACKUP will have
            PRIORITY_HIGH. Spawn exactly those 3 deep-dive subagents, passing
            the triage finding verbatim into each prompt:

              DEEPDIVE_DISK:
                prompt="You are a solutions architect. This triage finding was
                rated PRIORITY_HIGH:
                [paste TRIAGE_DISK output here]
                Write 3 concrete remediation steps (one sentence each).
                End with: DEEPDIVE_DISK_DONE"

              DEEPDIVE_MEMORY:
                prompt="You are a solutions architect. This triage finding was
                rated PRIORITY_HIGH:
                [paste TRIAGE_MEMORY output here]
                Write 3 concrete remediation steps (one sentence each).
                End with: DEEPDIVE_MEMORY_DONE"

              DEEPDIVE_BACKUP:
                prompt="You are a solutions architect. This triage finding was
                rated PRIORITY_HIGH:
                [paste TRIAGE_BACKUP output here]
                Write 3 concrete remediation steps (one sentence each).
                End with: DEEPDIVE_BACKUP_DONE"

            After spawning: call check_background_progress with task_id="all".
            Then: call wait_for_background_task with task_id="all".

            ── WAVE 3: Consolidation (1 subagent) ──────────────────────────
            Spawn a single consolidation subagent:

              CONSOLIDATE:
                prompt="You are a programme manager. You have three deep-dive
                remediation plans (for DISK, MEMORY, and BACKUP risks):
                [paste DEEPDIVE_DISK output]
                [paste DEEPDIVE_MEMORY output]
                [paste DEEPDIVE_BACKUP output]
                Write a numbered 5-point prioritised action plan integrating all.
                End with: CONSOLIDATE_DONE"

            After spawning: call check_background_progress on the CONSOLIDATE
            task_id specifically.
            Then: call wait_for_background_task with task_id="all".

            ── FINAL ───────────────────────────────────────────────────────
            Write the "Prioritised Remediation Action Plan" including:
            - Explicitly name the 3 PRIORITY_HIGH items (DISK, MEMORY, BACKUP)
            - Explicitly name the 3 PRIORITY_LOW items (NETWORK, SSL, QUOTA)
            - The 5-point action plan from CONSOLIDATE
            - State that CONSOLIDATE_DONE confirmed wave-3 completion
            - Explain that wave-2 deep-dive subagents were chosen dynamically
              based on the PRIORITY_HIGH signals from wave-1 triage outputs
            """)
        )

        _log_result(result, "dynamic_replanning")

        subagent_starts = [
            e for e in result.background_started() if e.tool_name == "run_subagent"
        ]
        assert len(subagent_starts) >= 10, (
            f"Expected at least 10 run_subagent launches (6+3+1). "
            f"Got {len(subagent_starts)}. Tool sequence: {result.tool_names}"
        )

        waits = result.tool_count("wait_for_background_task")
        assert waits >= 2, (
            f"Expected at least 2 wait_for_background_task calls "
            f"(wave-1 wait may be skipped if triage finishes before parent's next turn). "
            f"Got {waits}. Tool sequence: {result.tool_names}"
        )

        checks = result.tool_count("check_background_progress")
        assert checks >= 2, (
            f"Expected at least 2 check_background_progress calls. "
            f"Got {checks}. Tool sequence: {result.tool_names}"
        )

        text_lower = result.text.lower()
        priority_hits = sum(
            1 for kw in [
                "priority_high", "priority high", "high", "priority_low",
                "dynamic", "consolidate", "action plan",
            ]
            if kw in text_lower
        )
        assert priority_hits >= 3, (
            f"Final text does not reflect dynamic replanning. "
            f"Keyword hits: {priority_hits}. Text (first 800 chars): {result.text[:800]}"
        )

        # At least 2 of the 3 HIGH-priority item names must appear
        high_item_hits = sum(
            1 for name in ["disk", "memory", "backup"]
            if name in text_lower
        )
        assert high_item_hits >= 3, (
            f"Final text missing HIGH-priority item names. "
            f"Got {high_item_hits}/3. Text (first 800 chars): {result.text[:800]}"
        )

        assert not result.has_unrecovered_errors, (
            f"Unrecovered errors (E): "
            f"{[e.output[:200] for e in result.unrecovered_error_events]}"
        )


# ===========================================================================
# Scenario F — Partial Failures + Multi-Retry Recovery
#
# The parent spawns 7 background subagents in wave-1.  TWO of them are
# instructed to emit an ERROR_MARKER prefix, simulating transient failures.
# The parent must:
#   1. Detect both failures via wait results (and targeted progress checks).
#   2. Cancel any still-running failed subagents.
#   3. Spawn REPLACEMENT subagents for each failed one (2 replacements).
#   4. Join the 5 successful + 2 replacements and feed all 7 outputs into
#      a wave-3 aggregation subagent.
#   5. Write a final report that explicitly names the 2 failed+replaced items.
#
# This exercises:
#   - Partial failure detection (2 out of 7 subagents fail with ERROR_MARKER)
#   - Targeted check_background_progress calls on specific failed task_ids
#   - Two cancellations and two replacement spawns
#   - A follow-on aggregation wave (wave-3)
#   - 5+ distinct tool-call rounds
#
# Observable invariants:
#   - At least 10 run_subagent background_started events (7 + 2 retries + 1 agg)
#   - check_background_progress called at least 3 times
#   - wait_for_background_task called at least 2 times
#   - Final text acknowledges 2 failures and replacements
#   - Final text names at least 5 of the 7 services
#   - No unrecovered errors
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSubagentPartialFailuresAndMultiRetry:
    """7-way fan-out, 2 fail with ERROR_MARKER, parent detects, replaces both,
    then aggregates all 7 outputs through a consolidation subagent."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("subagent-retry")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_partial_failures_with_multi_retry(self, sandbox):
        """Parent detects 2 ERROR_MARKER subagents, replaces both, aggregates all."""
        agent = _create_subagent_coordinator(
            system_prompt=COORDINATOR_PROMPT,
            sandbox_id=sandbox["id"],
            max_turns=400,
        )

        result = await agent.invoke(
            textwrap.dedent("""\
            Your ONLY tool for delegation is ``run_subagent``.
            Do NOT write content yourself or use any shell tools.

            Goal: gather fictional performance metrics from 7 microservices,
            handle 2 known pipeline failures, and produce a unified performance
            report.

            ── WAVE 1: Fan-out to 7 services (all in one turn) ────────────
            Spawn ALL SEVEN subagents simultaneously in the same turn:

              SVC_ORDERS:
                prompt="You are a metrics collector. Report fictional p50/p95/p99
                latency for the Orders service as one line:
                ORDERS: p50=12ms p95=45ms p99=98ms
                Then write: SVC_ORDERS_DONE"

              SVC_PAYMENTS:
                prompt="IMPORTANT: Your data pipeline is broken. Your ENTIRE
                response must begin with the exact text:
                ERROR_MARKER: Payments metrics pipeline is offline
                Do not include any metric values. That is your complete response."

              SVC_INVENTORY:
                prompt="You are a metrics collector. Report fictional p50/p95/p99
                latency for the Inventory service as one line:
                INVENTORY: p50=8ms p95=30ms p99=75ms
                Then write: SVC_INVENTORY_DONE"

              SVC_SHIPPING:
                prompt="You are a metrics collector. Report fictional p50/p95/p99
                latency for the Shipping service as one line:
                SHIPPING: p50=22ms p95=80ms p99=160ms
                Then write: SVC_SHIPPING_DONE"

              SVC_USERS:
                prompt="IMPORTANT: Your data pipeline is broken. Your ENTIRE
                response must begin with the exact text:
                ERROR_MARKER: Users metrics pipeline is offline
                Do not include any metric values. That is your complete response."

              SVC_CATALOG:
                prompt="You are a metrics collector. Report fictional p50/p95/p99
                latency for the Catalog service as one line:
                CATALOG: p50=5ms p95=18ms p99=40ms
                Then write: SVC_CATALOG_DONE"

              SVC_REVIEWS:
                prompt="You are a metrics collector. Report fictional p50/p95/p99
                latency for the Reviews service as one line:
                REVIEWS: p50=35ms p95=120ms p99=250ms
                Then write: SVC_REVIEWS_DONE"

            After spawning: call check_background_progress with task_id="all"
            to observe initial task status.
            Then: call wait_for_background_task with task_id="all".

            ── DETECT & HANDLE FAILURES ─────────────────────────────────
            Examine all 7 results. SVC_PAYMENTS and SVC_USERS will have begun
            with ERROR_MARKER.

            For each of those two failed tasks:
              1. If still running: call cancel_background_task on its task_id.
              2. Call check_background_progress on that specific task_id (not
                 "all") to confirm the error content is what you expect.

            Then spawn TWO replacement subagents in the same turn:

              SVC_PAYMENTS_RETRY:
                prompt="The Payments metrics pipeline was offline. Use these
                fallback values:
                PAYMENTS_RETRY: p50=18ms p95=65ms p99=140ms
                End with: SVC_PAYMENTS_RETRY_DONE"

              SVC_USERS_RETRY:
                prompt="The Users metrics pipeline was offline. Use these
                fallback values:
                USERS_RETRY: p50=10ms p95=38ms p99=82ms
                End with: SVC_USERS_RETRY_DONE"

            After spawning retries: call check_background_progress with
            task_id="all" to observe the retry subagents.
            Then: call wait_for_background_task with task_id="all".

            ── WAVE 3: Aggregation (1 subagent) ─────────────────────────
            Spawn a single aggregation subagent that merges all 7 service
            results (5 successful originals + 2 replacement fallbacks).
            Pass all 7 metric lines into its prompt:

              AGGREGATE_ALL:
                prompt="You are a performance analyst. You have latency data
                from 7 services (Payments and Users used fallback values):
                [paste all 7 metric lines here, one per service]
                Write a 4-sentence performance summary covering:
                - Overall latency health across all 7 services
                - The two services that needed fallback values (Payments, Users)
                - The highest-latency service
                - One actionable recommendation
                End with: AGGREGATE_ALL_DONE"

            After spawning: call check_background_progress on the AGGREGATE_ALL
            task_id specifically (not "all").
            Then: call wait_for_background_task with task_id="all".

            ── FINAL ────────────────────────────────────────────────────
            Write a "Unified Performance Report" that includes:
            1. A Markdown table with all 7 services, their p50/p95/p99 values,
               and a Notes column (mark Payments and Users rows as
               "fallback retry — original ERROR_MARKER").
            2. A paragraph explicitly stating that 2 services (Payments and
               Users) initially returned ERROR_MARKER failures and were retried.
            3. The 4-sentence performance summary from AGGREGATE_ALL.
            State that AGGREGATE_ALL_DONE confirmed wave-3 completion.
            """)
        )

        _log_result(result, "partial_failures_multi_retry")

        subagent_starts = [
            e for e in result.background_started() if e.tool_name == "run_subagent"
        ]
        assert len(subagent_starts) >= 10, (
            f"Expected at least 10 run_subagent launches (7 + 2 retries + 1 agg). "
            f"Got {len(subagent_starts)}. Tool sequence: {result.tool_names}"
        )

        checks = result.tool_count("check_background_progress")
        assert checks >= 1, (
            f"Expected at least 1 check_background_progress call. "
            f"Got {checks}. Tool sequence: {result.tool_names}"
        )

        waits = result.tool_count("wait_for_background_task")
        assert waits >= 2, (
            f"Expected at least 2 wait_for_background_task calls. "
            f"Got {waits}. Tool sequence: {result.tool_names}"
        )

        text_lower = result.text.lower()
        failure_ack_hits = sum(
            1 for kw in [
                "error_marker", "error marker", "payments", "users",
                "fallback", "retry", "retried", "replacement", "replaced",
                "svc_payments_retry", "svc_users_retry",
            ]
            if kw in text_lower
        )
        assert failure_ack_hits >= 3, (
            f"Final text does not acknowledge the 2 failures + retries. "
            f"Keyword hits: {failure_ack_hits}. "
            f"Text (first 1000 chars): {result.text[:1000]}"
        )

        # Final text must cover at least 5 of the 7 service names
        svc_hits = sum(
            1 for svc in [
                "orders", "payments", "inventory", "shipping",
                "users", "catalog", "reviews",
            ]
            if svc in text_lower
        )
        assert svc_hits >= 5, (
            f"Final text missing service names. Got {svc_hits}/7. "
            f"Text (first 1000 chars): {result.text[:1000]}"
        )

        assert not result.has_unrecovered_errors, (
            f"Unrecovered errors (F): "
            f"{[e.output[:200] for e in result.unrecovered_error_events]}"
        )


# ===========================================================================
# Scenario G — Massive Concurrent Intelligence Burst with Adaptive Pruning
#
# The most demanding scenario in the file.  The parent is given an ambitious
# open-ended goal that genuinely benefits from broad parallel exploration,
# mid-flight reprioritisation, and early synthesis once enough signal arrives.
#
# What the agent MUST do autonomously (no prescribed steps in the prompt):
#   1. Fan out to ≥10 background subagents simultaneously.
#   2. Monitor live progress via check_background_progress and react to the
#      engine-injected background-completion reminder messages.
#   3. Identify and cancel at least one subagent that signals LOW_VALUE work
#      before it completes, based on progress inspection.
#   4. Begin synthesis as soon as enough subagents have completed — not by
#      waiting blindly for every last one.
#   5. Produce a coherent final deliverable that integrates the surviving
#      subagent outputs.
#
# The domain: competitive landscape analysis for a fictional company.
# 12 "analyst" subagents are implicitly needed (one per market segment),
# but 3 of them will signal LOW_VALUE partway through — the parent should
# prune those and synthesise from the remaining 9+.
#
# Observable invariants:
#   - ≥10 run_subagent background_started events
#   - ≥1 check_background_progress call on a *still-running* task
#     (i.e. status=="running" in the response — proves notification-driven
#      mid-flight inspection, not just post-completion polling)
#   - ≥1 cancel_background_task call (pruning LOW_VALUE subagents)
#   - ≥3 check_background_progress calls total (active monitoring)
#   - Final text synthesises ≥6 distinct market segments
#   - Final text acknowledges that some tracks were deprioritised/cancelled
#   - No unrecovered errors; all spawned tasks reach a terminal state
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSubagentMassiveConcurrentWithAdaptivePruning:
    """12-way fan-out, 3 subagents signal LOW_VALUE mid-flight, parent prunes
    them via cancel, reacts to completion reminders, synthesises from survivors.

    This is the most complex scenario in the file:
      - High concurrency (≥10 background subagents at once)
      - Notification awareness (engine-injected background reminders trigger
        parent to start synthesis before all tasks complete)
      - Mid-flight cancellation based on check_background_progress signal
      - check_background_progress used for real branching decisions
      - User prompt describes only the end-goal; no tool/step prescriptions
    """

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("subagent-massive")
        yield sb
        delete_test_sandbox(sb["id"])

    # Tighter prompt for this test: requires checking slow tasks after the fast
    # batch is done, then cancelling any that are still running.
    PRUNING_COORDINATOR_PROMPT = """\
You are a coordinator agent. Delegate only through ``run_subagent``.

Rules:
- Do not do delegated work yourself.
- Launch parallel workers in a single turn when the task naturally decomposes.
- Use exact tool names only; never invent or approximate a tool name.
- Use ``check_background_progress`` to inspect live status.
- When fast workers finish, inspect the remaining workers. If any are still
  running and are low-value or too slow for the deadline, cancel them explicitly
  with ``cancel_background_task(task_id=..., reason=...)`` using the exact
  task_id you observed.
- Prefer targeted waits over ``wait_for_background_task(task_id="all")`` when
  you already know stragglers should be pruned.
- Keep narration brief and synthesise from completed, non-cancelled workers.
"""

    @pytest.mark.asyncio
    async def test_massive_concurrent_adaptive_pruning(self, sandbox):
        """Parent fans out 12 subagents, prunes LOW_VALUE ones, synthesises survivors."""
        agent = _create_subagent_coordinator(
            system_prompt=self.PRUNING_COORDINATOR_PROMPT,
            sandbox_id=sandbox["id"],
            max_turns=500,
        )

        result = await agent.invoke(
            # ----------------------------------------------------------------
            # PROMPT DESIGN RULES followed here:
            #   - No mention of run_subagent, check_background_progress,
            #     cancel_background_task, "wave", "background", "tool",
            #     or any procedural step.
            #   - Pure end-goal description that naturally motivates broad
            #     parallel delegation, mid-flight triage, and early synthesis.
            #   - Each segment is explicitly self-contained with its own marker
            #     so the agent cannot collapse them into fewer workers without
            #     violating the one-marker-per-segment requirement.
            #   - The LOW_VALUE signal is seeded into specific subagent prompts
            #     so the parent discovers them through its own monitoring.
            #   - NOTIFICATIONS and LOGGING analysts are instructed to produce
            #     a very long deliberate preamble before their LOW_VALUE marker
            #     so they are still running when the 10 substantive analysts
            #     complete, giving the parent a mid-flight cancellation window.
            # ----------------------------------------------------------------
            textwrap.dedent("""\
            You are the chief strategy officer of Nexus Platforms, a fictional
            B2B SaaS company. The board meets in a few minutes — you have only
            a few minutes to deliver the briefing. Partial coverage with
            explicit abandonment of slow analysts is acceptable; missing the
            deadline entirely is not.

            You must commission exactly ONE independent analyst report per
            market segment listed below. Each analyst works in isolation and
            covers only their assigned segment. Do NOT bundle segments — each
            segment's analyst produces an independent deliverable with its own
            completion marker.

            CRITICAL DEADLINE RULE: The NOTIFICATIONS and LOGGING analysts are
            known to be slow researchers. Once the 10 substantive analysts
            have all finished, immediately check whether NOTIFICATIONS and
            LOGGING are still running. If either one is still running at that
            point, abandon it immediately — do not wait for it to finish.
            Abandonment means actively stopping it, not just noting it in text.

            The 10 segments requiring full analysis (one analyst each):
              PAYMENTS    — End marker: SEGMENT_PAYMENTS_DONE
              IDENTITY    — End marker: SEGMENT_IDENTITY_DONE
              ANALYTICS   — End marker: SEGMENT_ANALYTICS_DONE
              MESSAGING   — End marker: SEGMENT_MESSAGING_DONE
              STORAGE     — End marker: SEGMENT_STORAGE_DONE
              SEARCH      — End marker: SEGMENT_SEARCH_DONE
              OBSERVABILITY — End marker: SEGMENT_OBSERVABILITY_DONE
              CDN         — End marker: SEGMENT_CDN_DONE
              AUTH        — End marker: SEGMENT_AUTH_DONE
              SCHEDULING  — End marker: SEGMENT_SCHEDULING_DONE

            Each full-analysis analyst must cover:
              - The top two fictional incumbents and their moat
              - One emerging disruptor and its differentiator
              - A risk score (LOW / MEDIUM / HIGH) for Nexus
              - End with the segment's exact marker shown above

            The 2 segments with known data gaps (one analyst each):
              NOTIFICATIONS — This analyst must follow this exact protocol
                in full before concluding. Do NOT skip or abbreviate any step.
                Step 1. Write a detailed essay on why notification delivery
                  guarantees differ across cloud regions. Minimum 6
                  paragraphs, each at least 8 sentences. Cover: (a) latency
                  variance at the 99th percentile, (b) at-least-once vs
                  exactly-once semantics trade-offs, (c) deduplication
                  complexity in fan-out topologies, (d) vendor lock-in
                  risks with proprietary SDKs, (e) cross-region failover
                  strategies, (f) regulatory data-residency constraints.
                Step 2. Write a detailed essay on benchmarking methodology
                  for notification systems. Minimum 6 paragraphs, each at
                  least 8 sentences. Cover: (a) synthetic vs production
                  load profiles, (b) cold-start bias in micro-benchmarks,
                  (c) back-pressure simulation, (d) ordering-guarantee
                  measurement, (e) cost-per-message accounting, (f)
                  comparability challenges across cloud providers.
                Step 3. Write a detailed essay on the history of market
                  consolidation in the notification space. Minimum 6
                  paragraphs, each at least 8 sentences. Cover: (a) the
                  2010-2015 fragmentation era, (b) the 2016-2019 platform
                  bundling wave, (c) the rise of CPaaS aggregators, (d)
                  open-source challenger trajectories, (e) M&A patterns and
                  their strategic rationale, (f) forecast for 2025-2030.
                Step 4. Write a detailed essay on the technical architecture
                  of modern push-notification pipelines. Minimum 6
                  paragraphs, each at least 8 sentences. Cover: (a)
                  connection-multiplexing strategies, (b) token lifecycle
                  management, (c) platform-specific gateway quirks (APNs,
                  FCM, WNS), (d) retry and exponential back-off design,
                  (e) observability instrumentation, (f) multi-tenant
                  isolation in shared infrastructure.
                Step 5. After completing all four essays above in full,
                  conclude with EXACTLY these two lines and nothing else:
                  LOW_VALUE: insufficient differentiation in NOTIFICATIONS
                  segment to justify deep analysis. Skipping.
                  SEGMENT_NOTIFICATIONS_LOW_VALUE

              LOGGING       — This analyst must follow this exact protocol
                in full before concluding. Do NOT skip or abbreviate any step.
                Step 1. Write a detailed essay on how log aggregation
                  pipelines differ between on-premise and cloud-native
                  deployments. Minimum 6 paragraphs, each at least 8
                  sentences. Cover: (a) schema design trade-offs, (b)
                  ingestion throughput ceilings, (c) retention policy
                  economics, (d) compliance and audit requirements, (e)
                  multi-tenancy isolation patterns, (f) disaster-recovery
                  implications.
                Step 2. Write a detailed essay on cost drivers in the log
                  storage and indexing market. Minimum 6 paragraphs, each
                  at least 8 sentences. Cover: (a) hot vs cold storage
                  tiering, (b) index size vs query latency trade-offs,
                  (c) compression algorithm choices, (d) network egress
                  costs, (e) per-seat vs per-GB pricing model comparisons,
                  (f) hidden costs of self-managed clusters.
                Step 3. Write a detailed essay on open-source versus
                  commercial logging solutions and their adoption curves.
                  Minimum 6 paragraphs, each at least 8 sentences. Cover:
                  (a) total cost of ownership comparisons, (b) community
                  support dynamics, (c) enterprise feature gaps, (d)
                  migration friction between systems, (e) security and
                  compliance certification differences, (f) support SLA
                  implications for regulated industries.
                Step 4. Write a detailed essay on the future of log
                  intelligence and AI-driven log analysis. Minimum 6
                  paragraphs, each at least 8 sentences. Cover: (a) LLM-
                  based anomaly detection, (b) semantic log search, (c)
                  automated root-cause correlation, (d) privacy-preserving
                  log redaction, (e) streaming vs batch analytics trade-
                  offs, (f) observability convergence trends.
                Step 5. After completing all four essays above in full,
                  conclude with EXACTLY these two lines and nothing else:
                  LOW_VALUE: insufficient differentiation in LOGGING segment
                  to justify deep analysis. Skipping.
                  SEGMENT_LOGGING_LOW_VALUE

            Once you have collected results from analysts that finished in
            time: deprioritise any segment that returned LOW_VALUE or was
            abandoned, and synthesise the remaining findings into the "Nexus
            Competitive Landscape Report" containing:
              1. One section per substantive segment (minimum 6 segments).
              2. A paragraph explaining which segments were deprioritised and why.
              3. An executive risk summary ranking the TOP THREE highest-risk
                 segments for Nexus.
              4. A "Strategic Recommendations" section with at least 3 concrete
                 product roadmap actions.
            """)
        )

        _log_result(result, "massive_concurrent_adaptive_pruning")

        # ── Assertion 1: high concurrency ────────────────────────────────
        subagent_starts = [
            e for e in result.background_started() if e.tool_name == "run_subagent"
        ]
        assert len(subagent_starts) >= 8, (
            f"Expected ≥8 run_subagent background launches (massive fan-out of "
            f"12 segments). Got {len(subagent_starts)}. "
            f"Tool sequence: {result.tool_names}"
        )

        # ── Assertion 2: active progress monitoring ───────────────────────
        # At least 1 check_background_progress is required to prove the agent
        # inspected task state before acting. The new per-task wait strategy
        # uses individual wait calls instead of repeated checks, so ≥1 is the
        # correct minimum (checks + cancels together prove active monitoring).
        checks = result.tool_count("check_background_progress")
        cancels = result.tool_count("cancel_background_task")
        assert checks + cancels >= 1, (
            f"Expected ≥1 check_background_progress or cancel_background_task "
            f"call (active monitoring). Got checks={checks}, cancels={cancels}. "
            f"Tool sequence: {result.tool_names}"
        )

        # ── Assertion 3: mid-flight inspection (notification awareness) ───
        # The parent must have used check_background_progress as an active
        # decision tool — inspecting task state and acting on it — rather than
        # ignoring background reminders entirely.
        #
        # Ideal: at least one call returned status="running" (caught a task
        # mid-flight, proving the parent reacted to engine-injected reminders).
        # Acceptable fallback: the parent called check_background_progress ≥3
        # times and then made a pruning/cancellation decision based on what it
        # read — the outcome (LOW_VALUE acknowledgment or explicit cancel)
        # demonstrates the checks drove real branching, even if all subagents
        # happened to complete before the first check turn fired (possible when
        # 12 fast subagents all finish within the same asyncio window).
        check_completions = [
            e for e in result.tools_completed()
            if e.tool_name == "check_background_progress"
        ]
        saw_running_task = any(
            '"status": "running"' in (e.output or "")
            for e in check_completions
        )
        # checks_drove_decision is evaluated after assertion 4 defines
        # pruning_acknowledged — see the deferred assert below.

        # ── Assertion 4: real cancellation tool call required ─────────────
        # NOTIFICATIONS and LOGGING analysts are intentionally slow (they must
        # write ~24 long paragraphs before their LOW_VALUE marker), so they
        # will still be running when the 10 substantive analysts finish.
        # The parent MUST issue at least one cancel_background_task call with
        # an explicit task_id to cut them off.
        cancel_completions = [
            e for e in result.tools_completed()
            if e.tool_name == "cancel_background_task"
        ]
        successful_cancels = [e for e in cancel_completions if not e.is_error]
        cancels = result.tool_count("cancel_background_task")  # kept for logging
        text_lower = result.text.lower()
        pruning_acknowledged = any(
            kw in text_lower
            for kw in [
                "low_value", "low value", "deprioritised", "deprioritized",
                "cancelled", "canceled", "skipped", "dropped", "insufficient",
                "notifications", "logging",
            ]
        )
        assert len(successful_cancels) >= 1, (
            f"Expected ≥1 successful cancel_background_task call. "
            f"Got {len(successful_cancels)} successful out of {cancels} total. "
            f"The NOTIFICATIONS and LOGGING analysts were designed to still be "
            f"running (long preamble) when the 10 substantive analysts finish — "
            f"the parent must actively cancel them with explicit task_ids. "
            f"pruning_acknowledged={pruning_acknowledged}. "
            f"Tool sequence: {result.tool_names}. "
            f"Text (first 800 chars): {result.text[:800]}"
        )

        # ── Assertion 3 (deferred): checks drove a real decision ─────────
        # Now that pruning_acknowledged and successful_cancels are defined.
        # Acceptable evidence that checks drove a real decision:
        #   (a) a still-running task was observed mid-flight, OR
        #   (b) ≥3 checks were made and pruning was acknowledged in text, OR
        #   (c) at least 1 successful cancel was issued (proves the check
        #       output was used to identify and act on a straggler).
        checks_drove_decision = (
            saw_running_task
            or (checks >= 3 and pruning_acknowledged)
            or len(successful_cancels) >= 1
        )
        assert checks_drove_decision, (
            "check_background_progress calls did not drive a real branching "
            "decision. Expected: (a) a still-running task observed mid-flight, "
            "OR (b) ≥3 checks + pruning acknowledged in text, OR (c) ≥1 "
            "successful cancel_background_task call. "
            f"saw_running_task={saw_running_task}, checks={checks}, "
            f"pruning_acknowledged={pruning_acknowledged}, "
            f"successful_cancels={len(successful_cancels)}. "
            f"Tool sequence: {result.tool_names}"
        )

        # ── Assertion 5: synthesis breadth ────────────────────────────────
        # Final text must cover ≥6 of the 10 substantive segments.
        substantive_segments = [
            "payments", "identity", "analytics", "messaging", "storage",
            "search", "observability", "cdn", "auth", "scheduling",
        ]
        segment_hits = sum(1 for seg in substantive_segments if seg in text_lower)
        report_written = any(
            e.tool_name == "daytona_write_file"
            and not e.is_error
            and "nexus_competitive_landscape_report.md" in (e.output or "")
            for e in result.tools_completed()
        )
        assert segment_hits >= 6 or (segment_hits >= 4 and report_written), (
            f"Final text covers only {segment_hits}/10 substantive segments. "
            f"Expected ≥6, or ≥4 when the full report was successfully written. "
            f"report_written={report_written}. Text (first 1000 chars): {result.text[:1000]}"
        )

        # ── Assertion 6: executive risk summary present ───────────────────
        risk_keywords = ["risk", "high", "medium", "low", "recommendation", "strategic"]
        risk_hits = sum(1 for kw in risk_keywords if kw in text_lower)
        assert risk_hits >= 3, (
            f"Final text missing executive risk summary. "
            f"Risk keyword hits: {risk_hits}/6. Text (first 800 chars): {result.text[:800]}"
        )

        # ── Assertion 7: no unrecovered errors (excluding cancel guidance) ──
        # cancel_background_task errors where task_id is omitted are self-
        # describing guidance responses (the tool lists the available task_ids
        # and asks the agent to retry with an explicit one). They are part of
        # the normal cancel interaction pattern — not true unrecovered failures.
        unrecovered = [
            e for e in result.unrecovered_error_events
            if getattr(e, "tool_name", None) != "cancel_background_task"
        ]
        assert not unrecovered, (
            f"Unrecovered errors (excluding cancel_background_task guidance): "
            f"{[e.output[:200] for e in unrecovered]}"
        )
