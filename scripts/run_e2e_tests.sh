#!/usr/bin/env bash
# Run e2e live tests.
# Usage: ./scripts/run_e2e_tests.sh <name>    # run a specific test file
#        ./scripts/run_e2e_tests.sh all        # run all live e2e tests
#
# Examples:
#   ./scripts/run_e2e_tests.sh anthropic_live
#   ./scripts/run_e2e_tests.sh background_live
#   ./scripts/run_e2e_tests.sh tool_selection_eval
#   ./scripts/run_e2e_tests.sh all

set -euo pipefail
cd "$(dirname "$0")/.."

PYTEST=".venv/bin/python -m pytest"
E2E_DIR="backend/tests/test_e2e"
COMMON_OPTS=(-o addopts= -v -s --tb=short)
LIVE_OPTS=(-m live --log-cli-level=INFO)
MOCK_OPTS=(-m "e2e and not live" --log-cli-level=INFO)

# Live tests (hit real APIs via EvalAgent/DB registry)
LIVE_TESTS=(
    test_anthropic_live.py              # Anthropic client streaming protocol
    test_tool_selection_eval.py         # LLM tool selection accuracy
    test_background_live.py             # Background task execution
    test_background_reminder_live.py    # Ephemeral background reminders
    test_background_context_live.py     # Context pressure with background tasks
    test_background_autonomy_live.py    # LLM autonomous background decisions
    test_bg_high_concurrency_live.py    # High-concurrency bg+fg mixing
    test_bg_task_lifecycle_live.py      # Task lifecycle: progress, cancel, notify
    test_bg_idle_wait_live.py           # Idle/wait scenarios for bg tasks
    test_bg_mixed_chaos_live.py         # Mixed chaos: errors, relaunch, pipelines
    test_bg_physical_cancel_live.py     # Physical process kill on cancel
    test_bg_wait_tool_live.py           # Wait tool: blocking, timeout, wait_for_all
    test_bg_progress_output_live.py     # Progress checks, last_n_lines, output
    test_bg_autonomous_decisions_live.py # Autonomous decisions based on bg results
    test_bg_parallel_tasks_live.py      # Parallel bg/fg task orchestration
    test_bg_idle_patterns_live.py       # Complex idle and wait patterns
    test_bg_supernova_live.py           # Supernova: debug-fix-retest cycles
    test_bg_long_suite_live.py          # Long suite with early cancel iterations
    test_bg_live_tail.py                # Live progress tail via on_progress_line
    test_subagent_complex_live.py       # Complex subagent fan-out/refinement/recovery
    test_eval_persistence_live.py       # EvalAgent persistence parity
    test_live_api.py                    # Live API integration
    test_live_full_run.py               # Complete agent run with metrics
    test_live_sandbox_agents.py         # Sandbox tool calling
    test_live_agent_react_landing.py    # React page agent
    test_live_nextjs_sandbox.py         # Next.js sandbox agent
    test_live_minimax_comprehensive.py  # MiniMax comprehensive tests
    test_live_codeact_edge_cases.py     # CodeAct: pip install, CWD, team constraints
    test_live_codeact_occ_transactions.py # Direct CodeAct OCC transaction tool tests
    test_live_daytona_tool_occ_calls.py # Direct daytona_write_file/edit_file/codeact OCC tests
)

# Mock/unit tests (no real API needed)
MOCK_TESTS=(
    test_chat_flow.py                   # Chat SSE event flow
    test_agent_toolkits_skills.py       # Toolkit/skill registration
    test_compaction.py                  # Context compaction
    test_code_intelligence.py           # Code intelligence service
    test_daytona_toolkit_comprehensive.py # Daytona toolkit unit tests
    test_multi_tool_e2e.py              # Multi-tool execution
    test_tool_cancel_e2e.py             # Tool cancellation
    test_token_tracker_e2e.py           # Token tracking persistence/API
    test_minimax_agent.py               # MiniMax agent (server-based)
    test_anthropic_native_agent.py      # Anthropic native agent (server-based)
    test_agentic_loop_e2e.py            # Agentic loop (server-based)
)

_run_batch() {
    local label="$1"
    local mode="$2"
    shift 2
    local tests=("$@")
    local passed=0 failed=0 skipped=0
    local mode_opts=("${MOCK_OPTS[@]}")

    if [[ "$mode" == "live" ]]; then
        mode_opts=("${LIVE_OPTS[@]}")
    fi

    for test_file in "${tests[@]}"; do
        echo ""
        echo "================================================================"
        echo "  Running: $test_file"
        echo "================================================================"

        if $PYTEST "$E2E_DIR/$test_file" "${COMMON_OPTS[@]}" "${mode_opts[@]}"; then
            ((passed++))
        else
            exit_code=$?
            if [[ $exit_code -eq 5 ]]; then
                echo "  -> SKIPPED (no credentials)"
                ((skipped++))
            else
                echo "  -> FAILED"
                ((failed++))
            fi
        fi
    done

    echo ""
    echo "================================================================"
    echo "  $label Summary"
    echo "================================================================"
    echo "  Passed:  $passed"
    echo "  Failed:  $failed"
    echo "  Skipped: $skipped"
    echo "================================================================"

    [[ $failed -gt 0 ]] && return 1
    return 0
}

if [[ $# -eq 0 ]]; then
    echo "Usage: $0 <test_name|command>"
    echo ""
    echo "Commands:"
    echo "  all       Run all live e2e tests"
    echo "  mock      Run all mock/unit e2e tests"
    echo "  list      List all available tests"
    echo ""
    echo "Live tests:"
    for t in "${LIVE_TESTS[@]}"; do
        echo "  ${t%.py}"
    done
    echo ""
    echo "Mock tests:"
    for t in "${MOCK_TESTS[@]}"; do
        echo "  ${t%.py}"
    done
    exit 0
fi

NAME="$1"

case "$NAME" in
    list)
        echo "Live tests (require API credentials):"
        for t in "${LIVE_TESTS[@]}"; do echo "  ${t%.py}"; done
        echo ""
        echo "Mock tests (no API needed):"
        for t in "${MOCK_TESTS[@]}"; do echo "  ${t%.py}"; done
        exit 0
        ;;
    all)
        _run_batch "Live E2E" "live" "${LIVE_TESTS[@]}"
        exit $?
        ;;
    mock)
        _run_batch "Mock E2E" "mock" "${MOCK_TESTS[@]}"
        exit $?
        ;;
esac

# Find matching test file across both lists
MATCH=""
MATCH_MODE=""
for t in "${LIVE_TESTS[@]}" "${MOCK_TESTS[@]}"; do
    if [[ "$t" == *"$NAME"* ]]; then
        MATCH="$t"
        for live_t in "${LIVE_TESTS[@]}"; do
            if [[ "$live_t" == "$t" ]]; then
                MATCH_MODE="live"
                break
            fi
        done
        if [[ -z "$MATCH_MODE" ]]; then
            MATCH_MODE="mock"
        fi
        break
    fi
done

if [[ -z "$MATCH" ]]; then
    echo "No test matching '$NAME'. Run '$0 list' to see available tests."
    exit 1
fi

echo "Running: $MATCH"
if [[ "$MATCH_MODE" == "live" ]]; then
    $PYTEST "$E2E_DIR/$MATCH" "${COMMON_OPTS[@]}" "${LIVE_OPTS[@]}"
else
    $PYTEST "$E2E_DIR/$MATCH" "${COMMON_OPTS[@]}" "${MOCK_OPTS[@]}"
fi
