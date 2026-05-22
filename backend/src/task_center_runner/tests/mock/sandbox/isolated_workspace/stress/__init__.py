"""Tier 8 — stress / soak (PLAN §5 / §19.6 / NEXT-AGENT-GUIDE phase 8).

5 tests gated behind the ``live_e2e_soak`` marker per PLAN §9.5. They
exercise full TOTAL_CAP fan-out, FD/IP-pool drift detection, idle freeze-
at-rest, full pip-install-then-run e2e, and v2's at-rest disk-bounded
proof.
"""
