---
title: Manager Export Changes — sandbox delta to local filesystem
tags:
  - ephemeral-os
  - layerstack
  - manager
  - export
  - implementation-plan
status: implementation_plan
updated: 2026-07-07
---

# Manager Export Changes — sandbox delta to local filesystem

Revised four times on 2026-07-07. First, the CLI-surface simplification:
`--dest` and `--format` only. Second, the data-path correction: the daemon
**unmounts the host workspace bind after building the base**
(`operation/src/services.rs:84`, `detach_workspace_bind_after_base` — it
panics if the unmount fails), so the daemon has no host-visible write path,
ever; delivery streams the delta over the daemon protocol. Third, the
ownership move: export is a **manager operation** (`checkpoint_squash`
precedent), not a runtime one. Crossing the host boundary is operator
authority — the manager is the component that already owns host filesystem
actions (it seeds the base from a host directory at create) and sandbox
records; the runtime CLI drives in-sandbox state only. The manager service
runs on the host, so it owns the host-apply half and both CLIs stay pure
catalog clients — the manager CLI's charter ("a thin protocol client …
never a manager/runtime engine" refers to engines behind the wire; the
apply engine lives in `sandbox-manager`, server-side).

Fourth, an adversarial review (2026-07-07) hardened the host boundary and
corrected several oversold claims. The applier is a host process holding
operator privileges while consuming tar bytes authored inside the sandbox,
so a compromised daemon is in scope. The host-apply model is no longer a
blind tar-order stream: it resolves every path component with a no-follow,
dest-rooted fd walk and rejects any entry that escapes dest (invariant 9),
it buffers per directory and runs `apply_layer`'s three passes in order so
opaque clears cannot destroy just-written winners (invariant 2), and it
states an explicit metadata/fidelity boundary (invariant 10). Corrected
claims: the spool is cleaned by an explicit export-boot reap, not the
session reap (which never walks scratch for unknown dirs); the wire
envelope is `MAX_REQUEST_BYTES` (16 MiB, `sandbox-protocol/src/limits.rs:1`),
not the 8 MiB runner-result cap; each forward opens a fresh TCP connection
with no pooling; and byte-zero re-run holds for file winners only — opaque
clears and whiteouts re-apply.

Fifth, a measured speed revision (2026-07-08). Stage-1 benchmarks
(`export-perf-results.md`) attributed ~79% of the 20 MiB cold-dir wall to
the JSON chunk transport itself — base64's ×4/3 inflation and
encode/decode passes, serde round trips over ≈2.8 MB JSON strings, and a
fresh connection per 2 MiB chunk — which is exactly decision 15's named
successor condition. Delivery now streams the sealed spool to the manager
as one token-gated `daemon_http` octet-stream response (decision 19);
`read_export_chunk` remains registered as the compatibility fallback.
Invariants 6 and 9 are unchanged: the manager stays the only host writer,
and stream bytes stay hostile under the same canonicalization and caps.

## Goal

Convert the changes a sandbox has accumulated — every published layer above
the base — into a destination on the local (host) filesystem: applied
directly onto a directory (`dir`), or written as a whiteout-preserving
archive (`tar`, `tar-zst`).

The delta is not a patch file: `dir` mode performs real file writes,
deletions, and directory clears on the destination. Applied onto the host
directory the base was seeded from, the result **is** the sandbox's full
merged view — a workable tree — obtained at O(delta) cost because the host
already owns the base bytes.

Policy:

```text
Export is manager-owned. The daemon halves register cli: None (the
squash_layerstack precedent): dispatchable by name, invisible to the
runtime CLI catalog. The runtime surface gains nothing.
Export is read-only on layer-stack storage: no staging, no manifest change,
no sidecars; the spool lives under scratch and dies with the export.
Export exports the published state; live session upperdirs are invisible.
A running session never fails an export: the result names live sessions so
the caller knows unpublished upperdirs may exist, then decides.
The delta is every non-base manifest layer; the base (B*) never leaves.
Applying the delta onto the base's host-origin directory reproduces the
full merged view at delta cost; full materialization is composition, not a
mode.
The daemon never regains a host-visible path: the post-base bind detach is
law. The manager — already the host-authority component — is the only host
writer.
One wire format: the daemon always emits one zstd-compressed,
whiteout-preserving tar; dir and tar are manager-side renderings of that
stream. --format never changes what the daemon does.
```

Speed and space, explicitly (the two optimization targets):

| Cost | Bound | Mechanism |
| --- | --- | --- |
| time, enumerate | O(Σ delta-layer entries) | one newest-first metadata fold over non-base layer dirs |
| time, content read | O(merged delta bytes) | winners only — a path overwritten by a newer layer is never read from the older one; hardlinked winners are read and emitted as duplicate content (tar carries no cross-winner hardlink) |
| time, re-export host writes | O(new bytes) | manager-side skip-unchanged: entries carry source (size, second-granular mtime); the applier stamps mtimes on write and skips equal files (same-second same-size churn is a documented false-skip hole) |
| space, daemon intermediate | O(compressed delta) | one spool file under scratch — no staging tree, no per-layer copies; unlinked when the last chunk is served |
| space, wire | zstd delta × 1 (streamed); × 4/3 only on the chunk fallback | the sealed spool crosses as one octet-stream HTTP response; base64 framing survives only in the `read_export_chunk` fallback |
| memory | O(unique changed paths) | the winner map holds path → (layer dir, kind), never content |

Re-export re-streams the full compressed delta (the daemon cannot see the
host destination to diff against it) — accepted: squash bounds the delta,
and host writes still converge to O(new bytes) via the skip rule.

## CLI surface

One manager operation under the existing `management` family (no new
family), spec'd in the `sandbox-manager-operations` catalog beside
`checkpoint_squash`:

```text
sandbox-manager-cli export_changes --sandbox-id ID --dest PATH [--format dir|tar|tar-zst]
```

```text
sandbox_id  required, String.  Target sandbox; must be Ready (the existing
                               forward-path gate).
dest        required, Path.   A HOST path, absolute (the manager's CWD is
                               not the caller's — relative paths are
                               rejected). dir format: destination
                               directory, created if missing, applied in
                               place. tar formats: destination archive
                               file; must not be an existing directory.
format      optional, String, default "dir".
                               dir      apply the delta onto dest (writes,
                                        deletions, directory clears, mtime
                                        stamping, skip-unchanged)
                               tar      decompress the stream, write a
                                        plain tar
                               tar-zst  write the stream as received
```

