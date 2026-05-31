# Sandbox → Rust migration — PROGRESS

Living status tracker for `docs/plans/sandbox-rust-external-migration-PLAN.md`.
Spec = PLAN.md. Landed-status snapshot = PLAN §13. This file = done/next checklist.

**Last updated:** 2026-05-31 · **Phase:** 1 scoped fresh-ns runner closed; host dispatch fork remains Phase 2.

---

## Phase status at a glance

| Phase | Scope | Status |
|---|---|---|
| **0 — Bootstrap** | workspace, eos-protocol, put_archive, pins, CP-0/local upload | ✅ **local amd64+arm64 upload closeout complete; signing/full matrix deferred** |
| 1 — ns-runner (fresh-ns) | `eos-runner` unshare→mount→exec | ✅ **scoped direct `eosd ns-runner` closeout complete; host dispatch is Phase 2** |
| 2 — daemon + read paths | `eos-daemon` RPC, read verbs, readiness | ⬜ skeleton only |
| 3 — write/publish + shell/search + plugin (HIGH risk) | OCC/LayerStack publish, PPC | ⬜ skeleton only |
| 3.5 — isolated workspace | ns-holder + setns + shell-free net | ⬜ skeleton only |
| 5 — cutover | flip default, delete Python | ⬜ |

Legend: ✅ done · 🟡 partial · ⬜ not started.

---

## DONE (verified 2026-05-31, all checks re-run independently)

**Rust workspace `/sandbox` — 11 crates + xtask, ~7,800 LOC**
- ✅ `eos-protocol` **fully implemented + tested**: version/envelope/cas/audit/models/canonical. **29 tests green incl 18 executed CAS golden fixtures** (the `ensure_ascii` Unicode trap reproduced).
- ✅ Faithful **skeletons** for layerstack/overlay/occ/ephemeral/isolated/plugin/runner/ns-holder/daemon/eosd — **546 `// PORT backend/…:line` anchors + 19 `todo!()`**.
- ✅ `cargo check --workspace` green (12 crates) · `cargo clippy --workspace` green at deny-gate · `cargo fmt --all --check` clean.
- ✅ `xtask package` implemented for `eosd-linux-{amd64,arm64}`: default builder is `rust-lld` (`cargo` with `RUSTFLAGS=-C linker=rust-lld`), with optional `cargo`/`cross`; writes binary-only `SHA256SUMS`, `protocol_version`, per-artifact JSON manifests, and optional minisign `.minisig` signatures. Current Phase 1 artifacts package locally (`amd64` SHA `438d0bde3b5f615c451d5e850fe163818a37262d7201de2e5e34dc743beb77b6`, `arm64` SHA `710972fd0cdcfad35536e7998afefc5b1b100bfeaac189175c7ab0e208f8f577`).
- ✅ **Build-time guarantee holds**: `cargo tree -p eos-isolated` has no `eos-occ` edge (direct/transitive). HINGE split (`SnapshotLeasePort` vs `CommitTransactionPort` in `eos-layerstack`) + 3 severings wired (`OccServicesInjector` impls both `eos_occ::` and `eos_ephemeral::OccRuntimeServicesPort`, returns the per-root single writer — MF-1-aware).

**Contracts & fixtures (ground truth)**
- ✅ `sandbox/docs/contract/01-06.md` — source-verified wire/CAS/audit/models/provider/crate-map specs.
- ✅ `sandbox/crates/eos-protocol/fixtures/` — 18 CAS cases + envelope/audit/metrics fixtures (executed from real Python).
- ✅ `sandbox/docs/RUST-GUIDANCE.md` — the Rust standard for all builders (incl. exact `ensure_ascii` escaper spec).

