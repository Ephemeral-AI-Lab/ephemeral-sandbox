# CLI Observability — Side Spec (operation manual + command matrix)

Status: ready-to-implement (additive to the main spec).

The formal CLI surface for observability, written in the repo's **operation-family
/ operation-spec** format (`sandbox-protocol/src/cli_operation_spec.rs`,
`catalog.rs`, rendered by `help.rs`). Companion to `README.md` — the data model,
views, and the `get_observability` transport op.

This document is the source of truth for: **(§2)** the `CliOperationFamilySpec` +
`CliOperationSpec`s, **(§3)** the rendered `help` manual those specs produce, and
**(§4)** every command permutation with its exact output shape.

---

## 1. Integration — how observability becomes a CLI family

Runtime/manager operations are declared as `CliOperationSpec`s grouped into
`CliOperationFamilySpec`s and exposed per **execution space** (`Manager` |
`Runtime`; `catalog.rs:8`). Observability is neither: it is a **read** served by
the daemon op `get_observability` (`README.md` §7), not a runtime mutation
dispatched through `sandbox_runtime::dispatch_operation`.

To surface it as **`sandbox-cli observability <view> …`** (the form chosen in the
main spec), introduce a third, read-only execution space and build its catalog
from the specs in §2. Three small deltas:

1. `CliOperationExecutionSpace::Observability` (`catalog.rs:8`) +
   `operation_execution_space_name` arm `"observability"`.
2. `catalog_title` arm `"Sandbox Observability Help"` (`help.rs:240`).
3. An observability catalog (`CliOperationCatalog::new(Observability,
   &[&OBSERVABILITY_FAMILY], &[…5 specs…])`), served read-only.

**One transport, five views.** All five operations resolve to the single daemon op
`get_observability`; the operation `name` *is* the `view` value, and the CLI flags
map to that op's params (`trace`, `name`, `scope`, `window_ms`, `since_ms`,
`kind`). The specs exist so each view gets its own subcommand, help page, and
args.

---

## 2. The operation family + specs (exact format)

Mirrors `cli_definition/command_operations.rs`. `--sandbox-id` is required on every
operation (it selects the target daemon) and is shared via one `ArgSpec`.