```rust
pub const EXPORT_CHANGES_SPEC: CliOperationSpec = CliOperationSpec {
    name: "export_changes",
    family: "management",
    summary: "Export a sandbox's published changes to a host path.",
    description: "Fold every published layer above the base (newest-wins, \
                  whiteout/opaque aware) into a compressed delta stream, \
                  fetch it from the sandbox daemon, and apply it onto \
                  --dest or write it as an archive. Forwards \
                  export_layerstack and read_export_chunk requests to the \
                  sandbox daemon.",
    args: EXPORT_CHANGES_ARGS,
    cli: Some(CliSpec {
        path: &["manager", "export_changes"],
        usage: "sandbox-manager-cli export_changes --sandbox-id ID --dest PATH [--format dir|tar|tar-zst]",
        examples: &[
            "sandbox-manager-cli export_changes --sandbox-id sbox-1 --dest /home/me/myproject",
            "sandbox-manager-cli export_changes --sandbox-id sbox-1 --dest /tmp/delta.tar.zst --format tar-zst",
        ],
    }),
    related: &["inspect_sandbox", "checkpoint_squash"],
};

const EXPORT_CHANGES_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "sandbox_id",
        ArgKind::String,
        "Sandbox id.",
        Some(ArgCliSpec { flag: Some("--sandbox-id"), positional: None }),
    ),
    ArgSpec::required(
        "dest",
        ArgKind::Path,
        "Absolute host destination: directory for dir format, archive file for tar formats.",
        Some(ArgCliSpec { flag: Some("--dest"), positional: None }),
    ),
    ArgSpec::optional(
        "format",
        ArgKind::String,
        "Output format: dir, tar, or tar-zst.",
        Some("dir"),
        Some(ArgCliSpec { flag: Some("--format"), positional: None }),
    ),
];
```

The manager CLI stays a pure catalog client: it builds this one request and
prints the response. `dest` and `format` travel to the **manager service**,
which owns the whole transaction server-side (on the host): forward the
start request, page chunks, decode, render per format, return one merged
result. Nothing about export is special-cased in any CLI.

### Data path

```text
sandbox-manager-cli ── one export_changes request ──▶ manager (host process)
                                                        │ dest guard (absolute, dir/file rules)
                                                        │ forward export_layerstack ──▶ sandbox daemon
                                                        │                                fold → sealed spool → stats
                                                        │                                + single-use stream token
                                                        │ GET /export/<export_id> ─────▶ daemon_http (loopback)
                                                        │   one application/octet-stream response of the
                                                        │   sealed spool — no base64, no JSON framing, no
                                                        │   per-chunk round trips; MAX_STREAM_BYTES enforced
                                                        │   while reading; spool unlinked at claim
                                                        │ zstd decode → streaming tar apply onto dest
                                                        │ (or archive write, temp + rename)
                                                        ▼
                                            one merged result line back to the CLI
```

The start request rides the existing authenticated generic forward path
unchanged (`router/forward.rs`: record lookup, Ready gate,
`invoke_with_timeout` at `REQUEST_READ_TIMEOUT_S` = 30 s,
`sandbox-protocol/src/limits.rs:2`). Delivery is one HTTP GET against the
sandbox record's `daemon_http` endpoint: the daemon streams the sealed
spool as a single octet-stream body — one connection, one worker on each
side, and the only overlap is the socket buffer filling while the manager
reads. The manager enforces `MAX_STREAM_BYTES` while buffering, never
trusts a daemon-claimed length for allocation, and holds the whole body
read to one `REQUEST_READ_TIMEOUT_S` deadline — the same ceiling as any
forward. Completeness is a hard gate: the response must carry
`Content-Length` and the body must yield exactly that many bytes — a short
read (daemon death, dropped connection) aborts the export before any
render, because `tar-zst` mode writes the stream as received and a
truncated buffer would otherwise become a silently corrupt archive. The
abort leaves dir dests untouched (plan-before-mutate) and archive dests
absent (temp + rename); re-running converges.

Fallback: when the start result carries no stream token (a daemon
predating this revision) or the record has no `daemon_http` endpoint, the
manager pages the spool through `read_export_chunk` exactly as before.
The fallback keeps the old cost profile — each forward a fresh TCP
connection (`daemon_client.rs`: connect → write → shutdown → read; no
pooling), 2 MiB raw chunks base64-framed to ≈ 2.8 MiB under the 16 MiB
`MAX_REQUEST_BYTES` envelope (`limits.rs:1`), ~512 sequential forwards per
compressed GiB — and exists for compatibility, not speed.

The spool-building start request is the one long call and it must complete
the whole fold **and** the compressed spool write inside a single
`REQUEST_READ_TIMEOUT_S`. This is export's hard scaling limit: a delta
whose fold-plus-spool exceeds 30 s cannot start, and there is no
partial-progress or resumption path. The bound is O(Σ delta-layer entries)
for the fold plus O(compressed delta bytes) for the spool write — not the
image. Squash first collapses the delta layer count and is the documented
mitigation, but the spec names the ceiling honestly rather than calling
this "the same profile as `checkpoint_squash`": squash hardlinks winners
and splices the manifest, while export walks metadata, reads winner
content, and writes a full compressed spool — a strictly heavier call.

Scope and trace follow the checkpoint_squash idiom exactly: the manager CLI
op arrives system-scoped with `sandbox_id` in args, and the dispatcher
rebuilds each forwarded runtime request sandbox-scoped
(`CliOperationScope::sandbox(...)`). Every forward — start and all chunks —
reuses the manager request's `request_id`, so the whole export is one trace
across manager and daemon spans.

### Daemon operations (runtime side, both `cli: None`)

Two daemon-local runtime ops back the manager operation, registered like
`squash_layerstack` — dispatch-by-name entries in the layerstack group, no
catalog spec, no runtime CLI visibility, no new entry mechanism.

**`export_layerstack`** (no args): singleflight per layerstack root.
Acquire an `acquire_snapshot` lease → winner fold → emit the tar-zst spool
under `<scratch_root>/.export/<nonce>.tar.zst` → release the lease (layers
are no longer needed once spooled) → return:

```json
{
  "export_id": "exp-7f3a",
  "manifest_version": 12,
  "layers_exported": ["L000002-…", "S000003-…"],
  "entries": { "files": 214, "symlinks": 3, "whiteouts": 2, "opaques": 1 },
  "spool_bytes": 6291456,
  "stream_token": "0f6c…",
  "live_workspace_sessions": ["ws-7"]
}
```

**Stream delivery (primary).** The start result carries a single-use
stream token minted inside the authenticated start forward: ≥ 244 bits of
CSPRNG entropy, stored beside the spool's registry entry with its mint
instant. The manager claims the spool with one
`GET /export/<export_id>` against the `daemon_http` listener, the token in
the `x-eos-export-token` header. The claim is atomic under the registry
lock: constant-time token compare, expiry check (30 s TTL), entry removed,
spool opened and then unlinked (the bytes live on the open fd), body
streamed with the known content length. Reuse, expiry, an unknown
`export_id`, and a token minted for a different export all collapse to one
uniform 404 — the response never says which check failed. `daemon_http`
remains otherwise unauthenticated; this route hands bytes only to a caller
proving possession of a token that only the authenticated start path can
mint, and it writes nothing anywhere (the unlink of its own spool under
scratch is the sole mutation). A crashed manager after a claim costs
nothing: the spool died with the claim and re-running rebuilds it. Two
honesty notes. First, "authenticated start" is literal: the Docker
provider mints a per-sandbox RPC auth token at create
(`sandbox-provider-docker/src/runtime.rs:214`) and every manager forward
carries it — but the daemon's in-container unix socket skips the TCP-only
token check, so in-container code can mint stream tokens for its own
exports; that changes nothing at the host boundary (it receives bytes it
can already read from its own filesystem — the same analysis invariant 6
already makes for name-dispatch callers). Second, the token is a secret:
it never appears in the merged result line, observability attributes, or
any log — it lives in the start response and the one claim header, then
dies.

