# Workspace Process Topology Observability

Status: Draft  
Owners: sandbox-runtime, sandbox-observability, sandbox-manager, sandbox-console  
Target route: `/sandboxes/:sandboxId/observability/cgroup`  
Target operation: `cgroup`  
Live E2E spec: [`ephemeral-sandbox-test/e2e/observability/cgroup/test_spec.md`](../../ephemeral-sandbox-test/e2e/observability/cgroup/test_spec.md)

## Summary

Replace the current delegated-cgroup topology view with workspace process topology derived from Linux `/proc` namespace identity.

Every open workspace already has a namespace holder process. The runtime knows the stable mapping from `workspace_id` to `holder_pid`. A process belongs to that workspace when its PID namespace matches the holder's `pid_for_children` namespace and its mount namespace matches the holder's mount namespace. `/proc/<pid>/cgroup` remains useful diagnostic metadata, but neither writable cgroups nor one child cgroup per workspace is required.

The existing public URL and operation name stay stable. The console presents the route as **Processes** and renders workspace sessions, their namespace init, and their current processes. Read-only cgroup mounts, a root cgroup such as `0::/`, or the absence of delegated child cgroups must not produce an `unavailable` topology.

## Problem

The current topology collector treats a writable/delegated cgroup subtree as the source of truth. That assumption does not hold for common Docker deployments:

- Docker may mount `/sys/fs/cgroup` read-only inside the sandbox container.
- The daemon and all workspace processes may legitimately report the same cgroup membership.
- `CAP_SYS_ADMIN` does not guarantee that the container received a writable delegated cgroup subtree.
- Docker Desktop runs Linux containers in a VM and controls cgroup delegation independently of the image.

As a result, the UI can report:

> sandbox daemon did not report cgroup topology

even though the daemon can read `/proc`, knows every workspace holder, and can determine process placement without mutating cgroups.

## Goals

- Report every live workspace by its EOS `workspace_id`.
- Assign live processes to workspaces using kernel namespace identity.
- Work with read-only cgroup mounts and without `CAP_SYS_ADMIN`.
- Depend only on Linux `/proc` facilities already required by the namespace runner.
- Preserve the existing `cgroup` operation and `/observability/cgroup` route.
- Expose `/proc/<pid>/cgroup` membership as optional diagnostics on each process.
- Distinguish an idle workspace from unavailable topology.
- Handle normal `/proc` process races without failing the whole response.
- Keep collection on demand, bounded, deterministic, and free of background work.

## Non-goals

- Creating, delegating, or managing a writable cgroup per workspace.
- Enforcing per-workspace CPU, memory, PID, or I/O limits.
- Replacing manager-owned sandbox resource series on the Resources page.
- Producing lifetime-accurate per-workspace resource accounting. Processes that exit between samples are not recoverable from `/proc` alone.
- Supporting native Windows containers. EOS workspace isolation already depends on Linux namespaces. Linux containers on Docker Desktop for macOS or Windows are supported.
- Reading process environment variables or exposing command arguments.

## Support contract

The topology is image-distribution independent: it reads the container's kernel-provided procfs, not image packages or init-system APIs. It is expected to work on Linux Docker Engine and Linux containers under Docker Desktop when the existing EOS namespace runner works.

Required runtime facilities:

- procfs mounted and readable at `/proc`;
- readable namespace handles under `/proc/<pid>/ns`;
- readable `/proc/<pid>/status` and `/proc/<pid>/comm` for processes visible to the daemon;
- the workspace holder mapping retained by the running daemon.

Writable `/sys/fs/cgroup`, cgroup namespace mode, cgroup v1 versus v2, and `CAP_SYS_ADMIN` are not topology prerequisites.

## Source of truth

### Workspace identity

`sandbox-runtime-workspace` already retains:

```text
WorkspaceSessionId -> MountedWorkspace { holder_pid, namespace fds, ... }
```

The daemon must pass the `workspace_id` and `holder_pid` for each live workspace into the proc topology collector. The collector must not infer a workspace ID from a PID, process name, environment variable, cgroup path, or filesystem path.

### Namespace identity

For a workspace holder `H`:

- expected PID namespace: `stat(/proc/H/ns/pid_for_children)`;
- expected mount namespace: `stat(/proc/H/ns/mnt)`.

For a candidate process `P`:

- actual PID namespace: `stat(/proc/P/ns/pid)`;
- actual mount namespace: `stat(/proc/P/ns/mnt)`.

The namespace key is `(st_dev, st_ino)`, not parsed symlink text. `readlink` text may be returned as diagnostic data, but it is not the matching primitive.

