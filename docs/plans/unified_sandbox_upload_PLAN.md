# Unified Sandbox Upload (`upload_into_sandbox`) — Implementation Plan

**Status:** proposed
**Owner:** TBD
**Last updated:** 2026-06-04
**Scope:** legacy Python host (`backend/src`) upload path for the daemon-setup and
plugin-setup stages. Rust `eos-sandbox-host` parity noted in §10.

---

## 1. Goal

Replace the two stages' bespoke, multi-round-trip upload dances with **one**
host-side primitive, `upload_into_sandbox(content, path=None)`, that writes into
the `/eos` tmpfs over the proven-fastest transport and makes the "`/eos` is the
only upload target" rule structural.

This is a **transport-preserving, round-trip-reducing** change. It does **not**
introduce a new wire protocol, a new provider primitive, or any write outside
`/eos`.

## 2. Background — why `put_archive` was "broken"

`/eos` is a Docker **tmpfs** (`--tmpfs /eos:rw,exec,size=2g,mode=1777`,
`backend/src/sandbox/provider/docker/client.py:42`). Docker's archive endpoint
(`container.put_archive` / `docker cp`) extracts **daemon-side into the on-disk
rootfs**, behind the tmpfs mount. Verified on Docker 28.3 + `sweevo-dask__dask-10042`:

| write | result |
| --- | --- |
| `docker cp host → /eos/probe.txt` | **exit 0 (success)**, but `ls /eos` empty, `cat` → "No such file" — **silent loss** |
| `docker cp host → /tmp/probe.txt` (rootfs) | exit 0, file present |

Silent data loss (not an error) is why it surfaced downstream as an LSP/Pyright
init timeout (`/eos/env/eos-node22/bin` never populated) instead of an upload error.

The existing workaround `DockerProviderAdapter._put_archive_into_eos_tmpfs`
(`adapter.py:422`) is correct: it execs `tar -xf - -C <dir>` **inside** the
container's mount namespace and streams the tar over the hijacked exec socket, so
writes go *through* the mount. Both stages already funnel through it via the
`/eos*` routing branch in `put_archive` (`adapter.py:405`).

## 3. Background — the experiment (what actually costs)

Real-container benchmark, 5 samples, `sweevo-dask__dask-10042`:

**A** = current path (`put_archive` → exec+tar, stays in `/eos`).
**B** = clean archive-PUT to `/tmp` rootfs + one in-container `tar -xf` into `/eos`.

| payload | A exec+tar (`/eos`-only) | B archive→rootfs→move |
| --- | --- | --- |
| 0.05 M | **46.9 ms** | 385.8 ms |
| 3 M | **63.3 ms** | 402.2 ms |
| 20 M | **173.1 ms** | 535.7 ms |
| 40 M | **297.6 ms** | 701.0 ms |

Findings:
1. **exec+tar is the fast path** (2–8× faster than the archive endpoint route).
   The transport is *not* the problem.
2. **B's ~360 ms floor = one extra `adapter.exec`** call — `["/bin/bash","-lc",…]`
   is a *login* shell whose profile-sourcing startup dominates (amplified under
   qemu amd64-on-arm64). The cost is the **number of `bash -lc` round-trips**.
3. **Rule #1 (`/eos`-only) is free** — the only thing that would write outside
   `/eos` (B) is also the slow one. Strict `/eos`-only wins outright.

**Conclusion:** keep exec+tar; remove the redundant staging/`cp -a` round-trips
both stages wrap around it. Dropped alternatives: archive-endpoint-to-rootfs
(slower, violates rule #1) and TCP-stream-to-`eosd` (can't bootstrap `eosd` →
not unifiable; needs binary framing on a JSON-line protocol).

## 4. The new API

New module `backend/src/sandbox/host/sandbox_upload.py`:

```python
async def upload_into_sandbox(
    sandbox_id: str,
    content: bytes,
    *,
    path: str | None = None,
    mode: int = 0o644,
    unpack: bool = False,
    exec_fn: RawExecCallable,
    put_archive_fn: PutArchiveCallable,
) -> str:
    """Upload `content` into the sandbox — always under /eos — over the
    tmpfs-safe exec+tar transport. Returns the final in-sandbox path.

    unpack=False (default): `content` is ONE file's bytes, written at `path`
        with `mode`. `path` defaults to /eos/uploads/<uuid> when omitted and
        is returned to the caller.
    unpack=True: `content` is a tar archive; its tree is extracted INTO `path`
        (a directory). `mode` is ignored.

    Invariant (rule #1): `path` must resolve under /eos. A non-/eos `path`
        raises ValueError — the function never falls back to the Docker archive
        endpoint, which silently loses data behind the tmpfs mount.

    Atomic publish (rename into a live name) stays with the caller; this
        primitive only materializes bytes under /eos.
    """
```