**`read_export_chunk` (fallback)** `{export_id, offset, limit?}` → one
base64 frame:

```json
{ "chunk": "…", "offset": 0, "len": 2097152, "total": 6291456, "eof": false }
```

Default and maximum `limit` 2 MiB raw (≈ 2.8 MiB encoded — under the 16 MiB
`MAX_REQUEST_BYTES` envelope). Serving the final byte unlinks the spool.

Concurrency and lifetime: `export_layerstack` is singleflight per
layerstack root (the squash `begin_flight` precedent — the second
concurrent fold is **rejected** with an already-in-flight error, not
queued and not silently replacing the first), so two folds never race.
Spools are keyed by `export_id` and coexist: a fold never unlinks another
export's spool, so two `export_changes` invocations that serialize their
folds page independently, each `read_export_chunk` pinned to its own spool
by `export_id`. This resolves the earlier contradiction between "replaces
any prior spool" and singleflight — neither happens; folds serialize and
spools are per-export.

The `{export_id → spool path, total, stream token, mint instant}` registry
is **in-memory** in the layerstack service (the command/session-registry
precedent — no runtime op persists cross-request state). A stream claim
takes the whole entry, so a concurrent fallback `read_export_chunk` for the
same export finds export-not-found and aborts cleanly — the two delivery
paths can never interleave on one spool. Two consequences the spec owns
explicitly:
(1) a daemon restart between chunks drops the registry, so subsequent
`read_export_chunk` calls fail with an export-not-found error and the whole
`export_changes` aborts — re-running is the recovery, and the fold is cheap
relative to the wire; (2) a crashed export leaves a nonce-named spool under
`<scratch_root>/.export/` that the dropped registry no longer references.
The session boot reap is registry-driven and never walks scratch for
unknown directories (`workspace/src/lifecycle/persistence.rs:77-113`,
`operation/src/services.rs:146-189` — it iterates `manager.json` handles
and sweeps only `layer_stack_root`), so export ships its **own** boot step:
on daemon start, remove `<scratch_root>/.export/` wholesale before serving.
Without it the spool leaks permanently; "reaped with scratch at boot" was
false and is now a named deliverable.

## Output contract

The manager returns — and the CLI prints — one compact JSON line on stdout
(exit 0); faults are one `{"error":…}` line on stderr (exit 1). Daemon
stats and host-side apply stats merge into one line. Pretty-printed here.

**dir:**

```json
{
  "manifest_version": 12,
  "format": "dir",
  "layers_exported": ["L000002-…", "S000003-…"],
  "files_written": 214,
  "symlinks_written": 3,
  "deletes_applied": 2,
  "opaque_clears": 1,
  "skipped_unchanged": 190,
  "bytes_written": 18874368,
  "live_workspace_sessions": ["ws-7"]
}
```

**tar / tar-zst** (apply-side fields absent; entry counts come from the
daemon result; `bytes_written` is the archive size on disk):

```json
{
  "manifest_version": 12,
  "format": "tar-zst",
  "layers_exported": ["L000002-…", "S000003-…"],
  "files_written": 214,
  "symlinks_written": 3,
  "whiteouts_emitted": 2,
  "bytes_written": 6291456
}
```

**Empty delta** (base-only manifest — no `no_op` flag, the state speaks for
itself; dir dest untouched, tar dest is a valid empty archive):

```json
{
  "manifest_version": 1,
  "format": "dir",
  "layers_exported": [],
  "files_written": 0,
  "symlinks_written": 0,
  "deletes_applied": 0,
  "opaque_clears": 0,
  "skipped_unchanged": 0,
  "bytes_written": 0
}
```