**Python-side Phase 0 (surgical; focused sandbox tests passed)**
- ✅ `put_archive` on `ProviderAdapter` Protocol + Docker adapter (async → `container.put_archive`) + Daytona stub.
- ✅ `backend/src/sandbox/host/runtime_artifact/__init__.py` pins the local artifacts: `EOSD_VERSION=0.1.0-local.20260531`, amd64 SHA256 `438d0bde3b5f615c451d5e850fe163818a37262d7201de2e5e34dc743beb77b6`, arm64 SHA256 `710972fd0cdcfad35536e7998afefc5b1b100bfeaac189175c7ab0e208f8f577`, protocol version `1`. Minisign remains empty until the later release-provenance gate.
- ✅ `backend/src/sandbox/_contract_fixtures/` vendors the Rust fixtures; `pin.json` is hard-pinned to `2df20649b3158324d1be9c4c6c53a5844034ebc2` with `fixtures_sha256=3d62ff3017bf1b1a76e36de08ea4a3185d9640cb9ca98f7e4a1796b153aab221`; the backend pin assert is hard-fail (no skip).
- ✅ `EOS_SANDBOX_RUNTIME=python|rust` no-op host read exists in `daemon_client.py` and validates values; the actual dispatch fork remains Phase 2.
- ✅ `backend/scripts/bench_sandbox_e2e.py` has Docker-backed Phase 0 mode for CP-0 + CP-1 (`--phase0`) plus local artifact upload verification (`--eosd-binary`) that uses `put_archive`, Docker archive readback, and direct binary exec. `backend/scripts/build_upload_eosd_docker.py` is the narrower build/package/upload script for both arches. Neither path installs `apt`/`pkg` packages or requires Rust/Cargo inside the target sandbox image for the artifact check.
- ✅ GitHub CI is **not** part of the current Phase 0 closeout path. The current path is: build/package locally, then upload the static binary into the sandbox/container.

**Phase 0 CP baseline artifacts**
- ✅ `bench/baseline-amd64.json` captured in `sweevo-dask__dask-10042:latest` (Ubuntu 22.04.4, Python 3.10.14, kernel `6.10.14-linuxkit`, `x86_64`, `/eos-mount-scratch` tmpfs, overlay-in-userns probe green).
- ✅ CP-0 measured: runtime bundle upload `4092.846 ms`; daemon cold-start `885.234 ms`; daemon idle RSS `36,676 KiB`; Python process-start p50 `428.128 ms`; warm heartbeat p50 `1.103 ms`, p95 `1.993 ms`.
- ✅ CP-1 passed: `put_archive` vs base64-over-exec for `1.5 MiB` (`17.260 ms` vs `23,003.217 ms`, 64 chunks) and `3.0 MiB` (`32.196 ms` vs `45,602.537 ms`, 128 chunks); all SHA256s matched; put-archive size ratio `1.865` ≤ `2.5`.
- ✅ `bench/local-eosd-amd64-upload.json` captured the historical Phase 0 bootstrap amd64 handoff: `sandbox/dist/eosd-linux-amd64` (683,328 bytes, static PIE) uploaded to `/tmp/eosd-local/eosd` in `8.121 ms`; readback SHA256 matched `c81993538d4cfb6425e1a00f91d38d0a85dd07a1706907c3b07db6faf5a5629e`; mode `0755`; direct exec returned `eosd 0.1.0`; target `rustc`/`cargo` absent. Current Phase 1 amd64 artifact verification is `bench/phase1-ns-runner-amd64.json`.
- ✅ `bench/local-eosd-arm64-upload.json` captured the historical Phase 0 bootstrap arm64 handoff: `sandbox/dist/eosd-linux-arm64` (597,848 bytes, static aarch64 ELF) uploaded to `/tmp/eosd-local/eosd` in `8.444 ms`; readback SHA256 matched `6edbe7bdc7bb4d6414b2b331d58857b1ce55bcf61bd391f34f34b36bdba716c6`; mode `0755`; direct exec returned `eosd 0.1.0`; target `rustc`/`cargo` absent. Current arm64 artifact is rebuilt and pinned but not re-upload-smoked in this dask-only pass.