Semantics:

- **Single transport.** Internally always builds/forwards a tar and calls
  `put_archive_fn(tar_stream=…, dest_dir=…)`, i.e. `_put_archive_into_eos_tmpfs`
  (exec+tar). `unpack` only decides whether `content` is wrapped (file) or
  forwarded as-is (already a tar).
- **`path` optional.** `None` → scratch under `EOS_REMOTE_ROOT/uploads/<uuid>`,
  returned to the caller. Named `path` → that exact location (its parent dir is
  `mkdir -p`'d).
- **`/eos`-only is structural.** The `path` validation is the explicit form of
  the silent-loss guard that today lives implicitly in the adapter routing.
- **No publish policy inside.** Callers keep their existing atomic step (dir
  swap, file swap, or marker-gated in-place merge) — see §5.3.

### 4.1 What it replaces

| Removed | Replaced by |
| --- | --- |
| `plugin_install.py::_upload_entries` (staging + `cp -a` + `rm`) | `upload_into_sandbox(unpack=True, path=<staging dir>)` + caller `mv` |
| `PLUGIN_ARCHIVE_STAGING_ROOT = /eos/plugin-archives` intermediate | (deleted — direct extract into the final `.staging` dir) |
| `runtime_bundle.py` runtime staging + `cp -a` merge | `upload_into_sandbox(unpack=True, path=/eos/daemon)` (in-place; daemon not yet running) |
| `runtime_bundle.py` eosd `cat staging/eosd > eosd` | `upload_into_sandbox(unpack=False, mode=0o755, path=<staging>/eosd)` + caller `mv` |

## 5. Detailed workflow

### 5.1 Plugin setup stage — `ensure_installed` → bundle upload

```
BEFORE  (per bundle: 4 host round-trips, 2 full-tree tmpfs copies)
 ┌ exec  rm -rf /eos/plugin-archives/<uuid> && mkdir -p <uuid> /eos   bash -lc
 │ PUT   put_archive(dest=/eos/plugin-archives/<uuid>)                tar exec → tree #1
 │ exec  cp -a /eos/plugin-archives/<uuid>/. /eos/                    bash -lc → tree #2 (re-copy)
 │ exec  rm -rf /eos/plugin-archives/<uuid>                           bash -lc
 └ exec  mv  …<name>.staging  …/catalog/<name>                        bash -lc  (atomic publish)

AFTER   (per bundle: 3 host round-trips, 1 tmpfs copy)
 ┌ (mkdir folded into upload_into_sandbox)
 │ CALL  upload_into_sandbox(content=bundle_tar, unpack=True,
 │                           path=…/catalog/<name>.staging)           tar exec → tree ONCE, in /eos
 └ exec  mv  …<name>.staging  …/catalog/<name>                        bash -lc  (atomic publish)
```

The 40 MB LSP `node.tar.xz` package upload follows the identical shape; AFTER
removes one full 40 MB `cp -a` re-copy inside the tmpfs.

### 5.2 Daemon setup stage — `ensure_runtime_uploaded`

```
RUNTIME BUNDLE (.py tree)
 BEFORE (4 round-trips, cp -a merge)              AFTER (3 round-trips, in-place)
  exec  test marker                                exec  test marker
  exec  rm -rf staging && mkdir staging /eos/daemon CALL upload_into_sandbox(bundle_tar, unpack=True,
  PUT   put_archive(dest=staging)                        path=/eos/daemon)   # daemon not running → safe
  exec  cp -a staging/. /eos/daemon/ && rm && marker exec printf marker

EOSD BINARY (single file)
 BEFORE (~6 round-trips, cat> re-copy)            AFTER (3 round-trips, mv swap)
  exec  test marker; exec mkdir; exec mkdir staging exec test marker
  PUT   put_archive(dest=staging)                  CALL upload_into_sandbox(eosd_bytes, unpack=False,
  exec  cat staging/eosd > eosd  (ETXTBSY risk)          mode=0o755, path=/eos/daemon/.eosd-stg/eosd)
  exec  marker && eosd --version                   exec mv …/.eosd-stg/eosd /eos/daemon/eosd && chmod
                                                         && marker && eosd --version
```

### 5.3 Publish strategy stays with the caller (3 idioms)

| Upload | `unpack` | Publish idiom | Why |
| --- | --- | --- | --- |
| plugin bundle | `True` | `mv <staging-dir> <install-dir>` | fresh dir → atomic dir swap |
| LSP package | `True` | `mv` into `/eos/plugin-packages/<name>` | fresh dir → atomic dir swap |
| runtime bundle | `True` | in-place into `/eos/daemon`; marker gates | daemon not yet spawned; content-hashed |
| eosd binary | `False` | `mv <staging>/eosd /eos/daemon/eosd` | atomic file swap, avoids ETXTBSY + re-copy |

## 6. Diff table (before / after)

| Dimension | Before | After |
| --- | --- | --- |
| Transport primitive | `put_archive` → exec+tar | **same** (kept; 2–8× faster than archive route) |
| Host round-trips / upload | 4–6 (mostly `bash -lc` login shells) | **3** |
| Full-tree tmpfs copies / upload | 2 (tar extract **+** `cp -a`) | **1** (tar extract) |
| 40 MB node pkg tmpfs writes | ~80 MB (extract + cp) | **~40 MB** (extract once) |
| Intermediate staging dirs | 2 (`/eos/plugin-archives/<uuid>` + `<x>.staging`) | **1** (`<x>.staging`) |
| eosd publish | `cat staging/eosd > eosd` (re-copy, ETXTBSY) | `mv` (atomic rename) |
| Writes outside `/eos` | none (already `/eos`-only) | **none** + rootfs detour structurally impossible |
| `/eos`-only guard | implicit (adapter routing) | **explicit** (`path` validation in API) |
| Atomic publish | yes | **yes — preserved** |
| Upload call-site code | bespoke per stage | **one `upload_into_sandbox`** |
| `PutArchiveCallable` / `RawExecCallable` protocols | duplicated in 2 modules | **one shared definition** |
| Wire / daemon protocol | — | **unchanged** |

## 7. File / folder structure

```
backend/src/sandbox/
├── host/
│   ├── sandbox_upload.py          # NEW — upload_into_sandbox(), the unified primitive
│   ├── upload_protocols.py        # NEW (optional) — shared RawExecCallable / PutArchiveCallable
│   ├── paths.py                   # EDIT — add UPLOAD_SCRATCH_DIR = /eos/uploads
│   ├── runtime_bundle.py          # EDIT — daemon-setup callers use upload_into_sandbox
│   └── daemon_client.py           # unchanged
├── api/
│   └── plugin_install.py          # EDIT — remove _upload_entries + PLUGIN_ARCHIVE_STAGING_ROOT
│                                  #        plugin-setup callers use upload_into_sandbox
└── provider/docker/
    └── adapter.py                 # EDIT — 1-line guard comment on the /eos branch (:405)

backend/tests/unit_test/test_sandbox/
├── test_sandbox_upload.py         # NEW — unit tests for upload_into_sandbox (fakes for exec/put_archive)
├── test_plugin_install.py         # EDIT — assert direct-extract + mv, drop plugin-archives expectations
└── test_provider/test_docker_adapter.py   # unchanged (transport untouched)

docs/plans/
└── unified_sandbox_upload_PLAN.md # THIS DOCUMENT
```

## 8. Related classes and fields

### 8.1 New

| Symbol | Location | Notes |
| --- | --- | --- |
| `upload_into_sandbox(...)` | `sandbox/host/sandbox_upload.py` | the unified primitive (§4) |
| `RawExecCallable`, `PutArchiveCallable` | `sandbox/host/upload_protocols.py` | one home; both stages import from here |
| `UPLOAD_SCRATCH_DIR = f"{EOS_REMOTE_ROOT}/uploads"` | `sandbox/host/paths.py` | default scratch for `path=None` |

### 8.2 Existing — kept (transport)

| Symbol | Location | Role |
| --- | --- | --- |
| `DockerProviderAdapter.put_archive` | `provider/docker/adapter.py:394` | routes `/eos*` to tmpfs-safe path |
| `DockerProviderAdapter._put_archive_into_eos_tmpfs` | `adapter.py:422` | exec+tar streaming (the fast transport) |
| `DockerProviderAdapter.exec` | `adapter.py:344` | `bash -lc` round-trips (mkdir/mv/marker) |
| `EOS_REMOTE_ROOT` `/eos`, `BUNDLE_REMOTE_DIR` `/eos/daemon`, `EOSD_REMOTE_PATH`, `BUNDLE_HASH_MARKER`, `EOSD_SHA_MARKER` | `sandbox/host/paths.py` | path/marker constants |

### 8.3 Existing — edited (call sites)

| Symbol | Location | Change |
| --- | --- | --- |
| `ensure_installed` | `api/plugin_install.py:164` | unchanged signature; body calls new primitive |
| `_upload_and_run_setup` | `plugin_install.py:300` | `_upload_entries` → `upload_into_sandbox` + `mv` |
| `_upload_entries` | `plugin_install.py:475` | **deleted** |
| `_build_plain_tar`, `_prefixed_entries` | `plugin_install.py:252,517` | kept (build the tree tar passed as `content`) |
| `PLUGIN_ARCHIVE_STAGING_ROOT` | `plugin_install.py:54` | **deleted** |
| `PLUGIN_BUNDLE_REMOTE_ROOT`, `PLUGIN_PACKAGE_REMOTE_ROOT` | `plugin_install.py:52,53` | kept |
| `_ensure_runtime_uploaded_with_exec` | `runtime_bundle.py:198` | staging+`cp -a` → `upload_into_sandbox(unpack=True, path=/eos/daemon)` |
| `_ensure_eosd_uploaded` | `runtime_bundle.py:254` | `cat>` → `upload_into_sandbox(mode=0o755)` + `mv` |
| `_tar_file_at_path`, `_runtime_bundle_tar_bytes` | `runtime_bundle.py:369,92` | kept (produce `content`) |
| `PluginManifest` | `plugins/core/manifest.py` | unchanged (source of bundle files) |

### 8.4 Field-level inputs the primitive consumes

| Field | Type | Source |
| --- | --- | --- |
| `content` | `bytes` | `_runtime_bundle_tar_bytes()`, `_build_plain_tar(...)`, `eosd` file bytes, `node.tar.xz`/`pyright.tgz` bytes |
| `path` | `str \| None` | `/eos/...` staging dirs / file paths from `paths.py` + plugin roots |
| `mode` | `int` | `0o755` for `eosd`, `0o644` default |
| `unpack` | `bool` | `True` for `.py`/bundle trees, `False` for single binaries/packages |

## 9. Implementation steps (surgical, ordered)

1. Add `sandbox/host/upload_protocols.py` with the two `Protocol` classes;
   re-export from the two current sites to avoid churn, then switch imports.
2. Add `UPLOAD_SCRATCH_DIR` to `paths.py`.
3. Add `sandbox/host/sandbox_upload.py::upload_into_sandbox` (§4). Internally:
   `validate /eos → mkdir parent → (wrap file | forward tar) → put_archive_fn`.
4. Rewire `plugin_install.py`: delete `_upload_entries` + `PLUGIN_ARCHIVE_STAGING_ROOT`;
   `_upload_and_run_setup` calls `upload_into_sandbox(unpack=True, path=<staging>)`
   then `mv` publishes. Same for the LSP package upload.
5. Rewire `runtime_bundle.py`: runtime bundle direct in-place extract; eosd via
   `upload_into_sandbox(mode=0o755)` + `mv`.
6. Add the 1-line guard comment at `adapter.py:405` citing silent-loss.
7. Tests: §10.

## 10. Verification

- `cargo`-free; this is Python. Run:
  - `uv run pytest backend/tests/unit_test/test_sandbox/test_sandbox_upload.py`
  - `uv run pytest backend/tests/unit_test/test_sandbox/test_plugin_install.py`
  - `uv run pytest backend/tests/unit_test/test_sandbox/test_provider/test_docker_adapter.py`
- Live re-check of the round-trip drop with `/tmp/bench_eos_upload.py`
  (A path unchanged; confirms transport parity) + a new end-to-end stage timer
  (count `bash -lc` execs before/after).
- Success criteria: byte-identical `/eos` content; ≥1 fewer round-trip and zero
  `cp -a` per upload; `path` outside `/eos` raises; no Docker archive-endpoint
  call for any `/eos` destination.

## 11. Risks / open decisions

- **File-vs-tree signal (`unpack`).** One typed bool, intrinsic to "is `content`
  a tar or a file." Alternative considered: two functions
  (`upload_file_into_sandbox` / `upload_tree_into_sandbox`). Single function with
  `unpack` chosen for the requested one-name API; revisit if a third mode appears.
- **Runtime-bundle in-place extract** relies on the daemon not running during
  daemon-setup. True today (`ensure_runtime_uploaded` precedes daemon spawn). If
  that ordering changes, switch the runtime bundle to staging-dir + `cp -a`
  merge (still one extract, one merge) or per-file `mv`.
- **Rust migration.** `backend/src` is legacy (CLAUDE.md). The same shape —
  keep exec+tar, drop the staging dance — should land in
  `agent-core/crates/eos-sandbox-host` when that path owns provisioning. Out of
  scope here; flagged for the migration.
- **Protocol dedup.** Moving `RawExecCallable`/`PutArchiveCallable` to a shared
  module touches imports in 2 files; low risk, improves the duplication noted in
  §8.
```
