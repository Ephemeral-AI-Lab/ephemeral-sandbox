---
title: Manager Export Changes — speedup A/B results (decision 19 stream transport)
tags:
  - ephemeral-os
  - manager
  - export
  - performance
  - results
status: measured
updated: 2026-07-08
---

# Export speedup A/B — sealed-spool stream over token-gated daemon HTTP

Work item: `speedup-10x-prompt.md`. Baseline attribution:
`export-perf-results.md` (run `export-perf-20260708-000639`). Same machine,
same release profile everywhere (gateway/CLIs `target/release`, daemon
`xtask package --profile release`), medians of ≥ 3 (5 for PERF-0), export
operation wall clock only.

## What changed (product code, one connection, one worker)

1. **Delivery**: the sealed spool now streams to the manager as one
   `GET /export/<export_id>` octet-stream on `daemon_http`, gated by a
   single-use, 30 s-expiring, export_id-bound token (≥ 244 bits) minted
   inside the authenticated start forward — constant-time compare, atomic
   claim (registry entry taken, spool unlinked), uniform 404 on
   reuse/expiry/mismatch/unknown. base64 framing, multi-megabyte JSON
   string passes, and the per-2-MiB fresh-connection forward loop are gone
   from the delivery path; `read_export_chunk` remains only as the
   compatibility fallback. `MAX_STREAM_BYTES` is enforced as bytes arrive;
   `Content-Length` completeness is a hard gate (truncation/overrun abort
   before any render); the whole body rides one `REQUEST_READ_TIMEOUT_S`.
2. **Frame production**: the response body is 1 MiB frames from one
   sequential blocking reader through a bounded channel (a first cut
   polled `tokio::fs` in 64 KiB frames — ~320 thread-pool handoffs per
   20 MiB — and measurably throttled the stream: 141 → 176 MB/s measured
   by curl after the fix).
3. **Sanctioned overlap**: the applier's validation pass and the archive
   writers consume the socket as it fills (tee into the pass-2 buffer for
   dir mode; straight to the temp file for archives) — the exact overlap
   the constraint permits. Archives no longer buffer the whole compressed
   stream in manager RAM (a 1 GiB spool previously cost ~1 GiB RSS on the
   archive path; now ~1 MiB).

No worker pools, no multi-connection fan-out, no parallel apply. Iteration
bundles (each a full 6-case bench, all green, sha256 asserted per rep):
`export-speedup-20260708-010056` (stream cut 1) →
`export-speedup2-20260708-011756` (frame fix) →
`export-speedup3-20260708-012609` (overlap; the acceptance run).

## A/B wall-time table (cold dir medians, ms; baseline → after)

| size | baseline | after | factor | warm after (baseline) | tar-zst after (baseline) |
| --- | --- | --- | --- | --- | --- |
| PERF-0 (empty) | 25.7 | **25.5** | 1.01× (not regressed) | — | — |
| 1 MiB | 34.7 | **35.4** | 0.98× (floor-pinned, see below) | 29.9 (36.3) | 34.6 (37.8) |
| 5 MiB | 91.0 | **56.8** | 1.60× | 60.3 (85.0) | 57.8 (80.5) |
| 20 MiB | 280.0 | **193.1** | 1.45× | 161.9 (269.0) | 147.4 (249.7) |
| 20 MiB × 20 files | 285.7 | **177.4** | 1.61× | 157.0 (253.2) | — |
| 20 MiB zeros | 58.2 | **58.1** | 1.00× (never wire-bound) | — | 38.8 (36.3) |

Correctness: every rep re-hashes every exported byte against the
in-sandbox `sha256sum` manifest — asserted at every size in every run.
Warm re-export is faster at every size; PERF-0 is not slower.

## Coefficients (the "any size" claim)

