# Phase 7 Probe B - supervisor fork feasibility report

Companion to
[`phase-07-rpc-transport-and-supervisor-fork.md`](./phase-07-rpc-transport-and-supervisor-fork.md).
Records the live Daytona supervisor-fork feasibility probe for Task 7.0.B.

---

## 1. Verdict

**Verdict: abandon Task 7.2 for this stack.**

The supervisor-fork design depends on a long-lived Python process being
able to fork a child and call `os.unshare(CLONE_NEWUSER|CLONE_NEWNS)` per
request. The live Daytona sandbox Python does not expose `os.unshare` at
all, so the hypothesis fails before mount-cleanup, fd-growth, and
crash-recovery testing are meaningful.

This is not a sandbox-down result. The same probe successfully executed
Python inside the sandbox and the final `svc.cmd` live E2E passed.

---

## 2. Evidence

Timing artifact:
`backend/tests/test_e2e/_timings/phase_7_probe_transport_supervisor_2026-05-02T20-13-07Z.json`

Sandbox Python probe output:

```json
{
  "python": "3.11.14 (main, Nov 18 2025, 05:57:35) [GCC 14.2.0]",
  "euid": 1000,
  "has_os_unshare": false,
  "clone_newuser": null,
  "clone_newns": null
}
```

The same probe also ran 100 Daytona `transport.exec` calls:

| probe | p50 | p95 | samples |
|---|---:|---:|---:|
| `transport.exec` 100x `printf 1` | `77.289ms` | `83.423ms` | 100 |

That confirms the sandbox execution path was alive while the supervisor
primitive was missing.

---

## 3. Decision mapping

| Phase 7 supervisor decision rule | Observed result | Action |
|---|---|---|
| Zero growth across 1000 iters and crash recovery | Not tested; prerequisite missing | Do not implement |
| State leaks but crash recovery works | Not tested; prerequisite missing | Do not implement |
| Supervisor cannot safely run fork+unshare | `os.unshare` unavailable in sandbox Python | Abandon 7.2 |

Because the primitive is absent, implementing a Python supervisor would
add another long-lived process without removing the current per-call
`unshare -Urm` boot cost. The correct direction is a later native runtime
or a different sandbox primitive, not a Python supervisor in this stack.

---

## 4. Follow-up

Keep Phase 6's daemon-local `unshare -Urm python3 overlay_run.py ...` path
as production. If Python-boot reduction is reopened, start from one of
these alternatives:

1. A native helper that owns namespace setup without relying on
   `os.unshare` in Python.
2. A Daytona-provided namespace/process primitive that can be verified
   with the same 1000-iteration leak and crash-recovery corpus.
3. A narrower Phase 8 snapshot/runtime optimization that avoids changing
   namespace ownership.