`P` belongs to a workspace only when both namespace keys match. Requiring both prevents accidental assignment when a namespace is shared independently. The holder's `pid_for_children` is intentional: the holder itself remains in its parent PID namespace, while its namespace init and all later workspace processes enter the child PID namespace.

This relationship survives fork, daemonization, reparenting, and a browser disconnect. It does not depend on which client started the command.

### Cgroup membership

Read all non-empty lines from `/proc/P/cgroup` verbatim into `cgroup_memberships`. This supports both cgroup v1 and v2. Missing or unreadable membership makes this field empty and adds no top-level failure.

Cgroup membership is never used to decide `workspace_id` in this feature.

## Collection algorithm

Collection is performed once per explicit topology request:

1. Obtain one consistent runtime workspace snapshot containing `(workspace_id, holder_pid)`.
2. For each holder, stat `pid_for_children` and `mnt` and build a reverse map from the two namespace keys to the workspace.
3. Enumerate numeric entries in `/proc` once.
4. For each candidate PID, stat its `pid` and `mnt` namespace handles.
5. Skip candidates whose namespace pair is not present in the reverse map.
6. For matches, read bounded metadata from `status`, `comm`, and `cgroup`.
7. Group processes by workspace, sort workspaces by `workspace_id`, and sort processes by host PID.
8. Return all open workspaces, including those with no workload processes.

The existing mount-namespace scan in `sandbox-runtime-namespace-execution` quiesce behavior is precedent for safe numeric `/proc` enumeration. Shared neutral parsing helpers may be extracted, but observability must not depend on execution-policy code.

### Process metadata

For each matched process, collect:

- host/container PID from the `/proc` directory name;
- namespace PID from the last numeric value of `NSpid` in `/proc/<pid>/status`;
- parent PID from `PPid`;
- one-character Linux process state from `State`;
- process name from `/proc/<pid>/comm`;
- all readable cgroup membership lines.

Do not read `environ`. Do not return `cmdline` in v1 of this contract. `comm` is bounded by the kernel and is enough for an operational topology view without exposing arguments that may contain secrets.

### Workspace state

- `idle`: no matched process other than namespace PID 1;
- `active`: at least one matched process has namespace PID other than 1;
- `partial`: the holder is still part of the runtime snapshot but its namespace identity could not be read, or collection was truncated before its complete process set was known.

The namespace init is returned with `kind: "namespace_init"`. Every other row uses `kind: "process"`.

### Bounds

- Scan numeric proc entries once; do not repeatedly scan per workspace.
- Return at most 512 process rows across the sandbox.
- Return at most 16 structured collection warnings.
- Bound all proc file reads; `status` and `cgroup` reads must reject or truncate unexpectedly large data.
- Set `truncated: true` when the process-row cap is reached.
- Do not cache PIDs between requests.
- Do not add a topology background task or persistent state.

The collector is `O(number of open workspaces + visible processes)` in time and bounded by the response caps in retained memory.

## Public response contract

Keep the existing response envelope:

```json
{
  "view": "cgroup",
  "scope": "sandbox",
  "series": [],
  "topology": {}
}
```

Replace the delegated-cgroup topology payload with schema version 2:

```json
{
  "schema_version": 2,
  "available": true,
  "source": "proc_namespaces",
  "error": null,
  "truncated": false,
  "warnings": [],
  "workspaces": [
    {
      "workspace_id": "workspace-session-1",
      "state": "active",
      "holder_pid": 42,
      "pid_namespace": "pid:[4026532781]",
      "mount_namespace": "mnt:[4026532779]",
      "processes": [
        {
          "pid": 151,
          "namespace_pid": 1,
          "parent_pid": 42,
          "name": "ns-init",
          "state": "S",
          "kind": "namespace_init",
          "cgroup_memberships": ["0::/"]
        },
        {
          "pid": 164,
          "namespace_pid": 3,
          "parent_pid": 151,
          "name": "sleep",
          "state": "S",
          "kind": "process",
          "cgroup_memberships": ["0::/"]
        }
      ]
    }
  ]
}
```

Contract rules:

- `schema_version` is exactly `2` for the new payload.
- `available` is true when procfs enumeration and the runtime workspace snapshot are available, including when `workspaces` is empty.
- `source` is `proc_namespaces` when available.
- `error` is only for a topology-wide failure.
- `warnings` contains bounded non-fatal race or parsing summaries; it must not contain proc file contents.
- `holder_pid` is included to make the mapping independently diagnosable and testable.
- namespace strings are diagnostics from `readlink`; matching still uses stat identity.
- `processes` is present for every workspace and may be empty for a partial holder race.
- cgroup membership may be empty and may contain several v1 controller lines.