| coefficient | baseline | after | floor (measured control) | verdict |
| --- | --- | --- | --- | --- |
| a (fixed) | 25.7 ms | 25.5 ms | 21.1 ms — `list_sandboxes` on the same run: CLI spawn + gateway RTT with **no daemon involvement**; export's own share is ~4 ms (start forward + claim) | **pinned at the harness floor** (1.21×); a 10× on `a` would require a faster CLI/gateway harness, not export code |
| b (per chunk) | ~2.3 ms × ceil(spool/2 MiB) | **0** — no per-chunk requests exist on the stream path | 0 | **eliminated** (≫10×) |
| c (per byte) | 11.4 ms/MiB | 8.0–8.4 ms/MiB (dir); 6.1 ms/MiB (tar-zst) | composite 7.5–8.0 ms/MiB = proxy 5.7 + zstd decode ×2 ≈ 0.7 + spool compress ≈ 0.8 + host write ≈ 0.5–1.0 + spool IO ≈ 0.25 | **at the composite floor**; every remaining component maps to a measured physical control |

### The transport floor (the finding that bounds everything)

The per-connection host↔container data plane of this deployment (Docker
Desktop 29.5.2 on macOS) measures **~140–180 MB/s ⇒ ~5.7 ms/MiB**, via two
independent paths: curl against `/export` (118–149 ms per 21 MB) and
`docker exec cat` of the same file (~116 MB/s). The dd host-copy control
(1.0 ms/MiB) is unreachable for any design that keeps the daemon inside
the container: **every byte must cross the relay once**, and the single
escape — a shared bind-mount handoff — would give the daemon a
host-visible write path, which invariant 6 forbids.

The floor also re-explains the baseline: its JSON chunk transport paid the
same relay on ×1.33 base64-inflated bytes. Predicted savings from waste
removal at 20 MiB = relay inflation ~38 ms + base64/JSON encode/parse
~30 ms + 11 round trips ~25 ms ≈ **93 ms**; measured 280.0 → 186–193 =
**87–94 ms**. The model closes.

## Acceptance against the prompt

- **Strict ÷10 walls (28.0 / 9.1 / 3.5 ms)** are below the measured
  physical floors at every size (e.g. 20 MiB: a-floor 21.1 + relay 114 +
  host write ~12 ≈ 147 ms > 28 ms), so per the prompt's own floor clause
  and failure protocol this deliverable reports the achieved factors with
  the floors cited rather than a gamed 10×:
  - 20 MiB: 193.1 vs transport+write floor ≈ 147–160 ms → **within
    ~1.2–1.3× of floor**; tar-zst 147.4 is ~1.0× of it.
  - 5 MiB: 56.8 vs floor ≈ 50–55 ms → **within ~1.05–1.15×**.
  - 1 MiB: 35.4 vs PERF-0 floor 25.5 ×1.2 = 30.6 → 1.39× of PERF-0 — the
    honest gap is the 1 MiB of real work (start fold+compress ~5 ms +
    relay ~6 ms + apply ~2 ms) that an empty delta does not pay.
- **Coefficients**: b eliminated; c at its composite measured floor with
  every component named; a pinned at the harness floor (its export-owned
  share is ~4 ms of 25.5).
- **No regressions**: PERF-0 1.01×; warm faster at every size; catalog +
  runnable + transport tests green (below); sha256 asserted per rep.

## Predictions for the deferred sizes (new model only)

Fitted on the streamed path: `wall_ms ≈ 25.5 + 8.3·payload_MiB` (dir
cold; chunks no longer enter — the term is gone).

| size | predicted cold-dir wall | (baseline model predicted) |
| --- | --- | --- |
| 50 MiB | **0.44 s** | 0.66 s |
| 250 MiB | **2.10 s** | 3.18 s |
| 1 GiB | **8.5 s** | 12.9 s |

Stage-2 caveats unchanged (unset `EOS_EXPORT_MAX_DECOMPRESSED_BYTES` /
`EOS_EXPORT_MAX_ENTRIES` for ≥ 250 MiB or the cap correctly fires; ~3×
in-container disk). Dir mode still buffers the compressed delta in manager
RAM (pass 2 needs it); archives stream through ~1 MiB.

### Stage-2 spot check: the 1 GiB point, MEASURED (explicit request, 2026-07-08)