```rust
use sandbox_protocol::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationFamilySpec, CliOperationSpec, CliSpec,
};

pub const OBSERVABILITY_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "observability",
    title: "Observability",
    summary: "Inspect traces, events, and resource stats for a sandbox.",
    description: "Read a sandbox's observability stream — span waterfalls, domain \
events, cgroup/disk resource series, and live state, over the daemon \
get_observability op.",
};

const SANDBOX_ID_ARG: ArgSpec = ArgSpec::required(
    "sandbox_id",
    ArgKind::String,
    "Target sandbox id (selects the daemon to query).",
    Some(ArgCliSpec { flag: Some("--sandbox-id"), positional: None }),
);

// ── snapshot ────────────────────────────────────────────────────────────────
const SNAPSHOT_SPEC: CliOperationSpec = CliOperationSpec {
    name: "snapshot",
    family: "observability",
    summary: "Show live sandbox state.",
    description: "Show current state from the runtime registry: sandbox lifecycle \
state, workspaces (with layer counts), in-flight executions, and the latest \
resource sample per scope. Served live; does not read the log.",
    args: &[SANDBOX_ID_ARG],
    cli: Some(CliSpec {
        path: &["observability", "snapshot"],
        usage: "sandbox-cli observability snapshot --sandbox-id ID",
        examples: &["sandbox-cli observability snapshot --sandbox-id eos-abc"],
    }),
    related: &["trace", "cgroup"],
};

// ── trace ───────────────────────────────────────────────────────────────────
const TRACE_SPEC: CliOperationSpec = CliOperationSpec {
    name: "trace",
    family: "observability",
    summary: "Render one flow as a span waterfall.",
    description: "Fold the log into a span waterfall for one trace: spans nested by \
parent, offset by start, with attached events inline. Use --id last for the most \
recent root trace.",
    args: &[
        SANDBOX_ID_ARG,
        ArgSpec::optional(
            "trace",
            ArgKind::String,
            "Trace id to render, or 'last' for the most recent root trace.",
            Some("last"),
            Some(ArgCliSpec { flag: Some("--id"), positional: None }),
        ),
    ],
    cli: Some(CliSpec {
        path: &["observability", "trace"],
        usage: "sandbox-cli observability trace --sandbox-id ID [--id TRACE|last]",
        examples: &[
            "sandbox-cli observability trace --sandbox-id eos-abc --id req-7f3",
            "sandbox-cli observability trace --sandbox-id eos-abc --id last",
        ],
    }),
    related: &["events", "raw", "snapshot"],
};

// ── events ──────────────────────────────────────────────────────────────────
const EVENTS_SPEC: CliOperationSpec = CliOperationSpec {
    name: "events",
    family: "observability",
    summary: "List domain-fact events across traces.",
    description: "Fold the log into a flat, cross-trace stream of point-in-time \
events (lease, errors, …), newest first. Filter by exact name and/or a \
start timestamp.",
    args: &[
        SANDBOX_ID_ARG,
        ArgSpec::optional(
            "name",
            ArgKind::String,
            "Filter to events with this exact name (e.g. lease.acquired).",
            None,
            Some(ArgCliSpec { flag: Some("--name"), positional: None }),
        ),
        ArgSpec::optional(
            "since_ms",
            ArgKind::Integer,
            "Only events at or after this unix-ms timestamp.",
            None,
            Some(ArgCliSpec { flag: Some("--since-ms"), positional: None }),
        ),
    ],
    cli: Some(CliSpec {
        path: &["observability", "events"],
        usage: "sandbox-cli observability events --sandbox-id ID [--name NAME] [--since-ms MS]",
        examples: &[
            "sandbox-cli observability events --sandbox-id eos-abc",
            "sandbox-cli observability events --sandbox-id eos-abc --name lease.acquired",
        ],
    }),
    related: &["trace", "raw"],
};

// ── cgroup ──────────────────────────────────────────────────────────────────
const CGROUP_SPEC: CliOperationSpec = CliOperationSpec {
    name: "cgroup",
    family: "observability",
    summary: "Resource series for a scope (cpu/mem/io + disk).",
    description: "Fold the sample log for one scope into a time series with deltas: \
cgroup counters (cpu/mem/io from /sys/fs/cgroup) plus the disk sample (upperdir \
bytes/files) carried in the same record.",
    args: &[
        SANDBOX_ID_ARG,
        ArgSpec::optional(
            "scope",
            ArgKind::String,
            "Resource scope: 'sandbox' or a workspace id.",
            Some("sandbox"),
            Some(ArgCliSpec { flag: Some("--scope"), positional: None }),
        ),
        ArgSpec::optional(
            "window_ms",
            ArgKind::Integer,
            "Lookback window in milliseconds (max 600000).",
            Some("60000"),
            Some(ArgCliSpec { flag: Some("--window-ms"), positional: None }),
        ),
    ],
    cli: Some(CliSpec {
        path: &["observability", "cgroup"],
        usage: "sandbox-cli observability cgroup --sandbox-id ID [--scope SCOPE] [--window-ms MS]",
        examples: &[
            "sandbox-cli observability cgroup --sandbox-id eos-abc",
            "sandbox-cli observability cgroup --sandbox-id eos-abc --scope ws-1 --window-ms 60000",
        ],
    }),
    related: &["snapshot"],
};

// ── raw ─────────────────────────────────────────────────────────────────────
const RAW_SPEC: CliOperationSpec = CliOperationSpec {
    name: "raw",
    family: "observability",
    summary: "Print matching NDJSON log lines.",
    description: "Forward-scan the log and print matching records verbatim \
(newline-delimited JSON), for grep/jq. Filter by kind, trace, and start time.",
    args: &[
        SANDBOX_ID_ARG,
        ArgSpec::optional(
            "kind",
            ArgKind::String,
            "Filter by record kind: span | event | sample.",
            None,
            Some(ArgCliSpec { flag: Some("--kind"), positional: None }),
        ),
        ArgSpec::optional(
            "trace",
            ArgKind::String,
            "Filter to one trace id.",
            None,
            Some(ArgCliSpec { flag: Some("--trace"), positional: None }),
        ),
        ArgSpec::optional(
            "since_ms",
            ArgKind::Integer,
            "Only records at or after this unix-ms timestamp.",
            None,
            Some(ArgCliSpec { flag: Some("--since-ms"), positional: None }),
        ),
    ],
    cli: Some(CliSpec {
        path: &["observability", "raw"],
        usage: "sandbox-cli observability raw --sandbox-id ID [--kind K] [--trace ID] [--since-ms MS]",
        examples: &[
            "sandbox-cli observability raw --sandbox-id eos-abc --kind span --trace req-7f3",
        ],
    }),
    related: &["trace", "events"],
};
```