The old `root`, `controllers`, and `groups` fields are removed from schema version 2. Console and browser fixtures must migrate in the same change. The public operation and route remain stable, which limits migration to response consumers.

## Availability and race behavior

Top-level `available: false` is reserved for failures that make the whole topology unknowable, such as:

- the runtime cannot provide its live workspace snapshot;
- `/proc` cannot be opened or numeric entries cannot be enumerated;
- platform is not Linux/procfs-backed.

The following must not make topology unavailable:

- `/sys/fs/cgroup` is read-only or absent;
- all processes report the same cgroup membership;
- the daemon has no delegated child cgroups;
- no workspace is open;
- an open workspace is idle;
- a candidate process exits during scanning;
- a process's `comm`, `status`, or `cgroup` file disappears during scanning.

`ENOENT`/`ESRCH` after a PID was enumerated is a normal proc race and is skipped. A holder that disappears while still present in the runtime snapshot produces a `partial` workspace and a warning. Other permission or parse failures are bounded warnings unless they prevent all namespace matching.

## Backend ownership and changes

### `sandbox-runtime-workspace`

- Retain the existing `WorkspaceSessionId -> holder_pid` source of truth.
- Expose the holder PID through the internal runtime snapshot path; do not add a second registry.
- Ensure a workspace disappears from the snapshot after successful destroy.

### `sandbox-runtime` operation

- Extend the internal workspace snapshot used by observability with `holder_pid`.
- Obtain workspace identity from the live session handle, not persisted launch state alone.
- Preserve existing workspace snapshot behavior for unrelated consumers.

### `sandbox-observability-telemetry`

- Add a proc workspace topology collector that accepts neutral inputs such as `{ workspace_id, holder_pid }`.
- Keep this crate independent of workspace/runtime crates.
- Keep cgroup resource sampling separate from process topology.
- Parse cgroup membership as optional raw lines and support both v1 and v2 formats.

### `sandbox-observability-query`

- Change the topology input port and response construction to schema version 2.
- Preserve `view: "cgroup"`, `scope`, and existing resource `series` semantics.
- Do not decide workspace membership in the query layer.

### `sandbox-daemon`

- Compose the runtime workspace snapshot with the neutral proc collector.
- Serve topology even when daemon cgroup setup reports no delegated root.
- Avoid additional runtime snapshots within one topology request where practical.

### `sandbox-manager`

For sandbox scope, the manager continues to own Docker-derived resource series. On the explicit `cgroup` operation it must also query the daemon for topology, then return:

- `series` from the manager resource ring;
- `topology` from the daemon proc collector.

The manager must not hardcode topology as unavailable. It must not introduce daemon wakeups into normal aggregate resource polling; the additional daemon request is limited to the explicit topology page/operation.

Manager-to-daemon transport failures remain a valid top-level unavailable response with an actionable error distinct from cgroup delegation.

### CLI

`sandbox-observability-cli --operation cgroup --scope sandbox --output json` remains the public backend contract used by live tests. No new CLI or direct daemon endpoint is required.

### Expected implementation touch points

| Area | Current touch point | Expected change |
| --- | --- | --- |
| Workspace identity | `crates/sandbox-runtime/workspace/src/session/state.rs` and workspace snapshot service | Carry the live handle's holder PID with the workspace ID |
| Runtime observability snapshot | `crates/sandbox-runtime/operation/src/observability.rs` | Add internal holder identity needed by the daemon adapter |
| Proc collection | `crates/sandbox-observability/telemetry/src/collect/` | Add neutral workspace-process collection and keep resource cgroup sampling separate |
| Query port/response | `crates/sandbox-observability/query/src/ports.rs` and `query.rs` | Emit topology schema version 2 |
| Daemon composition | `crates/sandbox-daemon/src/observability/adapter.rs` | Join runtime workspace inputs to the proc collector |
| Manager response | `crates/sandbox-manager/src/management/service/impls/resource_metrics.rs` | Merge manager series with daemon topology instead of synthesizing unavailable |
| Console contract | `web/console/src/api/observability.ts` | Replace delegated-cgroup types with process-topology types |
| Console page | `web/console/src/pages/sandbox/observability/CgroupView.tsx` | Render workspace and process placement |
| Console navigation | observability tab and shell route labels | Display Processes while preserving the route |
| Browser fixtures | `web/console/tests/browser/P07ObservabilityFixture.spec.ts` | Migrate fixtures and state coverage to schema version 2 |

