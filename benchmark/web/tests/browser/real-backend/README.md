# Real-backend browser release gate

Run from `benchmark/web`:

```sh
npm run test:real-backend
```

The runner builds the production web bundle and release Rust binaries, starts a
loopback-only `sandbox-benchmark` process with a dedicated workspace root, and
then drives the application with Playwright. Docker, Git, the configured
EphemeralOS image, and the isolated gateway/daemon product path must all be
available; an `execution_ready: false` health response fails the suite.

This suite deliberately contains no request interception, HAR routing, mock
service worker, fake operation adapter, page-evaluated state, or DOM injection.
The browser creates seven campaigns: two bounded Command-family Quick Smokes
for comparison, one bounded Quick Smoke for each of Files, Workspace, and
LayerStack, one complete Run All, and one Run All cancelled during an active
trial. Each family run starts from its production family route and passes
through server validation, canonical review/hash, and the real start request.
Run All proves the exact persisted, non-overlapping Command → Files → Workspace
→ LayerStack transition sequence. The LayerStack report, raw observations, and
rendered UI separately prove storage plan/flatten/commit/remount-sweep phases
and per-session workspace remount.

The SSE proof deliberately disconnects while Run All is active, reconnects
with an older `Last-Event-ID`, and requires the exact missed persisted sequence
IDs to render in order before the first live ID. The suite rejects every
unexpected page request failure and React key warning, waits for a
server-confirmed active trial before cancellation, validates every indexed
artifact plus the JSON/CSV browser downloads, and compares the two completed
Command runs. The completed report is exercised and retained at 375, 768,
1024, and 1440 px.

Evidence defaults to a timestamped sibling directory outside the repository.
Set `BENCHMARK_EVIDENCE_ROOT` to another absolute path outside (and not
containing) the repository. The gate retains sanitized API snapshots, a request
ledger, artifact/export content, runner and Playwright reports, fixed-width
screenshots, an always-on Playwright trace, failure video, and a SHA-256
manifest. Before execution it
pulls the pinned `ubuntu:24.04` dependency and then snapshots Docker
containers/images/networks/volumes plus the benchmark run/results/runtime roots.
After runner shutdown it requires Docker, Docker-owned cgroup handles,
session registries, ownership markers, scratch volumes, and ephemeral roots to
match their baselines. Immutable result trees must instead match every
browser-observed allowlisted artifact path, byte count, and SHA-256 exactly.
The outside-root guard hashes regular files and symlink
targets, including existing ignored and untracked content (with only declared
build/cache exclusions), so a content change fails even when Git status is
unchanged.

`FINAL-EVIDENCE.md` records the redacted exact command sequence, tool/browser
versions, commit and dirty detail, production web-asset identity, Docker/image
identity, all seven run IDs/timestamps, product-artifact hashes, cleanup proof,
and stable links to every retained evidence file. Missing or mismatched terminal
report identity, timestamps, state, version-4 schema/derivation/counts,
persisted-report identity, or plan hash fails the gate. The gate
streams every retained file through the secret scanner and additionally expands
ZIP traces for scanning; it fails on any secret-like match or incomplete scan.