**Phase 1 implementation artifacts (local, 2026-05-31)**
- ✅ `eos-overlay::kernel_mount` now validates `O_DIRECTORY|O_NOFOLLOW` inputs, pins lower/upper/work dirs through `/proc/self/fd/*`, calls the raw `fsopen→fsconfig(lowerdir+)→fsconfig(upperdir/workdir)→fsmount→move_mount` sequence, and tears down stacked mounts via RAII drop.
- ✅ `eos-overlay::writable_dirs` now creates the canonical `/eos-mount-scratch/eos-sandbox-runtime` root and per-run `upper`/`work` dirs.
- ✅ `eos-runner` fresh-ns mode now performs best-effort `setsid` (Docker exec may already be process-group leader), `unshare(NEWUSER|NEWNS)`, root uid/gid map setup, private mount propagation, overlay mount guard acquisition, shell command execution with cwd/env policy, timeout kill, and `RunResult` JSON construction. Fast-child wait polling is `5 ms` to avoid an avoidable 100 ms floor.
- ✅ `eosd ns-runner` now reads a `RunRequest` from stdin, `--request PATH`, or one positional request path; writes compact JSON to stdout or `--output PATH`; and wires the runner to the `eos-overlay` mount adapter.
- ✅ Compile/lint checks cover both host and Linux syscall cfg surfaces: host `cargo check --workspace`, host targeted tests, `x86_64-unknown-linux-musl` targeted check, and Linux-target clippy for `eos-overlay`, `eos-runner`, and `eosd`.
- ✅ `bench/phase1-ns-runner-amd64.json` captured direct `eosd ns-runner` in `sweevo-dask__dask-10042:latest` with artifact SHA `438d0bde3b5f615c451d5e850fe163818a37262d7201de2e5e34dc743beb77b6`: AV shell smoke green (`hello.txt` read from lower, `generated.txt` captured in upper), timeout cleanup green (non-zero timeout, no lingering `sleep`, no parent-namespace `/testbed` mount leak), and 20/20 perf samples green.
- ✅ CP-2b direct-runner host-wall comparison passed: Rust fresh-ns `true` p50 `332.952 ms`, p95 `348.733 ms` vs refreshed CP-0 Python process-start p50 `428.128 ms` in the same dask image. This is the apples-to-apples direct-runner number: `95.177 ms` faster p50, `22.2%` latency reduction, `1.286×` speedup.
- ✅ CP-2a measured Rust mount-init path passed the ≥20× bar: `workspace.mount_s` p50 `1.506 ms` (`284.3×` faster than CP-0 Python process-start p50). This `284×` figure is intentionally **not** an end-to-end tool-call claim: it compares raw Rust/kernel overlay mount initialization (`fsopen→fsconfig→fsmount→move_mount`, no workspace copy) against Python process startup (`python3 -c pass`) in the dask container.
- ✅ Bottleneck interpretation recorded: network is not the main delay in this local dask run. Direct runner host-wall p50 is `332.952 ms`; internal `mount+tool` p50 is `296.068 ms`; raw mount p50 is `1.506 ms`; implied host/Docker/request overhead is about `36.884 ms`. The dominant remaining cost is shell/process startup (`bash -lc true`) under the amd64 dask container, likely amplified by Docker Desktop/emulation.

**Docs**
- ✅ PLAN §12 (verified Docker/dask/plugin config) + §13 (Phase-0 status + 8 source-verified corrections).

**Re-verify everything:**
```
.venv/bin/python backend/scripts/build_upload_eosd_docker.py --arch amd64 --image sweevo-dask__dask-10042:latest --report bench/local-eosd-amd64-upload.json
.venv/bin/python backend/scripts/build_upload_eosd_docker.py --arch arm64 --image python:3.11-slim --platform linux/arm64 --report bench/local-eosd-arm64-upload.json
cd sandbox && cargo test -p eos-protocol && cargo check --workspace && cargo clippy --workspace && cargo fmt --all --check
cd .. && .venv/bin/python -m pytest backend/tests/unit_test/test_sandbox/test_provider/ backend/tests/unit_test/test_sandbox/test_contract_fixtures_pin.py backend/tests/unit_test/test_sandbox/test_api/test_daemon_client.py -q
.venv/bin/python backend/scripts/bench_sandbox_e2e.py --commands 10 --report /tmp/eos-synthetic-bench.json
.venv/bin/python backend/scripts/bench_sandbox_e2e.py --docker-image sweevo-dask__dask-10042:latest --phase0 --commands 10 --report bench/baseline-amd64.json
# Direct Phase 1 dask evidence is currently captured in bench/phase1-ns-runner-amd64.json.
```