Counts, not path dumps: the result line stays bounded. `bytes_written`,
`files_written`, `symlinks_written`, `deletes_applied`, and `opaque_clears`
are **this run's work** — bytes actually written and operations actually
performed by this invocation, not current dest state — so they are
legitimate result fields under the squash counts-only rule (squash's own
result carries `squashed_blocks` counts and no byte totals;
`layerstack/service/impls/squash.rs`). `skipped_unchanged` is bounded by the
delta entry count (already surfaced via the daemon's `entries`), so it stays
in the line. Genuinely unbounded per-path deletion and skip detail belongs
to the observability record (`LAYERSTACK_EXPORT`), the same division of
labor squash uses.

`live_workspace_sessions` (both formats, omitted when empty — the
`faulty_sessions` precedent) lists the sessions alive at snapshot time.
Their uncommitted upperdir changes are invisible to export by design
(invariant 5); the field is a fact, not a fault — capture/publish and
re-export to include them. Session ids come from the existing session
registry; no upperdir walk, no dirty-check machinery.

Faults: daemon-side failures surface as the existing `operation_failed`;
manager-side failures (dest guard incl. deny-list, sandbox not Ready,
forward errors, a hostile/corrupt stream rejected by the applier, archive
rename) surface through the existing `ManagerError` → error-line path — no
dedicated error kind. A daemon restart between chunks drops the in-memory
spool registry, so the next `read_export_chunk` returns export-not-found and
the invocation aborts; re-running rebuilds the spool. A partially applied
dir dest is recovered by re-running (invariant 4).

## Vocabulary and invariants

| Name | Meaning |
| --- | --- |
| delta manifest | The active manifest's layers with every `B*` layer removed — the ordered (newest-first) set of published change layers. Computed from one snapshot; a predicate over `Manifest`, not a new type. The `B*` predicate (`layer_id.starts_with('B')`, the squash `partition_blocks` boundary at `stack/squash.rs`) matches the shared-base convention but is NOT a manifest invariant — nothing enforces exactly one base at the bottom — so the fold asserts it found ≥ 1 base and errors on a zero-base manifest rather than exporting a base as a delta layer. |
| export lease | An ordinary `acquire_snapshot` lease held from fold start to spool completion. Its only job is the existing never-mutate-leased guarantee: squash/GC cannot delete a layer dir mid-read. Zero new lease API. |
| winner fold | Pure newest-first fold over the delta layers' entries producing the winner map. Per path, the newest verdict wins: `File{layer_dir}`, `Symlink{layer_dir}`, `Directory`, `Delete` (whiteout winner), `OpaqueDir` (opaque cut — masks every older layer AND the base under that directory) — the same masking rules `MergedView`/`apply_layer` already encode (`is_kernel_whiteout_meta`, logical `.wh.` prefix, `OPAQUE_MARKER`). Reuses the per-layer walk idiom of `projection/apply.rs`; reads metadata only, never content. |
| winner map | `BTreeMap<LayerPath, Winner>` — O(unique changed paths) memory, deterministic emit order for free. |
| spool | The daemon's one intermediate artifact: winners streamed through `tar::Builder` into one zstd encoder, written to `<scratch_root>/.export/<nonce>.tar.zst`. Entries carry the source file's **mode and second-granular mtime** (tar's `mtime` is seconds); uid/gid, xattrs, and cross-winner hardlinks are NOT carried (invariant 10). `Delete` winners are emitted in the logical OCI encoding (`.wh.<name>`), opaque cuts as `.wh..wh..opq` — never kernel char-dev whiteouts, which need privileges to extract. A user file whose name begins with `.wh.` cannot appear here as content: `.wh.` is a reserved namespace and publish is fail-closed — any path component starting with `.wh.` is rejected as `ProtectedPath` at admission (`stack/publish/route.rs:22-33`), so no such name ever reaches a published layer. The logical `.wh.<name>` deletion encoding on the stream is therefore unambiguous by construction, not merely by convention (invariant 10). Unlinked when the last chunk is served; each spool is `export_id`-keyed so a new export never unlinks another's; a leftover is removed by the export boot step, not the session reap. |
| chunk paging | `read_export_chunk` serves the spool by byte offset in base64 frames (≤ 2 MiB raw). The read_command_lines shape: stable offsets, caller-driven, stateless between calls except the spool file itself. |
| host apply | The manager's dir-mode renderer. It is NOT a blind tar-order stream (that would let a dotfile child written before a later `.wh..wh..opq` be destroyed by the clear — invariant 2). It reproduces `apply_layer`'s per-directory three-pass order (`projection/apply.rs:11-63`): for each directory, (1) opaque clear, (2) whiteout deletions, (3) content — directories ensured, files compared and written, symlinks recreated. Path safety is a hard precondition, not a side effect (invariant 9): every entry name is rejected if absolute, if it contains a `..` component, or if it normalizes outside dest (whiteout targets validated AFTER the `.wh.` prefix strip); the applier reaches each entry by a dest-rooted `O_NOFOLLOW` open-parent fd walk, so a symlink at any path component — pre-existing or planted by an earlier entry — is never followed out of dest; hardlink tar entries are rejected. Within a directory: ensure-dir replaces a dest symlink/file at a directory position with a real directory; a file/symlink winner does `remove_path` then create (never writes through an existing symlink); `.wh.<name>` `remove_path`s the validated target; `.wh..wh..opq` clears the directory before its siblings apply — this is what removes base-origin files the sandbox masked with an opaque dir. |
| skip-unchanged | (size, second-truncated mtime) equality between a tar entry and its dest file. Both sides are truncated to whole seconds before comparing (tar carries seconds; `File::set_times` writes nanoseconds — comparing at nanosecond precision would make every re-export re-copy). Sound because host apply stamps the entry's second-granular mtime on every write; stateless because the destination carries the watermark. Known hole (documented, not fixed): a same-second, same-size content change published and re-exported inside one wall-clock second false-skips — add content-hash comparison later if fidelity under sub-second churn matters. |
| dest guard | Manager-side, before any forward: dest absolute (relative rejected — the manager's CWD is not the caller's); dir format: create if missing, must be a directory; tar formats: parent must exist, dest must not be a directory; archives written to a nonce-named sibling temp file, renamed into place on success. Deny-list (dir mode deletes and overwrites with manager privileges, so bare "operator authority" is not enough): reject dest equal to `/`, to `$HOME`, to the manager state/registry directory, or to any path inside `<scratch_root>/.export/`. Beyond the deny-list, in-place override is operator authority by design (B1). |

Invariants:

1. **Read-only on layer-stack storage** — export never writes under
   `layer_stack_root`: no staging, no manifest mutation, no sidecars, no
   substitution state. The spool lives under scratch and is transient.
2. **Merged-delta equivalence and apply ordering** — the delta stream
   applied onto an empty destination equals `MergedView` over the delta
   manifest for every path, including directory-only shapes, deletion
   masking, and opaque cuts. Equivalence depends on apply ORDER, not just
   content: the applier reproduces `apply_layer`'s per-directory three-pass
   order (opaque clear → whiteout → content, `projection/apply.rs:11-63`),
   because emit order is BTreeMap path order and a dotfile child
   (`cfg/.env`) sorts before its directory's `cfg/.wh..wh..opq` — a blind
   tar-order stream would write the winner then clear it away. One fold,
   one truth; the unit test pins BOTH content equivalence and the
   opaque-clear-does-not-destroy-a-dotfile-winner ordering case.
3. **Lease pins sources** — every layer dir the fold or emit reads is pinned
   by the export lease until the spool is complete. A concurrent squash sees
   the lease's newest layer as a boundary, exactly like a session lease; a
   concurrent publish prepends layers the snapshot simply doesn't include.
4. **Idempotent re-run (file winners)** — re-exporting the same manifest
   version onto the same dest writes zero *content* bytes on the host: every
   file entry skips on (size, second-mtime). Deletions and opaque clears
   still re-apply — a whiteout re-`remove_path`s an already-absent target (a
   no-op on the tree but a non-zero `deletes_applied` count), and an opaque
   clear re-clears its directory, removing anything the host added there
   since the last run (permitted by B1's fidelity condition). So
   `deletes_applied`/`opaque_clears` are non-zero on re-run and the tree is
   identical only for the file-winner subset; the qualifier matters for any
   delta carrying an opaque cut. This is still the crash-recovery story for
   a partially applied dir dest: re-run converges.
5. **Published-only** — no session upperdir, no namespace entry, no live
   mount is ever read. The snapshot manifest is the sole source of truth
   (same axis as the file-operations sessionless backend).
6. **The detach stays; the manager is the only host writer** — export never
   mounts, binds, or re-attaches anything host-visible in the daemon.
   `export_layerstack`/`read_export_chunk` are `cli: None`, so the runtime
   CLI catalog rejects them client-side (`request_builder.rs:280-289`; test
   in `sandbox-runtime-cli/tests/smoke.rs`), but the daemon dispatches by
   name across all registered ops regardless of `cli` (`operation.rs:59-70`)
   and the gateway does no catalog validation — so anyone who can reach the
   gateway can invoke them by name. This does not weaken the invariant: both
   ops only WRITE under scratch and only DELIVER bytes to their caller;
   neither has a host-visible write path. The worst a name-dispatch caller
   gets is a spool under scratch (a nuisance the boot reap and `export_id`
   keying bound) and its own bytes back. The daemon-HTTP stream route obeys
   the same law: it writes nothing host-visible, unlinks only its own spool
   under scratch, and hands spool bytes solely to the caller holding the
   single-use token that only the authenticated start forward can mint —
   an HTTP caller without the token gets a uniform 404 and nothing else.
   Delivery to the host filesystem happens only in the manager, full stop.
7. **Archive atomicity** — a tar-format dest is complete-or-absent:
   manager-side temp + one rename, so a manager crash mid-apply leaves the
   nonce-named `.tmp` sibling (never a half-written dest) for the operator
   to discard or the next run to overwrite. The spool is nonce-named and
   unlink-on-eof, so a crashed export leaves at most one dead file under
   `<scratch_root>/.export/` for the export boot step (NOT the session reap,
   which never walks scratch — see the daemon-ops section).
8. **No durability ceremony** — host apply does not fsync; durability of
   the host directory is the host's concern, and invariant 4 makes re-run
   the cheap answer to any doubt.
9. **Host-boundary safety** — the applier is a host process with operator
   privileges consuming sandbox-authored tar; a compromised daemon is in
   scope. Every entry name is rejected if absolute, if it contains a `..`
   component, or if it normalizes outside dest (whiteout targets validated
   after prefix strip); every path is reached by a dest-rooted `O_NOFOLLOW`
   open-parent fd walk so no symlink component is ever followed out of dest;
   hardlink entries are rejected. Daemon-claimed numbers (`total`, `len`,
   `spool_bytes`, content lengths, entry counts) are untrusted — the manager
   never pre-allocates on them, reads to actual EOF, enforces
   `MAX_STREAM_BYTES` on the delivery stream itself while the bytes arrive
   (both transports), and caps both decompressed output (tar mode, against a
   zstd bomb) and per-run entry count (against a millions-of-empty-files
   bomb). No canonicalization means no export.
10. **Fidelity boundary** — the stream carries content, **file** mode
    (second-granular mtime), symlink targets, logical deletions, and opaque
    cuts. It does NOT carry uid/gid (files land owned by the manager
    process), user xattrs, cross-winner hardlinks (hardlinked winners are
    emitted as duplicate content), or **directory mode**. Directory mode is
    outside the boundary because the overlay-capture model records directories
    only implicitly — via their file/symlink/opaque children, never as a
    first-class `LayerChange` (`workspace/src/overlay/capture.rs`) — so an
    empty directory is not captured at all and every consumer (squash,
    `MergedView`, and this export) materializes a directory at the layer-write
    default rather than the sandbox's `chmod`. The emit stage carries the
    directory's on-disk (default) mode faithfully; it simply has no
    sandbox-set mode to carry. Filenames beginning with `.wh.` cannot be
    represented as content — `.wh.` is a reserved namespace and publish
    fail-closes on any `.wh.`-prefixed path component (`ProtectedPath`,
    `stack/publish/route.rs:22-33`), so the logical `.wh.<name>` deletion
    encoding on the stream is unambiguous by construction. "Lossless
    OCI-style layer" (B4) means lossless for that carried set, not for
    ownership, xattrs, hardlinks, or directory mode.

## A. Expected file/folder structure with LoC change

`(new ~N)` = new file with estimated LoC; `(+N)` = lines added to existing
file. Calibrated against existing module sizes (`projection/apply.rs` 157,
`projection/mod.rs` 350, service impls 26–110, manager forward impls ~30).

```text
crates/sandbox-runtime/layerstack/
├── src/stack/projection/delta.rs           (new ~140)  delta manifest predicate + winner fold
│                                                       (newest-first, whiteout/opaque masking,
│                                                       metadata-only; shares apply.rs's walk
│                                                       and the whiteout helpers)
├── src/stack/projection/emit_stream.rs     (new ~130)  winner map → tar::Builder → zstd → spool
│                                                       file; logical .wh. re-encoding; mode +
│                                                       mtime on entries; entry counts out
├── src/stack/projection/mod.rs             (+15)       exports; walk visibility shared within
│                                                       projection
└── tests/unit/{export_delta.rs (new ~180), export_stream.rs (new ~160)} · tests/unit.rs (+2)

crates/sandbox-runtime/operation/
├── src/layerstack/service/impls/export.rs  (new ~130)  export_layerstack + read_export_chunk
│                                                       daemon ops, BOTH cli: None (squash
│                                                       precedent — rejected by the runtime CLI
│                                                       catalog, still name-dispatchable at the
│                                                       daemon, inv. 6): singleflight per root
│                                                       (begin_flight precedent, second fold
│                                                       rejected), lease scope (fold → spool),
│                                                       in-memory spool registry {export_id →
│                                                       path, total} (dies with the daemon —
│                                                       restart aborts paging), export_id-keyed
│                                                       spools, chunk reads, unlink-on-eof, live
│                                                       session ids in the start result
├── src/layerstack/service/{model,mod}.rs   (+30)       ExportOutcome, ExportChunk DTOs, exports
├── src/services.rs                         (+6)        export boot step: remove
│                                                       <scratch_root>/.export/ on daemon start
│                                                       (the session reap never walks scratch —
│                                                       H1); wired beside boot_reap_then_sweep
├── src/operation.rs                        (+4)        two entries join the layerstack group
└── tests/layerstack_export.rs (new ~180)   daemon-op dispatch: spool + paging to eof, empty
                                            delta, singleflight, spool replacement

crates/sandbox-manager-operations/
└── src/lib.rs                              (+55)       EXPORT_CHANGES_SPEC + args + CLI under
                                                        the existing "management" family; joins
                                                        SPECS (spec-only crate — dispatch stays
                                                        in sandbox-manager); checkpoint_squash's
                                                        related list gains "export_changes".
                                                        MUST land in the SAME change as the
                                                        dispatcher entry (no SPECS↔OPERATIONS
                                                        parity test exists today, and
                                                        OBSERVABILITY_SNAPSHOT already drifts —
                                                        H6); the parity test lands with it

crates/sandbox-manager/
├── src/operation/management/service/impls/export_changes.rs (new ~80)
│                                                       the manager transaction
│                                                       (checkpoint_squash.rs is the template):
│                                                       parse sandbox_id/dest/format, absolute-
│                                                       dest guard (the InvalidWorkspaceRoot
│                                                       precedent), rebuild the sandbox-scoped
│                                                       runtime request, forward
│                                                       export_layerstack, drive the chunk loop
│                                                       via forward_sandbox_request, hand the
│                                                       stream to the applier, merge the result
├── src/export_apply.rs                     (new ~200)  host-side renderer (crate-root engine
│                                                       module, the daemon_install.rs precedent):
│                                                       zstd decode, streaming tar apply
│                                                       (ensure-dir, skip-unchanged, mtime stamp,
│                                                       .wh./.opq application) or archive write
│                                                       (temp + rename)
├── src/operation/cli_definition/management_operations.rs (+3)
│                                                       import + ManagerOperationEntry::new(
│                                                       &EXPORT_CHANGES_SPEC,
│                                                       dispatch_export_changes) in OPERATIONS
│                                                       (lands with the catalog spec — H6)
├── src/operation/management/mod.rs         (+1)        re-export dispatch_export_changes
├── Cargo.toml                              (+3)        tar.workspace, zstd.workspace, base64
└── tests/manager_export.rs (new ~280)      catalog + forward loop against a fake AND a
                                            HOSTILE daemon; apply semantics: winners,
                                            deletions, opaque clears, dotfile-under-opaque
                                            ordering (inv. 2), skip, idempotent re-run,
                                            archive atomicity; security: reject `..`/absolute/
                                            hardlink entries and symlink-then-traverse, dest
                                            deny-list, decompression + entry-count caps (inv.
                                            9); SPECS↔OPERATIONS parity assertion (H6)

crates/sandbox-observability/
└── src/record.rs                           (+3)        LAYERSTACK_EXPORT

Cargo.toml (workspace)                      (+1)        zstd — NET-NEW (`tar` and `base64` 0.22
                                                        already present; no zstd usage exists in
                                                        the workspace today, so this is new
                                                        surface, not reuse). sandbox-manager
                                                        also gains base64.workspace (it does not
                                                        depend on base64 today; the runner does)

sandbox-runtime-cli / sandbox-runtime-operations         (+0)
sandbox-protocol / sandbox-daemon / sandbox-gateway / sandbox-config   (+0)
```

Totals: **5 new source files ≈ 680 LoC**, **≈ +140 LoC** in existing files
(the export boot step, canonicalization guard, and parity test add a
little), **≈ 800 LoC** of tests → ≈ 1,620 LoC end to end. Zero changes to
the protocol crate, daemon transport, gateway, config, and — after the
ownership move — zero changes to both CLIs and the runtime catalog.

Speed-revision delta (decision 19, 2026-07-08 — supersedes the zero-change
claims for the protocol and daemon-transport crates above):

```text
crates/sandbox-protocol/src/export_stream.rs      (new ~12)  shared vocabulary: route prefix,
                                                             token header name, token result
                                                             field, 30 s TTL
crates/sandbox-runtime/operation/…/service/core.rs (+~40)    ExportSpool gains token + minted_at;
                                                             claim_export_stream: constant-time
                                                             compare, expiry, single-use take,
                                                             unlink-at-claim
crates/sandbox-runtime/operation/…/impls/export.rs (+~15)    mint token at register, add it to
                                                             the start result
crates/sandbox-daemon/src/http/export.rs           (new ~80) GET /export/<id>: claim + stream the
                                                             spool fd as the response body
crates/sandbox-daemon/src/http/router.rs           (+3)      route
crates/sandbox-manager/src/…/impls/export_changes.rs (+~80)  stream-first delivery (sync HTTP GET
                                                             over one TcpStream, MAX_STREAM_BYTES
                                                             while reading, REQUEST_READ_TIMEOUT_S
                                                             deadline, Content-Length completeness
                                                             gate), chunk-paging fallback kept
Cargo.toml (workspace)                             (+0/-0)   no new dependency enters the tree
                                                             (the response body is a ~30-line
                                                             `Body` impl over `tokio::fs::File`;
                                                             the sandbox-runtime crate gains
                                                             uuid.workspace for token entropy)
```

### Adversarial review record — decision 19 revision (2026-07-08)

Reviewed against the codebase on `main` per `adversarial-review-prompt.md`
(four axes), scoped to the transport revision. Verdicts: Truth
**PASS** (auth wiring verified at `runtime.rs:214` + `dispatch.rs
strip_tcp_auth`; `daemon_http` record field at `model.rs:67`; hyper server
+ routes at `http/{server,router}.rs`); Architecture **PASS** (claim logic
lives beside the registry as one `LayerStackService` method; the manager's
client is a ~50-line sync GET, no new dependency — hyper client machinery
for one loopback GET fails prefer-less); Correctness **PASS-WITH-RISKS**
(finding 1); Security **PASS-WITH-RISKS** (findings 2–3). Findings, all
resolved in this revision:

1. **[High → resolved] Silent truncation in `tar-zst` mode.** The archive
   renderer writes bytes as received; a daemon death mid-stream would have
   produced a valid-looking but truncated `.tar.zst`. Resolution: the
   completeness gate — `Content-Length` required, received must equal it,
   short read aborts before any render (data-path section).
2. **[Medium → resolved] "Authenticated start" was deployment-dependent.**
   The RPC token check is TCP-only; the in-container unix socket can mint
   tokens. Resolution: verified the Docker provider always mints and
   passes the RPC token; the unix-socket caveat is now stated with its
   host-boundary analysis (stream-delivery section).
3. **[Medium → resolved] Token leak surface.** Resolution: the
   never-logged / never-in-result rule is now spec text; the manager's
   result builders construct fresh objects, so the token cannot ride out
   by construction, and the daemon span records no token attribute.
4. **[Low → accepted] Timing distinguishes unknown-export from bad-token**
   (HashMap miss skips the constant-time compare). Leaks only the
   existence of an in-flight export on a loopback surface; the token
   compare itself stays constant-time. Accepted with this note.
5. **[Low → resolved] The LoC delta initially omitted the response-body
   mechanics.** Landed as a dependency-free `Body` impl over
   `tokio::fs::File` (no `tokio-util` feature bump after all); no new crate
   enters the workspace, and `sandbox-runtime` gains only the existing
   workspace `uuid` for token entropy.

Build order: winner fold (pure) → emit-stream → daemon ops (spool +
chunks) → manager applier (pure over a byte stream, testable without a
daemon) → manager op impl + catalog spec → observability record.

## B. Export workflows

Legend: `Ln` published layer, `B` base, `wh(p)` whiteout of path p,
`opq(d)` opaque marker on directory d. Manifests are newest-first.

### B1. Primary — apply onto the seeding host directory, workable result

```text
host /home/me/myproject seeded the base at create time.
sandbox published: L1: src/a.rs, src/b.rs        L2: src/a.rs (edit), wh(src/b.rs)

sandbox-manager-cli export_changes --sandbox-id sbox-1 --dest /home/me/myproject

daemon:  fold → winners { src/a.rs → File(L2), src/b.rs → Delete, src/ → Dir }
         (L1's a.rs is masked: never read; the base's thousands of files
          never enter the fold)
         spool: src/ · src/a.rs · src/.wh.b.rs
manager: forwards the start request, pages 1 chunk, applies onto
         /home/me/myproject — a.rs overwritten (L2 content, L2 mtime),
         .wh.b.rs deletes b.rs

/home/me/myproject now equals the sandbox's full merged view — base +
delta — and is immediately workable. Total cost: two layer walks, one file
copy, one deletion; the base crossed nothing.
```

Fidelity condition: the result equals the sandbox view exactly when every
path the delta does not touch still carries base-seed content. Host edits
made after seeding survive at untouched paths (export neither knows nor
cares), are overwritten at winner paths, and are removed under
opaque-cleared directories. In-place override is destructive by design —
no backup, no dry-run; the workspace's own VCS is the review-and-undo
surface, and invariant 4 makes re-running always safe. A dest with no base
copy at all gets the sparse delta tree, not a workspace: full copies at
arbitrary locations are composition (seed copy first, then export — see
the `dir-full` deferral for the no-base-copy case). Fidelity is over
content, mode, symlink targets, and deletions (invariant 10) — uid/gid,
xattrs, and hardlinks are not reproduced, so "equals the sandbox view"
means the carried set, not ownership or extended metadata.

### B2. Re-export after more publishes — incremental by property

```text
first export @v3 onto /home/me/myproject      214 files applied
publishes land: v5 = [L4 L3 L2 L1 B]          L3, L4 touch 9 paths
second export @v5 onto the same dest:
  wire: full compressed delta streams again (the daemon cannot see dest)
  host: 205 entries equal (size, mtime) → skipped; 9 written
result: files_written 9, skipped_unchanged 205
```

No watermark flag, no server state: the mtime stamped at apply time is the
watermark, carried by the destination itself.

### B3. Masking — opaque directory over base content

```text
base: cfg/dev.yml, cfg/prod.yml     L1: opq(cfg), cfg/prod.yml (rewrite)

fold: cfg → OpaqueDir, cfg/prod.yml → File(L1)
stream: cfg/ · cfg/.wh..wh..opq · cfg/prod.yml
apply onto the seeding dir: the opaque entry CLEARS cfg/ (removing the
base-origin dev.yml the sandbox masked), then prod.yml applies

dest converges to the sandbox view including base files hidden by the
opaque cut — the whole reason the opaque marker rides the stream instead
of being resolved away by the fold.
```

Honesty boundary: a path that *leaves* the delta between exports without a
masking winner is not re-converged on a stale dest — the delta no longer
describes it. `amend_path` rewriting head is one producer; squash is
another — flattening a block `[L-with-file, L-with-whiteout]` can drop a
path that no surviving layer re-emits (`stack/squash/flatten.rs`,
newest-wins per subtree), and lease-release GC removes layer dirs once
unreferenced (`lease/cleanup.rs`). None of these leaves a whiteout
describing the vanished path. The contract is "dest reflects THIS delta",
not "dest is synchronized with delta history"; a fresh dest or re-seeded
copy is the escape hatch. Stated here so nobody retrofits rsync semantics
later.

### B4. Archive — lossless, transportable delta

```text
same v3 as B1, --format tar-zst --dest /tmp/delta.tar.zst

manager writes the stream as received: .delta.tar.zst.<nonce> → rename
entries: src/ · src/a.rs · src/.wh.b.rs        (logical OCI encoding)
result: files_written 1, whiteouts_emitted 1, bytes_written = archive size
```

The archive is a valid OCI-style layer for the carried set (invariant 10):
content, mode, symlink targets, and deletions survive and apply later onto
any base copy; uid/gid, xattrs, and hardlinks do not. `.wh.`-prefixed
filenames never collide with the deletion encoding because publish
fail-closes on them (reserved namespace, `stack/publish/route.rs:22-33`). It
compressed before crossing the wire.

### B5. Concurrent squash — the lease does all the work

```text
export starts @v13, lease snapshot [L12 L11 L10 … B]
checkpoint_squash runs mid-spool:
  export lease's newest layer is a boundary → squash blocks straddling it;
  replaced layers the export still reads stay on disk (never mutate or
  delete a leased layer dir — existing law, no new code)
spool completes on its snapshot → lease releases → refcount GC reclaims
whatever only the export pinned; chunks keep serving from the spool, which
depends on no layer
next squash compacts what this one had to skip
```

No retry, no invalidation, no special case: invariant 3 plus the spool's
independence from layer dirs is the whole story. The two manager
operations compose: squash to compact, export to deliver.

## C. Non-goals and deferrals

- **Live-session export** — upperdirs are captured/published by the existing
  session lifecycle; export reads published truth only and reports live
  sessions in the result line rather than failing on them. Publish first.
- **Full materialization (`dir-full`)** — a self-contained snapshot
  including the base, for a dest with no host copy of the seed or one that
  has diverged. Composition covers the common cases: in-place override
  (B1), and a full copy at an arbitrary location = copy the seed there
  (`cp -a` / fresh clone), then export onto the copy. When a no-base-copy
  full export is genuinely needed it is one new `format` value streaming
  ALL layers — the base is immutably available daemon-side
  (`base/B000001-base`), and `MergedView::project` already encodes the
  semantics — at documented O(image) wire and time cost. Deliberately not
  v1.
- **Dest defaulting from the sandbox record** — the manager knows the host
  workspace the base was seeded from (`inspect_sandbox` already surfaces
  the workspace root), so `--dest` could default to it. Deferred: a
  zero-extra-args invocation whose default behavior deletes host files is
  the wrong ergonomic to ship first; revisit once usage exists.
- **Server-side dest diffing** — wire bytes are always the full compressed
  delta. A future manifest-version watermark could skip spooling entirely
  when nothing changed; deferred until re-export frequency proves it
  matters.
- **Bounded parallel apply** — host apply is a serial tar stream; a width-N
  pool would need out-of-order extraction for marginal gain on a
  local-filesystem write path. Not planned.
- **Byte-level deltas between exports** — skip-unchanged captures the win at
  a fraction of the complexity.
- **Path filtering, dry-run, checksum flags** — deliberately absent. Per-
  layer digests in `.layer-metadata/` remain the integrity story.

## Decision log

1. **Two user arguments beyond the sandbox selector** (user review,
   2026-07-07): `--dest`, `--format`. Incrementality, deletion policy,
   parallelism, and scope became defaults/properties instead of flags.
2. **Deletions apply by default in dir mode**: "convert the changes"
   includes deletions; a fresh dest makes them no-ops; counts in the result
   line, per-path detail in the observability record (bounded result,
   squash precedent).
3. **No new family, no new entry mechanism**: the manager op joins the
   existing `management` family; the daemon ops register with `cli: None`
   (squash_layerstack precedent) so the runtime surface gains nothing.
4. **Reuse over invention**: winner fold lives beside `projection/apply.rs`
   and shares its walk and whiteout vocabulary; `MergedView` stays the
   read-path truth; equivalence is invariant 2 and a unit test, not a
   shared abstraction forced before it's needed.
5. **mtime-stamped writes** power skip-unchanged (tar entries carry source
   mtime; the applier stamps on write) — chosen over a server-side
   watermark to keep the daemon stateless about destinations it cannot see.
6. **`zstd` enters `[workspace.dependencies]`**; `tar` is already there.
   Compression is worth a dependency because the delta crosses the wire
   base64-framed — fewer raw bytes is the dominant win.
7. **Logical `.wh.` encoding on the stream** (kernel char-dev whiteouts
   need privileged extraction); host apply consumes whiteouts and opaque
   markers directly and never writes them to a dir dest. The encoding is
   unambiguous by construction: `.wh.` is a reserved namespace and publish
   fail-closes on any `.wh.`-prefixed path component (`ProtectedPath`,
   `stack/publish/route.rs:22-33`, landed 2026-07-07), so no user file can
   ever collide with a deletion marker on the wire. This closes the earlier
   whiteout-name-collision concern at the source (publish admission), not in
   the applier.
8. **No fsync in dir mode**: idempotent re-run is the recovery story;
   tar dests get temp+rename because a partial archive is corrupt, not
   merely incomplete.
9. **Delta, not full materialization** (design review, 2026-07-07): the
   base seed is host-origin, so a full export re-copies bytes the host
   already has at O(image); the delta yields the full view by composition
   onto the host's base copy at O(delta) and is the only form that carries
   deletions explicitly. `dir-full` stays a documented deferral.
10. **Running sessions report, never fail** (design review, 2026-07-07):
    session existence is not evidence of missing changes, sessions never
    mutate published layers (no consistency hazard to guard), and blocking
    export under long-lived sessions would make it unusable. Silent
    omission is equally wrong, so the result line carries
    `live_workspace_sessions` — the squash report-don't-fail precedent.
11. **Chunk streaming, not bind-mount writes** (data-path review,
    2026-07-07): the daemon unmounts the host workspace bind after base
    build (`services.rs:84`; panic on failure), so "write through the
    bind" was never available. One tar-zst spool, paged chunks
    (`read_command_lines` precedent). Costs accepted: base64 4/3 framing
    over zstd, O(compressed delta) spool in scratch, full-delta wire on
    re-export.
12. **Opaque markers ride the stream**: the winner fold must NOT resolve
    opaque cuts away, because they are the only record that base-origin
    files under that directory were masked — host apply's clear-directory
    is what makes dest converge to the sandbox view (B3).
13. **Manager-owned, manager-applied** (user direction, 2026-07-07):
    crossing the host boundary is operator authority, and the manager is
    the component that already touches the host filesystem (base seeding)
    and holds the sandbox records. The manager service — a host process —
    drives the forward loop and owns the applier, so BOTH CLIs stay pure
    catalog clients (the previous revision had special-cased the runtime
    CLI). `--sandbox-id` joins the surface as the ordinary manager-op
    selector; each forward is one bounded request on the existing
    `router/forward.rs` path; dest must be absolute because the manager's
    CWD is not the caller's.
14. **Three winner-fold implementations — keep separate, cross-check by
    test** (adversarial review, 2026-07-07): after export ships, newest-wins
    lives in `MergedView::read_entry` (on-demand reads), squash
    `flatten::collect_winners` (staging trees, content-hardlinking), and the
    export fold (metadata-only, streaming). They are not unified: each has a
    different output shape and forcing one abstraction would bloat all three.
    But export is the second consumer of the fold, which is exactly the
    trigger squash's "no shared abstraction before it's needed" reserved, so
    the divergence risk is paid down with a test — invariant 2 pins
    fold↔`MergedView`, and a fixture test asserts the export fold and
    `flatten` agree on winner SELECTION for a shared layer stack. Extraction
    stays deferred but is now explicitly a decision, not an omission.
15. **JSON chunk paging over the two faster transports** (adversarial
    review, 2026-07-07): `SandboxRecord.daemon_http` exists and hyper is a
    workspace dep, so an HTTP stream could kill the base64 4/3 tax and the
    ~512-round-trip loop; and the manager owns Docker, so a `upload_archive`
    download twin could bypass the daemon protocol entirely. Both are
    rejected for v1 for the same reason: the daemon protocol is where lease
    pinning and snapshot consistency live. A Docker-level volume read cannot
    see the lease and would tear against a concurrent squash GC or a partial
    layer write; the `daemon_http` surface is unauthenticated and adding a
    large-payload streaming path to it widens the attack surface the detach
    invariant works to keep narrow. JSON chunk paging reuses the audited,
    bounded `router/forward.rs` path and adds zero transport surface. If
    re-export volume ever makes the base64 tax dominate, the HTTP stream is
    the documented next step — not a v1 requirement.
16. **Host-boundary hardening is a v1 gate, not a follow-up** (adversarial
    review, 2026-07-07): the applier consumes sandbox-authored tar as a
    host process with operator privileges, so canonicalization (reject
    absolute/`..`/hardlink, `O_NOFOLLOW` fd walk, validate whiteout targets
    after prefix strip), per-directory three-pass ordering, and
    untrusted-count caps (invariants 2, 9) are preconditions of the first
    merge, tested against a hostile fake daemon. "Ensure-dir replaces a
    symlink" was never a sufficient defense.
17. **Boot cleanup is export-owned** (adversarial review, 2026-07-07): the
    session boot reap is registry-driven and never walks scratch for unknown
    directories, so the spool would leak forever under it. Export adds its
    own boot step to remove `<scratch_root>/.export/`. The prior "reaped
    with scratch at boot" claim was false and is corrected to a deliverable.
18. **Honest envelope and timeout** (adversarial review, 2026-07-07): the
    wire envelope is `MAX_REQUEST_BYTES` (16 MiB), not the 8 MiB
    runner-result cap the earlier draft cited; the start request must fit one
    30 s `REQUEST_READ_TIMEOUT_S`, which is export's hard scaling ceiling
    (squash-first is the mitigation); and each forward opens a fresh TCP
    connection, so the chunk loop's cost is stated, not hidden behind "same
    profile as checkpoint_squash".
19. **The sealed spool streams over daemon HTTP; JSON chunk paging becomes
    the fallback** (measured speed revision, 2026-07-08): stage-1 benchmarks
    (`export-perf-results.md`) fitted wall ≈ 25.7 + 2.3·chunks + 11.4·MiB
    against a 1.0 ms/MiB dd read+write control and a 25.7 ms PERF-0 floor,
    and attributed ~79% of a 20 MiB cold export to the chunk transport
    itself: base64's ×4/3 inflation and encode/decode passes, serde
    serialize/parse over ≈2.8 MB JSON strings, and a fresh thread + runtime
    + TCP connection per 2 MiB chunk. That is decision 15's own successor
    condition ("if the base64 tax dominates, the HTTP stream is the
    documented next step"), and both of its objections are answered rather
    than overridden: (a) consistency — the spool is a lease-independent,
    `export_id`-keyed artifact sealed before delivery, so streaming it
    changes no snapshot semantics and no layer read ever bypasses the
    daemon protocol; (b) auth — `daemon_http` stays unauthenticated except
    that `GET /export/<export_id>` serves bytes only against a single-use,
    30 s-expiring, export_id-bound token (≥ 244 bits CSPRNG) minted inside
    the authenticated `export_layerstack` forward, compared in constant
    time, with reuse/expiry/mismatch/unknown collapsing to one uniform 404.
    The claim is atomic (entry taken, spool unlinked at claim), so the
    stream and the fallback pager can never interleave on one spool. One
    connection, one worker, no added parallelism — the speedup is waste
    removal (framing, string passes, round trips), not fan-out.
    `MAX_STREAM_BYTES` is enforced on the stream while bytes arrive; the
    whole body read rides one `REQUEST_READ_TIMEOUT_S`; invariants 6 and 9
    are unchanged. The manager falls back to `read_export_chunk` whenever
    the start result carries no token (older daemon) or the record has no
    `daemon_http` endpoint.
    Post-implementation measurement addendum (2026-07-08): the response
    body is produced as 1 MiB frames by one sequential blocking reader
    through a small bounded channel (a first cut polled `tokio::fs` in
    64 KiB frames — ~320 thread-pool handoffs per 20 MiB — and measurably
    throttled the stream). With that fixed, the residual per-byte wall is
    the deployment's own host↔container data plane: Docker Desktop's
    per-connection relay measures ~140–180 MB/s on this machine via two
    independent paths (curl against `/export`, and `docker exec cat` of
    the same 20 MiB at ~116 MB/s), so ~5.7 ms/MiB is a TRANSPORT FLOOR the
    single-connection constraint cannot go under on this deployment — any
    delivery design that keeps the daemon inside the container pays it.
    The one escape (a shared bind-mount handoff) would give the daemon a
    host-visible write path and is forbidden by invariant 6 by design.
    Floors are cited per size in `export-speedup-results.md`.