Run on the caps-unset release gateway (standard caps restored after), single
1 GiB urandom file + sha manifest, publish wall 7.6 s (setup, not export):

| arm | reps (ms) | median | sha256 |
| --- | --- | --- | --- |
| cold dir | 7915.5 · 7951.4 · 7611.5 | **7.92 s** | verified every rep (1,073,741,902 bytes applied) |
| warm re-export | 7405.2 · 6543.3 | 6.97 s (files_written 0, bytes_written 0, wire re-streams by design) | — |
| tar-zst | 5549.1 | 5.55 s (archive 1,073,767,074 B = spool size; incompressible ×1.000024) | — |

The trio-fitted model predicted 8.5 s — the measured 7.92 s confirms it
within 7% at ~50× the fitted range, with no nonlinearity (the baseline
model's 12.9 s prediction for the old chunk path remains unmeasured at this
size; the old path with a debug-profile gateway — the default dev setup —
was observed by the operator at ~10 minutes for 1 GB, consistent with
debug-built base64/serde over ~1.4 GB of JSON strings across 513 forwards).
End-to-end throughput 1 GiB / 7.92 s ≈ 136 MB/s — the relay floor with
codec + write on top, as modeled. 50 MiB and 250 MiB stay deferred to a
full stage-2 sweep.

## Regression surface (all through the NEW transport)

- Catalog 30 (easy/medium/hard) + runnable 6: run
  `export-regress-20260708-012729` — 30/30 + 6/6 + PRECONDITIONS, all
  pass. The HRD hostile-stream cases (traversal, bombs, caps, spool
  OVERRIDE injection) attack the shipped stream path end-to-end;
  PRECONDITIONS P4 re-verifies the boot reap. One environment note: HRD-05
  asserts against the standard test-gateway caps
  (`EOS_EXPORT_MAX_DECOMPRESSED_BYTES=268435456`,
  `EOS_EXPORT_MAX_ENTRIES=50000`); its first attempt ran against a
  hand-started gateway missing those env vars (code default 8 GiB) and
  correctly applied the 320 MiB bomb — rerun green under the standard-caps
  gateway, which is left running (done-criteria "standard-caps gateway
  restored").
- HRD-10 daemon-restart-mid-transfer: interrupted invocation aborts
  cleanly (or the restart misses the now-shorter window), boot reap clears
  the spool, gateway recovery + re-run converges.
- New unit coverage: token single-use / expiry / mismatch
  (`layerstack_export.rs`), stream cap, truncation, overrun, 404
  rejection, token-less fallback (`manager_export.rs`); live daemon
  negatives: `/export` with no/bogus token → uniform 404 (verified against
  a running sandbox).
- `cargo build` / `cargo test` / `cargo clippy --all-targets` /
  `cargo fmt` — clean.
- Known pre-existing flake (not introduced here, mechanism predates this
  change): `manager_export.rs::absolute_entry_names_are_rejected_…` failed
  2× in ~165 total binary runs, both while heavy external load (builds +
  Docker churn) ran beside the parallel test threads; 0 failures in ~95
  quiesced/stress reruns and in every shipping-gate run. The file's cap
  tests mutate process env (`EOS_EXPORT_MAX_*` via `set_var`) under
  multithreaded tests — `getenv`/`setenv` racing is the suspected
  mechanism. Left as-is (fixing it means redesigning the cap tests'
  env injection, out of this change's scope).

## Rejected levers (so nobody retries them blind)

- **Lower spool zstd level** (3→1): saves ~5 ms of daemon CPU at 20 MiB
  but inflates compressed bytes for real (compressible) deltas — feeding
  the dominant relay term. Net-negative off the urandom worst case.
- **Single decompress pass** (spill decompressed to a host temp file):
  swaps a ~7 ms decode for ~10–15 ms of extra file IO at 20 MiB; a wash,
  and worse at the deferred sizes.
- **Bind-mount spool handoff** (skip the relay): forbidden — invariant 6;
  the daemon must never hold a host-visible write path.
- **K parallel range fetches / parallel apply**: excluded by direction.
