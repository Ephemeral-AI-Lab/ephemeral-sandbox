"""Tier 9 — performance (PLAN §19.7 / NEXT-AGENT-GUIDE phase 9).

All 7 tests are capability-gated per §18: on the reference CI host
(``EOS_CI_REFERENCE_HOST=true``) probe-False is a HARD failure; off-host
it is a loud skip. The HYBRID baseline (session medians × checked-in
``latency_budget.json``) is enforced via :class:`LatencyBudget`.
"""