---

## NEXT — ordered, concrete

### A. Phase 0 closeout follow-ups (not blocking local amd64)
1. **Release-grade provenance** — minisign fail-closed verification remains a later AV-8 gate. Current Phase 0 local closeout is SHA-pinned but unsigned by design.
2. **Arm64 CP baseline leg** — `local-eosd` arm64 upload/run is captured; `bench/baseline-arm64.json` CP-0/CP-1 remains for an arm64-native Docker host or explicit local runner. The local `sweevo-dask__dask-10042` image is the amd64 CP baseline leg.
3. **Minimal-image matrix** — when Phase 1/CP-1b starts, extend local upload checks to non-root and read-only-rootfs images. The current amd64 gate proves the artifact needs no in-image Rust/toolchain and can be uploaded via provider `put_archive`.

**Re-run the amd64 CP baseline when needed:**
   ```
   .venv/bin/python backend/scripts/build_upload_eosd_docker.py \
     --arch amd64 \
     --image sweevo-dask__dask-10042:latest \
     --report bench/local-eosd-amd64-upload.json
   .venv/bin/python backend/scripts/build_upload_eosd_docker.py \
     --arch arm64 \
     --image python:3.11-slim \
     --platform linux/arm64 \
     --report bench/local-eosd-arm64-upload.json
   .venv/bin/python backend/scripts/bench_sandbox_e2e.py \
     --docker-image sweevo-dask__dask-10042:latest \
     --phase0 \
     --commands 10 \
     --report bench/baseline-amd64.json
   ```

### B. Phase 1 closeout guardrails
- Treat Phase 1 as closed for the scoped direct `eosd ns-runner` fresh-ns boundary. Keep `bench/phase1-ns-runner-amd64.json` as the direct-runner dask evidence until a checked-in Phase 1 harness exists.
- Do not flip `EOS_SANDBOX_RUNTIME=rust` from this result alone. Host dispatch, persistent daemon routing, and endpoint readiness are Phase 2.
- Remaining scope clarification: setns mode stays Phase 3.5. Current `eosd ns-runner` is an executable request/response subcommand for the fresh path, not a full daemon runtime cutover.

### C. Phase 2 — daemon + read paths
- Fill `eos-daemon` server/dispatcher/audit-ring + read verbs + LayerStack/OCC **read** paths; host `EOS_SANDBOX_RUNTIME` dispatch fork + AF_UNIX local-fallback connector reproducing the **97/98** exit-code contract. Gate: CP-3 + AV-2 (respawn/readiness/endpoint-cache).

### D. Phase 3 (HIGH risk) — write/publish + OCC/LayerStack + plugin PPC
- Fill OCC publish (single `occ-commit-queue`, 0.002/64/3), LayerStack squash/GC, the **reentrant-RLock→Mutex restructuring** (do NOT 1:1 port — see RUST-GUIDANCE §5), `eos-plugin` PPC channel + MF-1 single-writer routing.
- Gate: CP-4 (final-workspace-state hash) + the **§7 differential/property tests under contention** (NOT fixtures) + AV-1c byte-identity + AV-7 forward/back on-disk parity + AV-10 plugin parity. Needs the Python differential harness.

### E. Phase 3.5 (isolated) then Phase 5 (cutover) — per PLAN §5.

---

## Notes / risks for next session
- **Skeletons are not logic.** The 19 `todo!()` bodies + 546 `// PORT` anchors are the precise work-list; each cites the exact Python `file:line` to port.
- **macOS can build/package this pure-Rust static musl amd64 skeleton with `rust-lld`, but cannot validate Linux syscall behavior.** All syscall/overlay/OCC-contention work must be checked in the dask container (PLAN §12.2 recipe) — `cargo check` on macOS only validates the non-Linux `cfg` surface.
- **Not committed.** Treat the worktree as parallel-agent dirty; stage intentionally.
- **CAS byte-identity is the sharpest correctness lever** — any new code computing `manifest_root_hash`/`layer_digest` must pass `fixtures/cas/cases.json` (esp. the unicode cases).