**Format notes.** Arg `name` is the wire param sent to `get_observability`; the
CLI flag is the spelling (e.g. `trace` ⇄ `--id`).

---

## 3. Rendered manual (what `help` prints)

Exactly what `render_catalog_help` / `render_operation_page` (`help.rs`) emit from
§2 — indentation and section order are load-bearing (asserted by
`gateway_cli.rs` tests).

### 3.1 `sandbox-cli observability help`

```text
Sandbox Observability Help

Observability
  Inspect traces, events, and resource stats for a sandbox.

  snapshot
    Show live sandbox state.

  trace
    Render one flow as a span waterfall.

  events
    List domain-fact events across traces.

  cgroup
    Resource series for a scope (cpu/mem/io + disk).

  layerstack
    Per-layer leasing/booking inventory, and stack series.

  raw
    Print matching NDJSON log lines.

Use:
  sandbox-cli observability help OPERATION
```

### 3.2 `sandbox-cli observability help snapshot`

```text
snapshot

Family
  Observability

Description
  Show current state from the runtime registry: sandbox lifecycle state, workspaces (with layer counts), in-flight executions, and the latest resource sample per scope. Served live; does not read the log.

Usage
  sandbox-cli observability snapshot --sandbox-id ID

Arguments
  --sandbox-id string required
    Target sandbox id (selects the daemon to query).

Examples
  sandbox-cli observability snapshot --sandbox-id eos-abc

Related Operations
  trace
  cgroup
  layerstack
```

### 3.3 `sandbox-cli observability help trace`

```text
trace

Family
  Observability

Description
  Fold the log into a span waterfall for one trace: spans nested by parent, offset by start, with attached events inline. Use --id last for the most recent root trace.

Usage
  sandbox-cli observability trace --sandbox-id ID [--id TRACE|last]

Arguments
  --sandbox-id string required
    Target sandbox id (selects the daemon to query).
  --id string optional
    Trace id to render, or 'last' for the most recent root trace.
    Default: last

Examples
  sandbox-cli observability trace --sandbox-id eos-abc --id req-7f3
  sandbox-cli observability trace --sandbox-id eos-abc --id last

Related Operations
  events
  raw
  snapshot
```

### 3.4 `sandbox-cli observability help events`

```text
events

Family
  Observability

Description
  Fold the log into a flat, cross-trace stream of point-in-time events (lease, errors, …), newest first. Filter by exact name and/or a start timestamp.

Usage
  sandbox-cli observability events --sandbox-id ID [--name NAME] [--since-ms MS]

Arguments
  --sandbox-id string required
    Target sandbox id (selects the daemon to query).
  --name string optional
    Filter to events with this exact name (e.g. lease.acquired).
  --since-ms integer optional
    Only events at or after this unix-ms timestamp.

Examples
  sandbox-cli observability events --sandbox-id eos-abc
  sandbox-cli observability events --sandbox-id eos-abc --name lease.acquired

Related Operations
  trace
  raw
```

