# C3 Phase 1 (blame) — implementation status

Status of implementing `docs/occ_merge_publish/c3_spec.md` **Phase 1** (publish-time
three-way merge + line origin, one append-after-commit auditability log, the
`file` blame domain + CLI). Work done directly on `main`, additive edits.

## Summary

| Area | State |
|------|-------|
| Step 1 — layerstack `merge.rs` (Myers + diff3 + structural `Origin`) | **Done, tested** |
| Step 2 — layerstack `resolve.rs` / `plan.rs` / `model.rs` / `publish.rs` | **Done, tested** |
| Step 3 — runtime `file/` domain (store + blame) | **Done, tested** |
| Step 4 — runtime publish hook (origin → owner, append after commit) | **Done, tested** |
| Step 5 — `file_blame` CLI + wiring | **Done, tested** |
| Static gate (`build` / `test` / `clippy` / `fmt`) | **Green** |
| Daemon cross-compile (`aarch64-unknown-linux-musl`) | **Green** (repackaged) |
| **Live e2e** (`sandbox-cli runtime file_blame` against a real sandbox) | **Green** — passed with a real host bind directory |
| Observability counters (§14) | **Closed as deferred** (not implemented; not gating) |

## What is done (and verified green)

### Step 1 — `crates/sandbox-runtime-layerstack/src/stack/publish/merge.rs` (new)
- `pub fn three_way_merge(base, active, command) -> MergeOutcome`.
- `pub enum Origin { Command, Active(usize) }` (0-based active line index; **no
  `Unknown`, no `mixed`, no `origin.rs`** — folded in).
- `pub struct LineRange { start, len }` (1-based over committed content).
- `pub enum MergeOutcome { Clean { bytes, origin }, Conflict, Ineligible }`.
- Myers `O(ND)` line diff of base↔active and base↔command + diff3 reconcile;
  byte-exact (CRLF / missing final newline preserved); non-text (NUL / invalid
  UTF-8) or oversized (> 8 MiB) → `Ineligible`. Consecutive `Active` lines coalesce
  into one range (the active cursor is monotonic).
- Exported from `lib.rs`: `three_way_merge`, `LineRange`, `MergeOutcome`, `Origin`.
- Tests: `tests/unit/merge.rs` (wired into the white-box `tests/unit.rs` harness):
  disjoint→Clean (B8), overlap→Conflict (B7), identical-edit→**`Active`** not
  `mixed` (B12 relabelled), binary/invalid-UTF-8→Ineligible, new-file→all-Command
  (empty-base regression), CRLF/no-final-newline byte-exactness, origin tiles with
  no gaps.

### Step 2 — resolver (replaces `validate.rs`)
- `resolve.rs` (new): `resolve_publish_changes(view, active, request, plan) ->
  ResolvedChangeset { changes, origin }`. Validates each source path; on a
  file-content mismatch attempts the three-way merge (clean → merged `Write`
  bytes + origin; conflict/ineligible → `SourceConflict`). Clean path computes
  origin via `three_way_merge(base, base, command)`. Non-text / ignored → wholesale
  (empty range list).
- `plan.rs`: now carries `RouteKind` per accepted change (`AcceptedChange`).
- `model.rs`: `+ ResolvedChangeset`; `+ origin` field on
  `PublishValidatedChangesResult`. **Request stays `Vec<LayerChange>`** — no
  `owner` / `AuthoredChange` / `OwnerRef` (boundary law held).
- `ops/publish.rs`: calls the resolver, commits **bytes only**, returns origin
  (empty on no-op).
- `validate.rs` removed; `mod.rs` re-exports `resolve_publish_changes`.

### Step 3 — `crates/sandbox-runtime/operation/src/file/` (new domain)
Restructured to mirror the sibling `workspace_session` / `layerstack` domains
(`service/impls/<op>.rs`), per request:
```
file/mod.rs                       pub use FileError, FileService, BlameRange
file/error.rs                     FileError { NotFound }
file/service.rs                   mod core, impls, store
file/service/core.rs              FileService { store }, BlameRange, open(), store()
file/service/impls/mod.rs         mod blame, record_publish
file/service/impls/blame.rs       blame(&str) -> Result<Vec<BlameRange>, FileError> + tile()
file/service/impls/record_publish.rs  origin → owner mapping + append (post-commit)
file/service/store.rs             FileAuditabilityStore (ndjson + in-memory index): append, latest; AuditEvent/OwnerRange
```
- `blame` is a **pure store read**: latest event for the path, tiled `[1..=line_count]`
  from `default_owner` + sparse `owner_ranges`, equal owners coalesced. `--path`
  normalized through `LayerPath::parse` (`./src/x` == `src/x`). Unknown → `NotFound`.