## Console design

Keep `/observability/cgroup` addressable for bookmarks and compatibility, but change the visible tab and breadcrumb label from **Cgroups** to **Processes**.

Page content:

- title: **Workspace process topology**;
- subtitle: **Process placement from Linux `/proc` namespace identity**;
- source badge: **proc namespaces**;
- existing auto-refresh control;
- one card/section per workspace;
- status badge (`active`, `idle`, or `partial`), process count, holder PID, and namespace diagnostics;
- process table with Name, PID, Namespace PID, State, Kind, and Cgroup membership;
- optional cgroup membership displayed as monospace diagnostic text, not as hierarchy.

Empty and failure states:

- available with no workspace: **No workspace sessions are open.**
- idle workspace: render the workspace and namespace init, plus **No workload processes are running.**
- partial workspace: keep visible data and show a non-blocking warning in that workspace.
- top-level unavailable: show the actual proc/runtime/transport cause and retain auto-refresh.

Remove CPU, memory, and child-cgroup columns from this topology panel. Sandbox resource charts remain on Resources; this view must not imply that `/proc` process sums provide authoritative lifetime accounting.

Responsive behavior:

- desktop uses a compact table within each workspace card;
- narrow layouts stack process fields without horizontal page overflow;
- long workspace IDs and membership lines wrap or scroll within their value cell.

Accessibility and stable test hooks:

- the navigation remains a semantic Mantine tab;
- workspace status is conveyed by text as well as color;
- process tables have accessible column names;
- page root: `data-process-topology="true"`;
- workspace container: `data-workspace-id="<id>"`;
- process row: `data-process-pid="<pid>"`.

## Security and privacy

- Never read or expose `/proc/<pid>/environ`.
- Do not expose command arguments in this version.
- Treat all proc text as untrusted: bound reads, normalize newlines, and render as text.
- Do not allow client-provided proc roots, PIDs, or paths through the public API.
- The collector reads only the daemon's procfs view and only reports processes matching daemon-owned workspace namespace identities.

## Product repository tests

### Collector tests

Use a fixture proc tree and deterministic namespace-stat abstraction to cover:

- zero workspaces;
- idle workspace with namespace init;
- two workspaces with distinct namespace pairs;
- same PID namespace but different mount namespace is not assigned;
- forked descendants remain assigned;
- process exit races are skipped;
- missing cgroup membership is non-fatal;
- cgroup v1 and v2 membership lines are retained;
- holder disappearance yields a partial workspace;
- deterministic ordering and 512-row truncation.

### Runtime and query tests

- runtime snapshots associate the correct holder PID with each workspace ID;
- destroyed workspaces are absent;
- query emits schema version 2 and preserves the response envelope;
- unavailable is limited to topology-wide failures.

### Manager tests

- explicit cgroup requests merge manager-owned series with daemon-owned topology;
- manager does not replace a valid daemon topology with a synthetic unavailable value;
- daemon transport failure is reported distinctly;
- normal resource polling does not acquire a new daemon dependency.

### Console tests

Update browser fixtures and cover:

- active, idle, partial, empty, and unavailable states;
- cgroup membership with v1 multiple-line and v2 root values;
- backend-started commands appearing on refresh;
- responsive rendering and stable tab routing;
- no legacy delegated-cgroup language.

## Rollout

1. Add the proc collector and fixture tests without changing the public response.
2. Thread holder identity from runtime to daemon observability.
3. Switch query and manager merge behavior to schema version 2 in one backend change;
4. migrate console types, fixtures, and UI in the same integration branch;
5. add and run the linked live E2E family;
6. remove obsolete delegated-child-cgroup topology code after all consumers use version 2.

No migration of stored sandbox state is required. Topology is generated from the live runtime snapshot on demand.

## Acceptance criteria

- A newly opened workspace appears by its exact workspace ID before a workload command runs.
- A process started through any backend client appears under the correct workspace on the next successful refresh.
- Processes from two workspaces are never merged when their namespace pairs differ.
- A completed process disappears without leaving a stale row.
- A destroyed workspace disappears from topology.
- Read-only `/sys/fs/cgroup` and `0::/` membership still produce `available: true`.
- No topology code requires writable cgroups or `CAP_SYS_ADMIN` beyond capabilities already required for workspace creation.
- The manager returns daemon topology instead of a hardcoded unavailable value.
- The console no longer presents missing delegated cgroups as an availability failure.
- Product tests and the linked live E2E suite pass.