### 3.5 `sandbox-cli observability help cgroup`

```text
cgroup

Family
  Observability

Description
  Fold the sample log for one scope into a time series with deltas: cgroup counters (cpu/mem/io from /sys/fs/cgroup) plus the disk sample (upperdir bytes/files) carried in the same record.

Usage
  sandbox-cli observability cgroup --sandbox-id ID [--scope SCOPE] [--window-ms MS]

Arguments
  --sandbox-id string required
    Target sandbox id (selects the daemon to query).
  --scope string optional
    Resource scope: 'sandbox' or a workspace id.
    Default: sandbox
  --window-ms integer optional
    Lookback window in milliseconds (max 600000).
    Default: 60000

Examples
  sandbox-cli observability cgroup --sandbox-id eos-abc
  sandbox-cli observability cgroup --sandbox-id eos-abc --scope ws-1 --window-ms 60000

Related Operations
  snapshot
```

### 3.6 `sandbox-cli observability help raw`

```text
raw

Family
  Observability

Description
  Forward-scan the log and print matching records verbatim (newline-delimited JSON), for grep/jq. Filter by kind, trace, and start time.

Usage
  sandbox-cli observability raw --sandbox-id ID [--kind K] [--trace ID] [--since-ms MS]

Arguments
  --sandbox-id string required
    Target sandbox id (selects the daemon to query).
  --kind string optional
    Filter by record kind: span | event | sample.
  --trace string optional
    Filter to one trace id.
  --since-ms integer optional
    Only records at or after this unix-ms timestamp.

Examples
  sandbox-cli observability raw --sandbox-id eos-abc --kind span --trace req-7f3

Related Operations
  trace
  events
```

---

## 4. Command permutation matrix — exact output shapes

Every meaningful flag combination per subcommand, with the exact rendered shape.
All examples use sandbox `eos-abc` and the data from `README.md` §4, so shapes
line up across docs.

### 4.0 Global forms

| Command | Outcome |
|---|---|
| `sandbox-cli observability help` | §3.1 catalog page |
| `sandbox-cli observability help <view>` | §3.2–3.6 operation page |
| `sandbox-cli observability <view>` *(no `--sandbox-id`)* | `error: missing required --sandbox-id` |
| `sandbox-cli observability bogus --sandbox-id eos-abc` | `unknown observability operation: bogus` + `Did you mean:` suggestions (help.rs) |

### 4.1 `snapshot` — 1 form

```console
$ sandbox-cli observability snapshot --sandbox-id eos-abc
sandbox eos-abc   state ready

  workspaces
    ws-7   active   profile=default   mounts 4   upper 156KB
    ws-9   active   profile=default   mounts 3   upper  88KB

  in-flight executions            (from runtime registry, not the log)
    ns-42  namespace.exec.shell   trace req-9a1   running 7.3s   ws-7

  resources (latest)
    sandbox   cpu 12.3s   mem 41MB / 256MB
    ws-7      cpu  4.1s   mem 18MB        disk 156KB (320 files)
```

### 4.2 `trace` — by id / last / unknown

```console
$ sandbox-cli observability trace --sandbox-id eos-abc --id req-7f3
trace req-7f3   sandbox eos-abc   wall 4.30s   (call returned at 1.05s)

  +00.000  daemon.dispatch op=exec_command                 1051ms  ✓
  +00.002   └ command.exec one_shot                        1048ms  ✓
  +00.003      ├ workspace_session.create                    38ms  ✓
  +00.009      │   • lease.acquired r5
  +00.013      │   └ namespace.exec.mount_overlay            27ms  ✓
  +00.042      ├ namespace.exec.shell           [async]    4231ms  ✓ exit0
  +00.055      │   └ namespace.runner.spawn_child            6ms  ✓   [Phase B]
  +04.275      └ workspace_session.destroy one_shot         25ms  ✓
  +04.295         • lease.released r5
```