- Store is append-only NDJSON (`file_auditability_<seq>.ndjson`) loaded into a
  `HashMap<path, AuditEvent>` on open; `append` is `write + fsync` then index update.
  **No serde derive** — `json!` + `serde_json::Value`.
- Tests: `tests/file_blame.rs` — tiling, coalescing, path normalization, whole-file
  (non-text) owner, structured not-found, latest-event-wins.

### Step 4 — runtime publish hook
- `layerstack::PublishChangesRequest` gains `owner: String`.
- `LayerStackService` holds `Arc<FileService>` + an `audit_gate: Mutex<()>`;
  `publish_changes` serializes commit + append, and **after** the layer commits
  maps each resolved line's `Origin` → owner string and appends one `AuditEvent`
  per path (`Command` → this publish's owner; `Active(i)` → owner of active line
  `i` from the latest event; absent → `original`; uncovered → `unknown`).
- `exec_command::finalize_one_shot` stamps `workspace_session:<id>`.
- One shared `Arc<FileService>` is created in `services.rs::from_config` and threaded
  into both `SandboxRuntimeOperations.file` (for blame) and `LayerStackService` (for
  append) so blame sees publish-time appends in the same in-memory index. Store dir =
  `config.workspace.layer_stack_root.parent()/storage/file_auditability`.

### Step 5 — `file_blame` CLI
- `cli_definition/file_operations.rs` (new): `FILE_FAMILY` + `FILE_BLAME_SPEC`
  (`name = "file_blame"`, cli path `["runtime","file_blame"]`, arg `--path`) +
  `dispatch_file_blame` (`json!` output `{ path, ranges:[{start_line,line_count,owner}] }`;
  unknown → structured `not_found`).
- Wiring: `cli_definition/mod.rs` (`pub(crate) mod file_operations;`); `operation.rs`
  (`FILE_FAMILY` in `CLI_FAMILIES`; `operation_entry_groups()` changed from
  `[_; 2]` to the slice `&'static [&'static [OperationEntry]]`; the per-domain
  `operation_entries()` are now `const fn`); `lib.rs` (`pub mod file;`).
- `services.rs`: `pub file: Arc<FileService>` + 4th `new()` arg. Fixed the 6 test
  `::new` sites (added a shared `support::test_file_service()` helper) and updated the
  `service_graph` catalog test to expect the `file` family / `file_blame` op.

### Static verification (all green)
- `cargo build` (workspace) ✓
- `cargo test -p sandbox-runtime-layerstack` ✓ (42 incl. 8 merge)
- `cargo test -p sandbox-runtime` ✓ (all suites incl. `file_blame` 6)
- `cargo clippy --all-targets` ✓ — the two C3 crates are warning-free (remaining
  warnings are pre-existing in `sandbox-gateway` / `sandbox-runtime-workspace`,
  untouched by this work).
- `cargo fmt` ✓
- `cargo run -p xtask -- package --target aarch64-unknown-linux-musl` ✓ (daemon with
  the new `file` domain + sha2 cross-compiles cleanly).