```console
$ sandbox-cli observability trace --sandbox-id eos-abc        # --id defaults to "last"
trace req-9a1   sandbox eos-abc   wall — (in flight)   1 span open

  +00.000  command.exec ws-7                                1020ms  ✓
  +00.020   └ namespace.exec.shell  ns-42  [async]          running  (live, from registry)
```

```console
$ sandbox-cli observability trace --sandbox-id eos-abc --id nope
trace nope   sandbox eos-abc   (no records — unknown trace, or rotated out)
```

### 4.3 `events` — none / --name / --since-ms / both

```console
$ sandbox-cli observability events --sandbox-id eos-abc
events  sandbox eos-abc   12 matched (newest first)

  ts        name                trace     parent  attrs
  +04.295   lease.released      req-7f3   d-6     revision=r5
  +00.009   lease.acquired      req-7f3   d-2     revision=r5 owner=req-7f3
  …
```

```console
$ sandbox-cli observability events --sandbox-id eos-abc --name lease.released
events  sandbox eos-abc   name=lease.released   2 matched

  ts        trace     parent  attrs
  +04.295   req-7f3   d-6     revision=r5
  +18.130   req-9c2   d-31    revision=r7
```

```console
$ sandbox-cli observability events --sandbox-id eos-abc --since-ms 1719500004280
events  sandbox eos-abc   since 1719500004280   1 matched

  ts        name                trace     parent  attrs
  +04.295   lease.released      req-7f3   d-6     revision=r5
```

```console
$ sandbox-cli observability events --sandbox-id eos-abc --name lease.acquired --since-ms 1719500000000
events  sandbox eos-abc   name=lease.acquired since 1719500000000   1 matched

  ts        trace     parent  attrs
  +00.009   req-7f3   d-2     revision=r5 owner=req-7f3
```

### 4.4 `cgroup` — default scope / workspace / window cap error

```console
$ sandbox-cli observability cgroup --sandbox-id eos-abc          # scope=sandbox, window=60000 (defaults)
scope sandbox   window 60s   (Δ computed at read)

  t(+s)   cpu_total   Δcpu      mem_cur    io_w      Δio_w
  00.0    10.20s       –        38.0MB     4.0MB       –
  30.0    12.30s    +2.10s      41.0MB     5.2MB    +1.2MB
```

```console
$ sandbox-cli observability cgroup --sandbox-id eos-abc --scope ws-1 --window-ms 60000
scope ws-1   window 60s   (Δ computed at read)

  t(+s)   cpu_total   Δcpu      mem_cur    disk        Δdisk
  00.0     1.00s        –       18.0MB     1.20MB        –
  10.0     4.10s     +3.10s     21.0MB     1.32MB     +120KB
  20.0     4.25s     +0.15s     20.5MB     1.32MB        +0
```

```console
$ sandbox-cli observability cgroup --sandbox-id eos-abc --scope ws-1 --window-ms 999999999
error: window_ms exceeds max (600000)
```

### 4.5 `raw` — none / --kind / --trace / --since-ms / combined

```console
$ sandbox-cli observability raw --sandbox-id eos-abc --kind span --trace req-7f3
{"ts":1719500000040,"kind":"span","trace":"req-7f3","span":"d-4","parent":"d-2","name":"namespace.exec.mount_overlay","dur_ms":27.0,"status":"completed"}
{"ts":1719500001050,"kind":"span","trace":"req-7f3","span":"d-1","parent":"d-0","name":"command.exec","dur_ms":1048.0,"status":"completed","attrs":{"one_shot":true}}
{"ts":1719500004273,"kind":"span","trace":"req-7f3","span":"d-5","parent":"d-1","name":"namespace.exec.shell","dur_ms":4231.0,"status":"completed","attrs":{"exec_id":"ns-9","async":true,"exit_code":0}}
```