Unit-level equivalent of the e2e is covered: `tests/file_blame.rs` appends an
`AuditEvent` to a tempdir NDJSON and asserts blame tiles it (the §15 "blame is
independent of merge" test).

## Live e2e result

### Corrected host bind path — green
`bin/start-sandbox-docker-gateway --rebuild-binary` builds and serves fine, but
`sandbox-cli manager create_sandbox --image <seed> --workspace-root /workspace`
used to fail before the daemon ever ran:

```
sandbox daemon install failed: start daemon for eos-…:
docker api error: start_container: expected value at line 1 column 1;
container state=Some(CREATED) running=Some(false) exit_code=Some(0)
```

That command is invalid on this host: `--workspace-root`/`--workspace-bind-root`
is the host bind source, while `/workspace` is the container path. Docker Desktop
rejects the missing/unshared host path before the daemon starts, so the C3 daemon
and `file_blame` code never execute.

Findings so far:
- Host: macOS, Docker Desktop 29.5.2 (Engine API **1.54**, min 1.40); workspace pins
  **bollard 0.17.1** which connects with `API_DEFAULT_VERSION = 1.45`
  (`engine.rs::connect`).
- Docker itself starts equivalent containers when the bind source is a real host
  directory. The same container shape with `-v /workspace:/workspace:ro` fails with
  Docker Desktop's `mounts denied` message because host `/workspace` is not shared
  and does not exist.
- The container is left in `CREATED` because Docker rejects the bind during start.
  Bollard reports Docker Desktop's plain-text error body as the misleading serde
  parse error `expected value at line 1 column 1`.
- **Ruled out:** port 7000 / macOS AirPlay conflict (Docker publishes the
  container ports to random host loopback ports, so no config change is needed);
  multi-platform manifest-list image (rebuilt
  single-arch `--provenance=false`, same error); daemon binary executable bit
  (`archive.rs` uploads `0o755`).

Root cause: the live e2e command used the in-container path `/workspace` as the
host bind source. The provider now fails fast for missing direct-bind host
directories, and the CLI displays the less ambiguous `--workspace-bind-root`
flag while keeping `--workspace-root` as a compatibility alias.

Corrected run:
- `sandbox-cli manager create_sandbox --image ubuntu:24.04 --workspace-bind-root "$seed"`
  used a real host seed directory (`/tmp/eos-c3-readme.qhZE5K`) and created
  sandbox `eos-a808b989-8cb6-4644-97a2-3b2b13efcce2`.
- Daemon reached ready in about `0.476s`, copied 2 files / 25 bytes into the base
  layer, and unmounted the workspace bind.
- The one-shot command edited `README.md` and appended a byte to `logo.png`, then
  publish finalized successfully.
- `file_blame README.md` returned four tiled ranges:
  `original`, `workspace_session:00000118bddd3a80284ffe`, `original`,
  `workspace_session:00000118bddd3a80284ffe`.
- `file_blame does/not/exist` returned structured `not_found`.
- `file_blame logo.png` returned a single whole-file
  `workspace_session:00000118bddd3a80284ffe` range.
- The sandbox was destroyed after the assertions.

## Deferred

### Observability counters (§14) — closed as deferred, non-gating
- layerstack merge metrics: `automerge_attempted/clean/conflict`,
  `automerge_ineligible{reason}`, `merge_bytes_processed`.
- runtime audit counters: `audit_events_appended`, `audit_skipped{reason}`.
No merge/audit counter code was added. These counters are not on the e2e
critical path and are not required for Phase 1 feature completion; add them only
when acceptance requires full §14 metrics.

## Live e2e command that passed

Use the normal `ubuntu:24.04` image. Because `--workspace-bind-root` is a host
bind source, the workspace files are created in a real host temp directory and
mounted into the sandbox as `/workspace`.

1. `bin/start-sandbox-docker-gateway --rebuild-binary`
2. Create a real host seed directory:
   `seed=$(mktemp -d /tmp/eos-c3-readme.XXXXXX); printf '# Project\nSetup\nUsage\n' > "$seed/README.md"; printf '\001\002\003' > "$seed/logo.png"`
3. `sandbox-cli manager create_sandbox --image ubuntu:24.04 --workspace-bind-root "$seed"` → capture `sandbox_id`.
4. One-shot exec to change line 2, append line 4, and touch the binary file
   (publishes, owner = `workspace_session:<id>`):
   `sandbox-cli runtime exec_command --sandbox-id <id> "cd /workspace && sed -i '2s/.*/Installation/' README.md && printf 'License\n' >> README.md && printf '\004' >> logo.png"`
   (wait for the one-shot finalize/publish to complete).
5. `sandbox-cli runtime file_blame --sandbox-id <id> --path README.md` and assert:
   - shape `{ path, ranges:[{start_line,line_count,owner}] }`;
   - ranges tile `[1..=4]` with no gaps/overlaps;
   - owners ⊆ `{ original, workspace_session:<id> }` (expected: 1=original,
     2=ws, 3=original, 4=ws);
   - `file_blame --path does/not/exist` → structured not-found;
   - `file_blame --path logo.png` → whole-file single-owner range.

## Files touched

New: `merge.rs`, `resolve.rs`, `file/` (8 files), `cli_definition/file_operations.rs`,
`tests/file_blame.rs`, `tests/unit/merge.rs`, this doc.
Changed: layerstack `plan.rs`, `model.rs`, `ops/publish.rs`, `publish/mod.rs`,
`lib.rs`, `tests/unit.rs`; runtime `services.rs`, `operation.rs`, `lib.rs`,
`cli_definition/mod.rs`, `command_operations.rs`, `workspace_session_operations.rs`,
`layerstack/service/core.rs` + `model.rs` + `impls/publish_changes.rs`,
`command/service/exec_command.rs`, `operation/Cargo.toml` (+sha2), and the 6 test
`::new` sites + `support/mod.rs`.
Removed: layerstack `publish/validate.rs`.