```console
$ sandbox-cli observability raw --sandbox-id eos-abc --kind sample --since-ms 1719500010000
{"ts":1719500010000,"kind":"sample","scope":"ws-1","cpu_usec":4100000,"mem_cur":21000000,"disk_bytes":1320000,"files":340}
{"ts":1719500020000,"kind":"sample","scope":"ws-1","cpu_usec":4250000,"mem_cur":20500000,"disk_bytes":1320000,"files":340}
```

```console
$ sandbox-cli observability raw --sandbox-id eos-abc            # no filter: all lines, bounded by the size cap
… every record in the log, append order …

$ sandbox-cli observability raw --sandbox-id eos-abc --trace nope
                                                               # empty: no matching lines (exit 0)
```

### 4.7 Permutation coverage summary

| view | flags | distinct forms shown |
|---|---|---|
| `snapshot` | — | 1 |
| `trace` | `--id {id\|last\|unknown}` | 4 |
| `events` | `--name`?, `--since-ms`? | 4 |
| `cgroup` | `--scope`?, `--window-ms`? (+cap error) | 3 |
| `raw` | `--kind`?, `--trace`?, `--since-ms`? | 4 |
| global | `help`, `help <view>`, missing/unknown | 4 |

---

## 5. Notes & reconciliation

- **Flag conventions** follow the codebase (kebab-case, time args end in `-ms`):
  `--sandbox-id`, `--window-ms`, `--since-ms`. These are canonical; the looser
  spellings in early `README.md` §7 prose (`--window`, `--since`) defer to this
  file.
- **`get_observability` params** map 1:1 from flags: `view` = subcommand name,
  `trace` ⇄ `--id`/`--trace`, `name`, `scope`, `window_ms`, `since_ms`, `kind`.
  `--sandbox-id` is CLI routing, not an op param.
- **Publish is a span, not an event.** `layerstack.publish` is no longer a
  point-in-time event: a real publish is the sync span `layerstack.publish`
  (`attrs{base, revision, layers_added, bytes, no_op}`; `status=error` +
  `attrs.reason="manifest_conflict"` on a rejected publish — the old
  `layerstack.publish_rejected` event is gone). One-shot teardown never publishes
  (it evicts the upperdir and releases the lease), so Case A's tail is
  `lease.released` alone, carrying the same revision as `lease.acquired`. The
  cross-trace publish audit therefore moves off `events --name layerstack.publish`
  to `raw --kind span` filtered on `name == "layerstack.publish"` (jq); the
  `events` view still serves `lease.*` and the other domain facts. The capacity
  columns (`base`/`revision`/`layers_added`/`bytes`) survive verbatim as span
  attrs.
- **`trace` dispatch duration is the yield window, not I/O cost.** A
  `daemon.dispatch op=write_command_stdin` / read span measures the
  `yield_time_ms` poll window, not the write/read cost; read/write poll-loop
  dispatches surface as single-node traces, kept honest by config-gating emission
  and the root-status fix (a faulted `Response` colors the root span `error`, not
  a green ✓). A Ctrl-D that ends a one-shot attributes the teardown tail to the
  originating exec trace by design — the model is a tree, not a DAG.
- **Read filters own their values.** The daemon-side `RawFilter` holds owned
  `Option<String>` fields and derives `Default`, so a view folds its filter as
  `RawFilter { kind: Some("event".into()), ..Default::default() }`; the `events`
  view reuses `scan()`'s already-parsed `Event` records rather than re-parsing
  `raw`'s NDJSON lines.
- **Testing**: add catalog/help golden tests mirroring `gateway_cli.rs`
  (`Family\n  Observability`, each operation page) and a per-view request-builder
  test asserting flags map to the right `get_observability` params.
