# Test Plan — `isolated_workspace` mock-sandbox tier

**Date:** 2026-05-23
**Target dir:** `backend/src/task_center_runner/tests/mock/sandbox/isolated_workspace/`
**Tier:** Mock sandbox (real Docker Linux daemon via `sweevo_image_sandbox`)
**Source plan:** `.omc/plans/enter-workspace-with-isolated-network-20260522.md`
**Implementation report:** `.omc/plans/enter-isolated-workspace-impl-report-20260523.md`

---

## 0. Why this tier

Every load-bearing property of `enter_isolated_workspace` lives in the Linux
kernel: `unshare`, `setns`, `fsmount`, cgroup v2 quotas, bridge port
isolation, nftables MASQUERADE, IPv6 default-route purge. A unit-test tier
with a `FakeRuntime` can only catch lifecycle-order bugs; it cannot catch the
bugs the design is actually trying to prevent. Therefore the entire test
investment lands here — `sweevo_image_sandbox` boots a real Docker container,
the daemon is the real daemon, and `call_daemon_api(sandbox_id,
"api.isolated_workspace.enter", ...)` exercises the actual kernel path.

The three earlier macOS unit tests (manager lifecycle, network helpers,
import-fence) were deleted as part of this re-plan. The structural fences
(R3 import-graph, N2 dynamic-import, R10 setns_exec discipline, C1/C2
handle-shape) are reborn here as **Tier 0: pre-flight** — they're
platform-agnostic AST/import walks but live alongside the live tests so the
test directory tells the whole story in one place.

---

## 1. Directory layout

```
isolated_workspace/
├── PLAN.md                              # this file
├── __init__.py
├── conftest.py                          # shared fixtures (see §2)
├── _iws_invariants.py                   # audit-event + reachability helpers
├── _iws_rpc.py                          # thin client over call_daemon_api
├── _iws_fixtures.py                     # peer-publish, sentinel-layer, http_fixture_on_host
│
├── pre_flight/                          # Tier 0: structural fences
│   ├── test_import_graph_fence.py       # R3 + N2
│   ├── test_setns_exec_discipline.py    # R10
│   ├── test_handle_shape_no_publish.py  # C1
│   └── test_exit_path_no_occ.py         # C2 (source scan)
│
├── happy_path/                          # Tier 1: golden scenarios
│   ├── test_enter_then_shell_then_exit.py
│   ├── test_server_survives_tool_call_boundary.py
│   ├── test_status_reports_open_handle.py
│   └── test_lowerdir_visible_inside_mntns.py
│
├── isolation/                           # Tier 2: structural separation
│   ├── test_full_cycle_never_calls_occ.py     # R1 behavioral
│   ├── test_upperdir_discarded_on_exit.py
│   ├── test_lowerdir_pinned_against_peer_publish.py
│   ├── test_default_mode_unaffected_during_pinned.py
│   └── test_cross_agent_unreachable.py        # bridge port isolation
│
├── network/                             # Tier 3: networking
│   ├── test_arbitrary_egress_via_masquerade.py
│   ├── test_imds_dropped.py
│   ├── test_imds_rule_reinstalled_on_boot.py
│   ├── test_masquerade_rule_reinstalled_on_boot.py
│   ├── test_dns_routable_resolver.py
│   ├── test_dns_systemd_resolved_fallback.py
│   ├── test_dns_fallback_survives_tool_call_boundary.py
│   ├── test_dns_symlinked_resolv_conf.py
│   ├── test_no_ipv6_default_route.py
│   ├── test_port_isolation_flag_present.py
│   └── test_rfc1918_egress_drop_opt_in.py
│
├── failure_modes/                       # Tier 4: adversarial / partial-rollback
│   ├── test_setup_timeout_wedge.py            # N1
│   ├── test_ns_holder_dies_before_ready.py
│   ├── test_overlay_mount_fails.py
│   ├── test_veth_install_fails_releases_lease.py
│   ├── test_dns_helper_fails_does_not_strand_handle.py
│   ├── test_holder_refuses_sigterm_sigkill_fallback.py
│   └── test_write_file_streams_large_body_without_argv_e2big.py
│
├── resource_controls/                   # Tier 5: quota / TTL / RAM
│   ├── test_quota_one_per_agent.py
│   ├── test_total_cap_blocks_new_agent.py
│   ├── test_host_ram_gate_refuses_over_budget.py
│   ├── test_ttl_evict_and_audit.py
│   ├── test_ttl_does_not_evict_active.py
│   └── test_upperdir_tmpfs_enospc_natural_backpressure.py
│
├── concurrency/                         # Tier 6: races
│   ├── test_two_agents_same_port.py
│   ├── test_concurrent_enter_no_ip_double_allocation.py
│   ├── test_concurrent_default_and_isolated_in_same_agent.py
│   ├── test_same_agent_tool_calls_can_overlap.py
│   ├── test_map_lock_serializes_enter_exit_only.py
│   ├── test_init_complete_blocks_enter_during_startup_gc.py
│   └── test_re_enter_after_exit_gets_fresh_handle.py
│
├── gc_and_persistence/                  # Tier 7: durability
│   ├── test_manager_json_roundtrip.py
│   ├── test_manager_json_schema_mismatch_treated_as_empty.py
│   ├── test_daemon_restart_reaps_orphan_veth.py
│   ├── test_daemon_restart_reaps_orphan_cgroup.py
│   ├── test_daemon_restart_reaps_orphan_scratch.py
│   ├── test_daemon_restart_reaps_orphan_netns.py
│   ├── test_daemon_restart_releases_orphan_lease.py
│   ├── test_daemon_restart_reconciles_ip_pool.py
│   ├── test_iws_daemon_restart_mid_parallel_calls.py
│   ├── test_lowerdir_disk_usage_is_o1.py
│   ├── test_lowerdir_layer_paths_shared_across_concurrent_handles.py
│   ├── test_upperdir_discarded_on_abnormal_exit_daemon_kill.py
│   └── test_upperdir_fully_discarded_on_normal_exit.py
│
└── stress/                              # Tier 8: scale / soak
    ├── test_5_concurrent_isolated_workspaces.py   # TOTAL_CAP probe (cap=5)
    ├── test_rapid_create_destroy_cycle.py         # daemon crash-loop GC
    ├── test_disk_at_rest_bounded.py
    └── test_pip_install_then_run_e2e.py           # full network stack
```

**Total: 83 test files, organized into 12 tier directories.** The directory structure is
the documentation: each tier-named folder answers one design question.

---

## 2. Shared fixtures (`conftest.py`)

### `iws_sandbox` (session-scoped, depends on `sweevo_image_sandbox`)

Boots the sandbox via the configured provider (Docker by default per
`EOS_SANDBOX_PROVIDER=docker`; Daytona remains an opt-in fallback),
ensures the daemon is running, sets
`EOS_ISOLATED_WORKSPACE_ENABLED=true` for that container's environment
(write to `/etc/environment` + daemon restart), waits for capability ready.
Yields `(sandbox_id, daemon_endpoint)`. Session-scoped because daemon boot is
~10 s and most tests share state by design (multi-agent tests, peer-publish
tests).

### `iws_clean_sandbox` (function-scoped, depends on `iws_sandbox`)

Before each test:
1. Drives `api.isolated_workspace.exit` for every known test-agent (`agent-A`,
   `agent-B`, `agent-C`) — idempotent no-op if already closed.
2. Clears `manager.json` via daemon RPC `api.test_only.iws_reset` (added in
   the implementation; gated by env flag so prod can't accidentally hit it).
3. Asserts `active_count() == 0` before yielding.

Skipped in tests that explicitly want post-test state (e.g.,
`test_daemon_restart_*`).

### `iws_audit_tail` (function-scoped)

Returns a callable `wait_for(event_type, *, timeout_s=5.0, predicate=None)`
that tails `sandbox_events.jsonl` and yields the matching event. Used by every
test to assert audit-bus correctness without timing flake.

### `peer_publish` (function-scoped)

Helper to publish a layer through the existing default flow (`api.write_file`
via `call_daemon_api`). Returns the new manifest version. Used by
`test_lowerdir_pinned_against_peer_publish`.

### `sentinel_layer` (function-scoped, depends on `peer_publish`)

Publishes `/testbed/sentinel-{uuid}.txt` with body
`lowerdir-visible-{uuid}`. Yields the uuid. Exact-content asserts for
overlay-visibility tests.

### `http_fixture_on_host` (session-scoped)

Boots a tiny `aiohttp` server on the daemon-host's primary interface
(routable from the bridge's MASQUERADE'd egress), listening on a free port.
Yields `(ip, port)`. The fixture-side `aiohttp` access log captures the
incoming source IP so tests can prove MASQUERADE source-NAT works
(`test_v2_driver_4_acceptance`).

### `dns_resolver_on_host` (session-scoped)

Boots a tiny UDP DNS responder (`dnslib`) on the daemon host answering
`fixture.test → <host primary IP>`. Yields `(ip, port)`. Used by
`test_dns_routable_resolver` and the fallback tests.

### `synthetic_imds_target` (session-scoped)

Adds a host-side iptables PREROUTING rule mapping `169.254.169.254:80` to
the `http_fixture_on_host` port — lets tests prove the IMDS DROP rule is
active by attempting a connection that WOULD succeed if the rule were
missing. Removed in teardown.

## 3. RPC helper (`_iws_rpc.py`)

Thin client wrapping `call_daemon_api`. Exposes:

```python
async def enter(sandbox_id, agent_id, *, layer_stack_root=DEFAULT) -> dict
async def exit(sandbox_id, agent_id) -> dict
async def status(sandbox_id, agent_id) -> dict
async def shell(sandbox_id, agent_id, command, *, timeout_s=30) -> dict
async def read_file(sandbox_id, agent_id, path) -> dict
async def write_file(sandbox_id, agent_id, path, content: bytes | str) -> dict
async def edit_file(sandbox_id, agent_id, path, content) -> dict
async def grep(sandbox_id, agent_id, pattern, *, path="/testbed") -> dict
```

Each function returns the raw JSON response and raises on transport-level
errors only — caller asserts on the `success` / `error.kind` envelope so test
intent stays explicit ("I expected `isolated_workspace_already_open` here").

A small extension: `iws_rpc.daemon_exec(sandbox_id, argv)` shells into the
sandbox container via `adapter.exec` for tests that need to observe host-side
state (`ip link show`, `nft list ruleset`, `ls /sys/fs/cgroup/eos-iws-*`).

---

## 4. Invariants helper (`_iws_invariants.py`)

Mirroring `_background_shell_invariants.py`, this module owns the reusable
assertion logic so every test reads as intent, not boilerplate.

```python
def assert_audit_sequence(jsonl_path, expected: list[EventType])
def assert_handle_id_unique_per_enter(jsonl_path)
def assert_no_orphan_resources(sandbox_id, *, name_prefix="eos-iws-")
def assert_lease_acquired_released_balanced(jsonl_path)
def assert_reachability(sandbox_id, agent_id, dst_ip, port, *, expect: bool)
def assert_no_ipv6_default_route(sandbox_id, agent_id)
def assert_event_payload(jsonl_path, event_type, key, value)
def assert_no_event(jsonl_path, event_type)  # negative assertion
```

The negative-event helper is load-bearing: many isolation tests prove
absence (no `sandbox_occ_changeset_received` during an isolated cycle).

---

## 5. Test-by-test catalogue

Below: every test, what it catches, what an "innocent" bug fix would break.
This is the load-bearing column — if a test couldn't articulate a specific
failure mode it would prevent, it doesn't belong here.

### Tier 0: Pre-flight (4 tests)

| Test | Catches | What an innocent refactor would do to fail it |
|---|---|---|
| `test_import_graph_fence` | OCC reachable from `isolated_workspace_ops` transitive imports | Add `from sandbox.occ.changeset import …` to share a "convenient" change-set type |
| `test_setns_exec_discipline` | `setns_exec.py` imports `logging` / `asyncio` / `subprocess` / `threading` | Add `import logging` for debug output — breaks `setns(CLONE_NEWUSER)` with EINVAL |
| `test_handle_shape_no_publish` | `IsolatedWorkspaceHandle` gains a `publish_*` attr or subclasses `OperationOverlayHandle` | "Let's reuse `OperationOverlayHandle` so we don't duplicate fields" |
| `test_exit_path_no_occ` | `exit()` / `_teardown()` source mentions `apply_changeset` / `commit_prepared` | Cleanup PR shares a "common cleanup helper" that flushes upperdir to OCC |

These run in <100 ms on any platform; they're the canaries before paying for
real Docker boot.

### Tier 1: Happy path (4 tests)

| Test | Asserts |
|---|---|
| `test_enter_then_shell_then_exit` | Enter → `shell("echo hi")` → exit. Audit: `enter, tool_call, exit`. `status` returns `open=false` after exit. |
| `test_server_survives_tool_call_boundary` | tool_call A starts `python -m http.server 8080 &`. tool_call B `curl localhost:8080` succeeds. `pgrep -f http.server` returns the same PID across both calls. **Driver #1** of the source plan. |
| `test_status_reports_open_handle` | After enter, `status(agent_id)` returns `open=true, manifest_version, created_at, last_activity` — and `last_activity` advances after each tool call. |
| `test_lowerdir_visible_inside_mntns` | Uses `sentinel_layer`. `cat /testbed/sentinel-{uuid}.txt` returns `lowerdir-visible-{uuid}` exactly. Regression guard for `setns(CLONE_NEWNS)` + `fsmount` propagation. |

### Tier 2: Structural separation (5 tests)

| Test | Asserts |
|---|---|
| `test_full_cycle_never_calls_occ` | **R1 behavioral** — patches `CommitQueue.apply` + `apply_sync` (via debugger hook injected through a test-only env flag), drives full cycle, asserts `call_count == 0`. Re-claims the discriminatory power that the macOS unit test couldn't have. |
| `test_upperdir_discarded_on_exit` | Enter; write `/testbed/scratch.txt`; exit. Re-enter. `cat /testbed/scratch.txt` returns no-such-file. Critically: ALSO assert via host-side daemon `find {scratch_root}/runtime/isolated-workspace` that no leftover upper/ exists. |
| `test_lowerdir_pinned_against_peer_publish` | Enter agent-A. Peer agent-B publishes a new layer via default flow that deletes `/testbed/important.txt`. Inside ws-A, `cat /testbed/important.txt` still succeeds. A1 (snapshot-at-enter) is the design property. |
| `test_default_mode_unaffected_during_pinned` | Agent has open isolated ws. Same agent issues default `api.write_file("/testbed/x.txt", ...)` — succeeds; layerstack tip advances; isolated ws's view is unchanged. Tests that pinned ws is a side-channel. |
| `test_cross_agent_unreachable` | ws-A `10.244.0.X`, ws-B `10.244.0.Y`. From ws-A: `ping -c 1 -W 2 10.244.0.Y` fails; `curl --max-time 2 http://10.244.0.Y` fails. Mechanism: kernel-level bridge port isolation. **No nft rule** — so dropping `bridge-nf-call-iptables` from the host can't accidentally re-enable peer reach. |

### Tier 3: Network (11 tests)

| Test | Asserts |
|---|---|
| `test_arbitrary_egress_via_masquerade` | Uses `http_fixture_on_host`. From ws: `curl -s --max-time 5 http://{host_ip}:{port}/probe` → HTTP 200. Fixture-side log shows source IP == daemon-host's external IP. **Driver #4** measurable criterion. |
| `test_imds_dropped` | Uses `synthetic_imds_target`. From ws: `curl --max-time 2 http://169.254.169.254/` — connection drops. From daemon's own netns (outside ws): same curl reaches the synthetic target. Proves drop is scoped to forward chain. |
| `test_imds_rule_reinstalled_on_boot` | Stop daemon. From host: `nft delete table inet eos_iws_filter`. Restart daemon. Assert `capabilities.isolated_workspace=true` AND `test_imds_dropped` repro passes. R13 idempotent reinstall. |
| `test_masquerade_rule_reinstalled_on_boot` | Same but delete the NAT table. MASQUERADE re-installed at boot. |
| `test_dns_routable_resolver` | Lowerdir's `/etc/resolv.conf` = `nameserver {dns_resolver_on_host.ip}`. Enter. `getent hosts fixture.test` returns the host's IP. |
| `test_dns_systemd_resolved_fallback` | Lowerdir's `/etc/resolv.conf` = `nameserver 127.0.0.53`. Enter. Daemon detects 127.0.0.0/8 inside new mntns, bind-mounts fallback. `cat /etc/resolv.conf` inside ws shows `1.1.1.1`. `getent hosts fixture.test` resolves via fallback. |
| `test_dns_fallback_survives_tool_call_boundary` | After fallback applied, second tool call still shows fallback `/etc/resolv.conf`. Validates bind-mount lifetime == mntns lifetime. |
| `test_dns_symlinked_resolv_conf` | Lowerdir ships `/etc/resolv.conf` as symlink to `/run/systemd/resolve/stub-resolv.conf`. Daemon's own `/run` has different content. Detection MUST follow the symlink inside the new mntns (architect soundness). Fallback decision is based on in-mntns resolution. |
| `test_no_ipv6_default_route` | Enter. `ip -6 route show default` returns empty. `sysctl net.ipv6.conf.eth0.accept_ra` == `0`. Mitigates IPv6 bypass of IPv4 MASQUERADE. |
| `test_port_isolation_flag_present` | After enter, host-side `bridge -j -d link show dev {veth_host}` shows `isolated: true, mcast_flood: false`. Detects accidental flag drop. |
| `test_rfc1918_egress_drop_opt_in` | Boot daemon with `EOS_ISOLATED_WORKSPACE_RFC1918_EGRESS=deny`. Enter. `curl --max-time 2 http://10.99.99.99` drops; `curl --max-time 5 http://{public_fixture}` succeeds. Validates Scenario 5 opt-in. |

### Tier 4: Failure modes & partial rollback (8 tests)

| Test | Asserts |
|---|---|
| `test_setup_timeout_wedge` | Inject `EOS_ISOLATED_WORKSPACE_TEST_HANG_AT=mount_overlay` (test-only env knob). Daemon hangs in `mount(2)`; setup-timeout fires at 5 s (`EOS_ISOLATED_WORKSPACE_SETUP_TIMEOUT_S=5`); error has `kind: setup_timeout, failed_step: overlay_mount`. **N1.** Critically asserts subsequent enter() succeeds — the wedge didn't strand state. |
| `test_ns_holder_dies_before_ready` | Test-only knob `EOS_ISOLATED_WORKSPACE_TEST_HOLDER_CRASH=true`. Holder exits before writing `ready`. enter() returns `setup_failed`. No orphan veth / cgroup / scratch left behind. |
| `test_overlay_mount_fails` | Test knob makes mount return EBUSY. enter() releases lease. Manager `_by_agent` map empty. Audit event includes `failed_step: overlay_mount`. |
| `test_veth_install_fails_releases_lease` | Test knob makes `ip link add` return EEXIST conflict. Lease released; IP pool's `_allocated_ips` cardinality decreases by 1 (no IP leak). |
| `test_dns_helper_fails_does_not_strand_handle` | DNS detection raises. Manager rolls back: kills holder, deletes veth, releases lease. State machine ends at `stopped`, not stuck in `exiting`. |
| `test_holder_refuses_sigterm_sigkill_fallback` | After enter, host-side `kill -STOP {holder_pid}` to make it ignore SIGTERM. exit() takes ~5 s (grace), then SIGKILL fires; netns/mntns/pidns reaped. |
| `test_write_file_streams_large_body_without_argv_e2big` | write_file with a 5 MB body. The namespace-runner stdin payload path must not trigger argv-E2BIG. |

### Tier 5: Resource controls (6 tests)

| Test | Asserts |
|---|---|
| `test_quota_one_per_agent` | Second enter on same agent → `isolated_workspace_already_open` with `created_at` / `last_activity` in details (S1 diagnostic surface). |
| `test_total_cap_blocks_new_agent` | Default cap is **5** (`EOS_ISOLATED_WORKSPACE_TOTAL_CAP=5`). For this test override to `total_cap=2` to keep wall-clock low: three different agents enter; third returns `quota_exceeded`. Validates the env-var override path AND the default ceiling boundary. |
| `test_host_ram_gate_refuses_over_budget` | Set `EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES` ≥ host's free RAM. enter() returns `host_capacity_exceeded` with `required_bytes` / `budget_bytes` in details. R6. |
| `test_ttl_evict_and_audit` | `EOS_ISOLATED_WORKSPACE_TTL_S=1`. Enter; wait 3 s; TTL sweep evicts; audit shows `evicted, reason=ttl`. Idempotent re-enter succeeds. |
| `test_ttl_does_not_evict_active` | Tool calls keep `last_activity` fresh; TTL sweep does not evict. |
| `test_upperdir_tmpfs_enospc_natural_backpressure` | `EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES=10MB`. Write a 12 MB file → tool call returns `exit_code != 0` with `No space left on device`. Subsequent reads inside the same ws still work. ENOSPC is natural backpressure, not a daemon crash. |

### Tier 6: Concurrency (7 tests)

| Test | Asserts |
|---|---|
| `test_two_agents_same_port` | agent-A and agent-B each start `python -m http.server 8080` inside their own ws. Neither sees EADDRINUSE. From agent-A's tool calls, curl localhost:8080 succeeds; reaching agent-B's IP:8080 fails (cross-agent isolation). |
| `test_concurrent_enter_no_ip_double_allocation` | Trio of `enter` calls fired concurrently from 3 different agents. After all settle, 3 distinct IPs allocated. Audit log: 3 `enter` events; 3 unique `ns_ip` values. |
| `test_concurrent_default_and_isolated_in_same_agent` | Historical Phase 1 case. Phase 2 routes foreground `api.v1.<verb>` calls through the active isolated workspace when a handle is open; default-mode coexistence for the same agent is no longer a separate tool-op RPC path. |
| `test_same_agent_tool_calls_can_overlap` | Two `shell` calls for the same agent fired concurrently. The manager does not impose a per-handle execution lock, so both calls overlap in wall time. |
| `test_map_lock_serializes_enter_exit_only` | Two different agents' tool calls fired concurrently. The map lock protects enter/exit map mutations only; tool calls overlap in wall time. |
| `test_init_complete_blocks_enter_during_startup_gc` | Restart daemon. From the moment the daemon process is up, fire `enter()`. Concurrently `manager_json` reconciliation is running. Assert enter does NOT return until GC settled (audit shows the gc_orphan events arrive before the enter event). |
| `test_re_enter_after_exit_gets_fresh_handle` | enter; write_file `/testbed/scratch.txt`; exit; enter again. New `handle_id`; `read_file /testbed/scratch.txt` returns no-such-file. Fresh ephemeral upperdir guaranteed. |

### Tier 7: GC and persistence (13 tests)

| Test | Asserts |
|---|---|
| `test_manager_json_roundtrip` | Enter agent-A. Read host-side `{scratch_root}/runtime/isolated-workspace/manager.json`. Expected schema_version=1, exactly one handle row with non-empty lease_id, ns_ip, cgroup_path. NO raw FDs (`ns_fds` key absent). |
| `test_manager_json_schema_mismatch_treated_as_empty` | Write `schema_version=999` to manager.json. Restart daemon. Daemon logs WARN; reconciliation falls back to naming-convention reap; manager comes up empty. |
| `test_daemon_restart_reaps_orphan_veth` | Enter; SIGKILL daemon while ws open; restart daemon. Host-side `ip link show` shows no `eos-iws-*` veth. Audit: `gc_orphan, kind=veth`. |
| `test_daemon_restart_reaps_orphan_cgroup` | Same setup. `ls /sys/fs/cgroup/eos-iws-*` is empty after restart. |
| `test_daemon_restart_reaps_orphan_scratch` | Scratch dirs in `{scratch_root}/runtime/isolated-workspace/*` removed. |
| `test_daemon_restart_reaps_orphan_netns` | `ip netns list \| grep eos-iws-` returns nothing (whether or not we adopt named netns; this test gates on the naming convention). |
| `test_daemon_restart_releases_orphan_lease` | Before kill: `LeaseRegistry.active_count() == 1`. After restart + GC: 0. New `enter()` succeeds (lease wouldn't get an empty layer path if the old one were still pinned). |
| `test_daemon_restart_reconciles_ip_pool` | Enter agent-A (gets .2). SIGKILL daemon; modify `manager.json` to say `.2` is allocated. Restart. Concurrent fresh enter: gets .3 (NOT .2). |
| `test_iws_daemon_restart_mid_parallel_calls` | SIGKILL daemon during concurrent isolated tool calls. Restart must reap orphan handles and allow fresh enters without leaked resources. |

### Tier 8: Stress / scale / soak (4 tests, marked `slow`)

| Test | Asserts |
|---|---|
| `test_5_concurrent_isolated_workspaces` | Create 5 different agent IDs. Concurrent enter() for all (asyncio.gather). All 5 succeed. 6th agent's enter() returns `quota_exceeded`. All 5 teardown cleanly in parallel. Maximum-load proof of the v2 `TOTAL_CAP=5` default. Also asserts: 5 distinct `ns_ip` allocations from `10.244.0.2 – 10.244.0.6`; 5 distinct cgroup dirs; daemon CPU stays bounded during concurrent spin-up. |
| `test_rapid_create_destroy_cycle` | 100 enter/exit cycles for the same agent in a tight loop. No FD leak (host-side `lsof -p {daemon_pid} \| wc -l` stays bounded). No veth leak. No IP-pool drift. |
| `test_disk_at_rest_bounded` | Enter; wait 60 s; check the open workspace's writable dirs stay bounded at rest. |
| `test_pip_install_then_run_e2e` | Marked `@pytest.mark.requires_internet`. Enter ws. `pip install --target /tmp/pkg httpx`. `PYTHONPATH=/tmp/pkg python -c "import httpx; print(httpx.get('https://httpbin.org/get').status_code)"` → 200. Whole stack: DNS + MASQUERADE + bridge + outbound HTTPS + cross-tool-call package availability. |

---

## 6. What's deliberately NOT in this plan

- **Macro-scenario tests via `SCENARIO_REGISTRY`.** The agent-driven scenario
  harness is wrong for the isolated_workspace tier — it adds a mock-agent
  layer between the test intent and the daemon RPC. We drive
  `call_daemon_api` directly. (One exception: the `pip_install_then_run`
  e2e test in stress/ could become an agent-driven scenario later.)
- **macOS-runnable mocks.** Removed in this re-plan. The structural fences
  (Tier 0) are platform-agnostic AST walks; they happen to run on macOS
  but aren't macOS-specific.
- **Tests for not-yet-implemented features:** `api.runtime.ready` capability
  advertisement, `EOS_ISOLATED_WORKSPACE_AUDIT_EGRESS` flow logging, the
  daemon-host RFC1918 reachability boot warning. These will get tests when
  the features land, not before.

---

## 7. Implementation order (recommended sprint plan)

Do not write all 50 at once. Recommended order maps to "what breaks if you
land the corresponding production code without it":

1. **Tier 0 (pre-flight, 4 tests).** Catches the most common refactor
   mistakes; runs in <100 ms; no Docker needed. **Land before any other
   production change to `isolated_workspace_ops`.**
2. **Tier 1 happy path (4 tests).** Smoke; if these don't pass nothing
   else can. Needed before claiming the feature works end-to-end.
3. **Tier 2 isolation (5 tests).** The security argument. Land before
   production rollout.
4. **Tier 7 GC and persistence (10 tests).** Largest design surface that
   can silently break. Land before the second daemon restart.
5. **Tier 3 network (11 tests).** Per-rule coverage; can land
   incrementally per nft rule.
6. **Tier 4 failure modes (8 tests).** Adversarial; lands as the production
   error paths are filled in.
7. **Tier 5 + 6 (14 tests).** Resource controls + concurrency. Lands once
   the manager is stable enough to subject to real load.
8. **Tier 8 stress (4 tests).** Last; needed before production rollout for
   the "soak" gate.

---

## 8. Cost model

| Tier | Avg test wall-time | Daemon restart cost | Notes |
|---|---|---|---|
| 0 | <100 ms each | none | Static analysis only |
| 1 | ~3 s each | shared `iws_sandbox` | Function-scoped cleanup |
| 2 | ~5 s each | shared | Peer-publish adds ~1 s |
| 3 | ~4 s each | per `_reinstalled_on_boot` test | Daemon-restart tests pay ~10 s |
| 4 | ~8 s each | per test | Failure-injection requires fresh state |
| 5 | ~5 s each | shared | TTL tests use tiny TTL values |
| 6 | ~6 s each | shared | Concurrent enters tracked via asyncio.gather |
| 7 | ~15 s each | per test | Daemon SIGKILL + reboot dominates |
| 8 | 30 s – 5 min | per test | Marked `@pytest.mark.slow` |

Total budget: ~12 minutes for the full mock-sandbox suite, plus stress tier
gated behind `--run-slow`.

---

## 9. Open questions to resolve before writing code

1. **`api.test_only.iws_reset` RPC**: do we add a test-only daemon op for
   forced-reset, or do we accept slower per-test reset via daemon restart?
   (Recommendation: add it, gated by `EOS_ENABLE_TEST_RPCS=true`.)
2. **Synthetic IMDS target**: iptables PREROUTING or an IP alias on the host?
   The alias is cleaner but needs CAP_NET_ADMIN at fixture setup.
3. **Test-only failure-injection knobs**: env-var pattern
   (`EOS_ISOLATED_WORKSPACE_TEST_HANG_AT=…`) is grep-friendly but pollutes
   the production binary. Alternative: a `failure_injector` callable hooked
   into `_Runtime` via a setter that exists only when test mode is enabled.
   (Recommendation: env knob — easier to debug from logs; minimal blast
   radius if a production deploy accidentally sets one.)
4. **Daemon-restart preservation of `iws_audit_tail`**: the audit log file is
   inside the daemon's bundle; do we tail by path (survives restart) or by
   stream subscription (drops at restart)? Path tail wins for daemon-restart
   tests.
5. **CI gating**: the full suite needs the sweevo Docker image which not
   every CI runner has. Recommendation: gate Tier 1–7 on
   `database_configured() and live_e2e_heavy_enabled()` (matching existing
   `_background_shell` convention); gate Tier 8 on `--run-slow` additionally.

---

## 10. Done definition

For each tier, "done" means:

- All listed tests pass against the sweevo Docker image.
- Each test has a one-line docstring stating the property it preserves
  (load-bearing column above).
- Each test that requires daemon restart asserts not just the post-restart
  state, but also reads the daemon's INFO/WARNING log lines to verify the
  R5 GC step ordering. (Cheating on order — e.g., killing before
  unfreezing — would silently strand zombies; the log assertion is the
  canary.)
- `_iws_invariants.py` has zero ad-hoc test-specific assertions inlined in
  individual test files. If a check is reused twice, it moves to the
  helper.
- The directory README (a TODO follow-up after Tier 1 lands) explains the
  9-tier layout for future maintainers in <30 lines.

---

# v2 Extension (2026-05-23)

**Status:** Consensus-approved through ralplan iteration 2 (Architect: PLAN
SOUND WITH FLAGS; Critic: APPROVE with 6 inline follow-ups). Extends §§1-10
above; does NOT replace them. Adds 22 tests across existing Tiers 1, 2, 3,
6, 7, 8 plus one new tier (Tier 9: performance). Enriches the 5
`sandbox_isolated_workspace_*` audit-event payloads with `phases_ms` + `total_ms`.

## 11. Scope of the v2 extension

Six user requests drive this extension:

1. **More FS / overlay coverage** — symlinks, whiteouts, userns chmod escape.
2. **Network inbound rejection** — external→ws impossible; daemon-host→ws
   allowed (intentional); conntrack RELATED/ESTABLISHED for return traffic.
3. **Parallel non-interference at N=5** — fs / network / cgroup memory /
   audit-bus interleaving under TOTAL_CAP=5.
4. **Lowerdir O(1) disk + graceful GC** — N concurrent ws share one
   snapshot's `layer_paths`; upperdir fully discarded on normal AND
   abnormal exit.
5. **Per-operation latency audit** — per-phase ms breakdown emitted in
   every lifecycle event; regression-detection via hybrid baseline.
6. **Audit payload enrichment** — `phases_ms` + `total_ms` additive on the
   5 existing events; **no new `EventType` enum values**.

### Global constant (clarification)

**TOTAL_CAP=5 is the WORKSPACE CONCURRENCY cap** (env var
`EOS_ISOLATED_WORKSPACE_TOTAL_CAP=5`), not a test count cap. All parallel
tests in this extension target N=5 concurrent live workspaces. Tests that
prove cap enforcement may sequentially attempt a 6th and assert
`quota_exceeded`, but live-concurrent fanout stays at 5.

## 12. Principles (v2)

**P1. Discriminator-first.** Every new test must articulate the specific
bug it catches (continues §5 convention).

**P2. Tests own the surface they assert, not the surface they wish
existed.** A test asserting a phase timing MUST exercise a codepath that
emits it. Stubbed codepaths (e.g., `_LinuxRuntime.mount_overlay` raising
`NotImplementedError` at `service/isolated_workspace.py:642`) are gated
behind an empirical capability probe at fixture setup.

**P3. FakeRuntime mirrors `_Runtime` Protocol exactly.** Any protocol
widening (e.g., future 4-phase `tool_call`) is paired with a FakeRuntime
update in the same PR.

**P4. HYBRID latency baseline.** Per-op stability uses session-collected
medians (catches in-PR variance, hardware-portable); absolute regression
uses checked-in `latency_budget.json` (catches multi-PR drift + hardware
regression). Refreshed once per milestone PR (~10 feature PRs). Pure
session-only fails hard requirement #5 (same-PR-baked regression invisible);
pure budget-only flakes on CI host churn.

**P5. Conditional-key emission + enumerated back-compat.** Phase keys
appear in `phases_ms` only if the codepath ran. Emitting a key with value
`0.0` for an unrun branch is **FORBIDDEN**: absence and zero have distinct
semantics. Existing audit-payload consumers:

- `task_center_runner/audit/recorder.py:400-412` — passes payload opaquely
  (no schema validation).
- `task_center_runner/audit/performance_report.py:289-295` — reads
  `total_ms` as top-level numeric.

Future consumers must inherit: `total_ms` top-level guaranteed across all
emissions; `phases_ms` sub-dict keys are emitter-defined and may grow.

## 13. Decision Drivers (v2)

1. **Test surface must scale to both per-op stability AND
   absolute-hardware regression.** A single baseline source cannot serve
   both — hybrid is the only design that satisfies hard requirement #5.
2. **PR ordering must match dependency ordering.** Tier 9 cannot certify
   against a stubbed `mount_overlay`; a wiring PR must land first.
3. **Audit back-compat is a committed constraint, enumerated above (P5),
   not aspirational.**

## 14. Audit-Event Payload Enrichment Contract

The enrichment landing in PR 1 (§17 below) adds the following keys to each
workspace-lifecycle `AuditEvent` payload. **No new `EventType` enum values
are added** — enrichment is additive on the existing 5 sandbox events
(`sandbox_isolated_workspace_{enter, exit, tool_call, evicted, gc_orphan}`).

### Shape

```jsonc
{
  // existing top-level fields — UNCHANGED for back-compat (P5)
  "handle_id":          "<hex16>",
  "agent_id":           "<string>",       // enter only
  "manifest_version":   <int>,            // enter only
  "manifest_root_hash": "<sha256>",       // enter only
  "ns_ip":              "<dotted-quad>",  // enter only
  "rfc1918_egress_mode": "allow|deny",    // enter only
  "reason":             "explicit|ttl",   // exit / evicted
  "lifetime_s":         <float>,          // exit / evicted
  "upperdir_bytes_discarded": <int>,      // exit / evicted
  "argv0":              "<string>",       // tool_call
  "exit_code":          <int>,            // tool_call
  "duration_s":         <float>,          // tool_call (preserved)
  "kind":               "veth|cgroup|scratch|netns|lease", // gc_orphan
  "identifier":         "<string>",       // gc_orphan

  // v2 additive — top-level
  "total_ms":           <float>,          // wall-clock total for the operation
  "lowerdir_layer_count": <int>,          // enter only — len(snapshot.layer_paths)
  "tree-copy":        false,            // enter only — must always be false; tripwire

  // v2 additive — nested phases (keys conditional on emission, P5)
  "phases_ms": {
    // enter:
    "prepare_snapshot":  <float>,
    "spawn_ns_holder":   <float>,
    "open_ns_fds":       <float>,
    "install_veth":      <float>,
    "mount_overlay":     <float>,   // ABSENT while _LinuxRuntime stub raises NotImplementedError
    "configure_dns":     <float>,
    "create_cgroup":     <float>,

    // exit / evicted:
    "kill_holder":       <float>,
    "teardown_veth":     <float>,
    "release_snapshot":  <float>,
    "cgroup_rmdir":      <float>,
    "rmtree_scratch":    <float>,

    // tool_call:
    "exec":              <float>,   // coarse: setns + spawn + exec + wait

    // gc_orphan (per-orphan; see §15.3):
    "discover":          <float>,
    "reap":              <float>
  }
}
```

### SUBSET-COVER invariant

> **`sum(observed_phases_ms.values()) ≤ total_ms + ε`**
> where `ε = max(_PHASE_TIMER_OVERHEAD_BUDGET_MS, 0.05 × total_ms)`
> and `_PHASE_TIMER_OVERHEAD_BUDGET_MS = 2.0` (constant in
> `service/isolated_workspace.py`, exported for tests).

**Why subset-cover and not equality:** when a phase's codepath did not run
(stubbed `mount_overlay`, errored mid-enter, etc.), its key is absent from
`phases_ms` — the sum is strictly less than `total_ms`. The `+ ε` slack
covers `_PhaseTimer` bookkeeping plus the inter-phase gaps. The generalized
`max(2.0 ms, 5% × total_ms)` form (per Critic follow-up #5) keeps the
invariant non-degenerate for sub-5 ms operations: at
`total_ms = 3.0`, `ε = max(2.0, 0.15) = 2.0`; at
`total_ms = 200`, `ε = max(2.0, 10.0) = 10.0`.

**Forbidden:** emitting `"<phase>_ms": 0.0` for a branch that did not run.
Absence ≠ zero (P5).

### Source-code cross-references (per-phase boundary)

| Phase | File:line | Notes |
|---|---|---|
| `prepare_snapshot` | `service/isolated_workspace.py:348-352` | Calls `LayerStackPort.prepare_workspace_snapshot(...)`. Test 14.1 below asserts the kwarg via interception. |
| `spawn_ns_holder` | `service/isolated_workspace.py:401-403` | Covers `unshare` exec + `ns-up` handshake read. |
| `open_ns_fds` | `service/isolated_workspace.py:404` | 4 × `os.open` on `/proc/{pid}/ns/{user,mnt,pid,net}`. |
| `install_veth` | `service/isolated_workspace.py:405-407` | IP pool allocate + 5 × `ip link` calls. |
| `mount_overlay` | `service/isolated_workspace.py:408` | Stubbed via `NotImplementedError` until PR 0 wires the live path; phase ABSENT until then. |
| `configure_dns` | `service/isolated_workspace.py:409` | |
| `create_cgroup` | `service/isolated_workspace.py:410` | |
| `kill_holder` | `service/isolated_workspace.py:448-450` | SIGTERM + grace + SIGKILL. |
| `teardown_veth` | `service/isolated_workspace.py:451-453` | |
| `release_snapshot` | `service/isolated_workspace.py:458-461` | |
| `cgroup_rmdir` | `service/isolated_workspace.py:462-464` | Ordered AFTER `release_snapshot` in source. |
| `rmtree_scratch` | `service/isolated_workspace.py:465-466` | |
| `exec` | `isolated_workspace/pipeline.py:190-200` + `_LinuxRuntime.run_in_handle` | Coarse for v1 (see §15.2). |
| `discover` (gc_orphan) | `service/isolated_workspace.py:289-291` (veth); `:307-308` (scratch) | Per-resource share of the walk. |
| `reap` (gc_orphan) | `service/isolated_workspace.py:298-301` (veth); `:312` (scratch) | |

### Implementation helper

A small `_PhaseTimer` (~15 LoC) lives in `service/isolated_workspace.py`:

```python
class _PhaseTimer:
    def __init__(self, clock: Callable[[], float] = time.monotonic) -> None:
        self._clock = clock
        self._start = clock()
        self._phases: dict[str, float] = {}

    @contextlib.contextmanager
    def measure(self, name: str):
        t0 = self._clock()
        try:
            yield
        finally:
            self._phases[name] = (self._clock() - t0) * 1000.0

    def total_ms(self) -> float:
        return (self._clock() - self._start) * 1000.0

    @property
    def phases_ms(self) -> dict[str, float]:
        return dict(self._phases)
```

Used at each emit site:

```python
timer = _PhaseTimer()
with timer.measure("install_veth"):
    handle.veth = self._network.install_veth(...)
# … other phases …
self._emit("sandbox_isolated_workspace_enter", {
    ..., "total_ms": timer.total_ms(), "phases_ms": timer.phases_ms,
})
```

## 15. Three structural decisions resolved (no longer open questions)

### 15.1 Latency baseline = HYBRID (P4)

Three options considered:

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| Session-only | Hardware-portable; tight per-op variance bounds | Cannot detect same-PR-baked regression, cross-PR drift, absolute hardware regression. Fails hard requirement #5. | **Rejected.** |
| `latency_budget.json` only | Catches absolute drift; reviewable in PR | Brittle to CI host churn; flake-prone for per-op variance | **Rejected.** |
| **HYBRID** | Session catches in-PR variance; budget catches drift | Two assertion classes per test (~10 LoC `LatencyBudget` helper amortizes) | **Chosen.** |

`latency_budget.json` shape:

```json
{
  "_schema_version": 1,
  "_reference_host": "ci-linux-x86_64-2vcpu-4gb",
  "_refreshed_at": "2026-05-23",
  "_refreshed_by_pr": "<URL>",
  "workspace_create": {"total_ms_p95": 800, "total_ms_p99": 1200},
  "tool_call":        {"total_ms_p95": 250, "total_ms_p99": 400},
  "kill_holder":      {"total_ms_p95": 150, "total_ms_p99": 250},
  "gc_orphan":        {"total_ms_p95_per_orphan": 50}
}
```

### 15.2 `tool_call` phase count = 1

**Decision: ship one phase (`exec`) and do not reintroduce hidden per-call
pause/resume work.**

Rationale: `_Runtime.run_in_handle` Protocol at
`service/isolated_workspace.py:178-185` returns
`tuple[int, bytes, bytes]` — no sub-phase timing exposed. The runtime no
longer performs hidden work between calls, so the only honest timing
boundary is the actual setns/exec helper call.

**Deferral ticket** (file alongside PR 1):
`[isolated-workspace] Widen _Runtime.run_in_handle to expose
setns_spawn/exec per-phase timings`.

**Sunset trigger** (commits the protocol-widening to "next step", not
"diagnose later" — per Critic follow-up #4): when `tool_call.exec` P95
exceeds **500 ms** on the reference CI host over a rolling 7-day window of
`latency_budget.json` refresh data, the ticket auto-graduates to a v2
protocol-widening PR.

### 15.3 `gc_orphan` timer design = per-orphan, not per-pass

**Decision: each reaped orphan emits one event with its own `total_ms`
+ `phases_ms.{discover, reap}`.**

Rationale: per-orphan localizes regression to specific orphan kind (a
veth-reap and a scratch-rmtree have very different perf characteristics);
matches `performance_report.py` aggregation pattern; bounded by TOTAL_CAP=5
(at most a handful of orphans per GC pass in worst case).

`latency_budget.json` stores `gc_orphan.total_ms_p95_per_orphan`, not
per-pass.

## 16. PR Sequence (with dependency ordering)

| PR | Title | Files touched | Lands |
|----|-------|---------------|-------|
| **PR 0** | `_LinuxRuntime.mount_overlay` + `configure_dns` live wiring | `service/isolated_workspace.py:642`, new `scripts/setns_overlay_mount.py`, FakeRuntime | Replaces `NotImplementedError` with setns helper subproc that calls `mount_overlay`; `configure_dns` writes `/etc/resolv.conf` inside ws mntns; FakeRuntime updates. **Sub-decomposition required in the PR description** (Critic follow-up #1): three sub-surfaces — (a) `setns_overlay_mount.py` helper subprocess + signal handling, (b) `_LinuxRuntime.mount_overlay` wiring, (c) `_LinuxRuntime.configure_dns` + matching FakeRuntime update. If review feedback requests a split, accept it without re-ralplan. **PR 0 acceptance criterion** (Critic follow-up #6): a backstop test that calls `_LinuxRuntime.mount_overlay` directly (not through the manager) and asserts `/proc/<pid>/mountinfo` reflects the mount — guards against future regressions that re-stub the method. |
| **PR 1** | Audit-event enrichment (§14 contract) + `_PhaseTimer` | `service/isolated_workspace.py` (5 `_emit` call sites, `_PhaseTimer` class) | Conditional-key emission; SUBSET-COVER invariant test fixture; back-compat consumer tests against `performance_report.py:289-295`. Existing 49 macOS unit tests must still pass. |
| **PR 2** | Tier 1 + Tier 2 FS coverage (3 tests + enrichment) | `happy_path/`, `isolation/` | `test_lowerdir_symlink_traversal`, `test_whiteout_on_upperdir_delete`, `test_chmod_uid_in_userns_does_not_escape`; `test_workspace_create_passes_tree-copy_false` interception. |
| **PR 3** | Tier 3 inbound-rejection (4 tests via `unshare -n`) | `network/` | `test_external_inbound_impossible` etc. Uses `unshare -n` host-netns probe, NOT a second sandbox container (avoids sweevo session-lock collision; rationale is provider-agnostic — applies to Docker default and Daytona fallback alike). |
| **PR 4** | Tier 6 + Tier 8 noisy-neighbor at N=5 (4 tests + enrichment) | `concurrency/`, `stress/` | Cross-interference proofs; existing Tier 8 `test_5_concurrent_isolated_workspaces` enriched with `phases_ms.install_veth ≤ 5× in-test median` contention bound. |
| **PR 5** | Tier 7 + Tier 8 lowerdir O(1) + GC (5 tests + enrichment) | `gc_and_persistence/`, `stress/` | Layer-paths sharing structural check; `du --bytes` behavioral backstop; upperdir discard normal + abnormal. |
| **PR 6** | Tier 9 (performance) directory + fixtures | `performance/`, `_iws_invariants.py`, `conftest.py` | 7 tests; `latency_baseline` session fixture; `LatencyBudget` helper; `_capability_probe`. **All 7 tests are capability-gated; without PR 0 merged, they SKIP loudly** (§18). |
| **PR 7** | First `latency_budget.json` refresh | `_data/latency_budget.json` | 100-iteration distribution dump on the reference CI host; runbook for future refresh PRs. |

PRs 2-5 are mutually independent after PR 1 lands. PR 6 depends on PR 1 +
PR 0. PR 7 depends on PR 6.

## 17. `latency_budget.json` governance (Critic follow-up #2)

- **Owner:** workspace-platform on-call rotation (rotating weekly).
- **Refresh trigger:** EITHER file age > 90 days, OR > 10 merged feature PRs
  touching `service/isolated_workspace.py` since last refresh, whichever
  fires first. Tracked via a CI cron that opens an issue (not a blocking
  PR) when the trigger fires.
- **Stale-action policy:** when trigger fires, CI emits a **warning** on
  every PR until refreshed — not a failure. A scheduled refresh PR
  auto-opens via the on-call rotation; reviewer ack on the PR closes the
  warning.
- **Why warning, not failure:** budget staleness is governance debt, not
  correctness debt. Blocking feature work on it inverts the relationship.

The first `latency_budget.json` lands in PR 7, derived from PR 6's
empirical measurement on the reference host. Until PR 7 lands, Tier 9
absolute-ceiling checks `pytest.skip` with the explicit reason
`latency_budget.json not yet committed`.

## 18. Capability-probe failure policy (Critic follow-up #3)

Tier 9 tests are gated behind empirical probes (`_capability_probe`)
that run once at fixture setup and detect whether each `_LinuxRuntime`
surface is live or stubbed.

| Probe | What it checks | How |
|---|---|---|
| `has_mount_overlay()` | `_LinuxRuntime.mount_overlay` no longer raises `NotImplementedError` | Calls on a throwaway 1-layer handle; treats `NotImplementedError` as "unwired" and any other exception as "wired but failing" (loud skip). |
| `has_configure_dns()` | Returns `True` (not the `:646-648` stub `False`) | Direct call. |
| `has_run_in_handle()` | `_LinuxRuntime.run_in_handle` executes `["true"]` and returns exit code 0 | E2E call. |

**Failure-mode policy:**

- **Until PR 0 merges:** probe-False → `pytest.skip` with explicit reason
  `"PR 0 (mount_overlay/configure_dns wiring) not yet landed"`. Skip is
  acceptable in this window because the surface genuinely doesn't exist.
- **After PR 0 merges, on the reference CI host:** probe-False →
  `pytest.fail` with the same reason. Skip is no longer acceptable because
  the capability is now expected; silent skip would be false-coverage signal.
- **Local-dev or non-reference hosts:** probe-False continues to `skip`
  with loud reason `"capability not detected; this is a kernel-touching
  test"`. Local devs without kernel access are not blocked.

The reference CI host is identified by env var `EOS_CI_REFERENCE_HOST=true`.
Other hosts default to local-dev semantics.

## 19. New test inventory (per existing tier)

### 19.1 Tier 1 (happy path) — 2 new tests

| File | Asserts | Innocent-refactor failure mode |
|---|---|---|
| `happy_path/test_lowerdir_symlink_traversal.py` | Lowerdir ships `/testbed/dir/symlink → ../target.txt`; inside ws, `cat /testbed/dir/symlink` returns target body. | "Optimize" overlay to pass absolute lowerdir paths — breaks relative-symlink resolution after `setns(CLONE_NEWNS)`. |
| `happy_path/test_workspace_create_uses_layer_paths.py` | Intercepts `LayerStackPort.prepare_workspace_snapshot` via a recording adapter; asserts the call kwargs include `shared_layer_snapshot=True`. | Future `mount_overlay` PR flips per_call_tree_copy=True "to fix a path-not-found error" — disk inflates to O(N) silently. Cheap structural backstop. |

Enrichment: `test_lowerdir_visible_inside_mntns` (existing) gains: assert
`layer_paths` returned from `prepare_workspace_snapshot` is a non-empty
tuple; assert audit `enter` event carries `lowerdir_layer_count` and
`shared_layer_snapshot=true`.

### 19.2 Tier 2 (isolation) — 2 new tests

| File | Asserts | Innocent-refactor failure mode |
|---|---|---|
| `isolation/test_whiteout_on_upperdir_delete.py` | Inside ws: `rm /testbed/existing-from-lowerdir.txt`; subsequent `ls` doesn't show it. Exit → re-enter; the file is back (whiteout in upperdir was discarded). | "Optimize" exit to skip rmtree of upperdir.work — leftover whiteouts re-shadow next ws. |
| `isolation/test_chmod_uid_in_userns_does_not_escape.py` | Inside ws: `chmod 4755 /testbed/scratch.txt`, `chown 0:0`. From daemon's outer user-ns: `stat upperdir/scratch.txt` shows mapped UID (subuid base + 0), not real root. | Drop `--user` from `unshare` — chown 0:0 now affects real root. |

### 19.3 Tier 3 (network) — 4 new tests (inbound rejection via `unshare -n`)

> **Probe mechanism:** all 4 tests use `unshare -n` spawned from the
> daemon container's host netns, NOT a second sandbox container.
> `external_probe_sandbox` was rejected because it would collide with
> `_acquire_sweevo_session_lock(instance_id)` in
> `environments/sweevo_image/fixtures.py:78-100` (1-per-session, provider-
> agnostic — the lock guards the SWE-EVO instance regardless of whether
> `EOS_SANDBOX_PROVIDER=docker` (default) or `=daytona`). A second
> container would also double the container-provisioning latency of every
> test session. `unshare -n` exercises the same kernel surface (raw
> socket from a fresh netns trying to reach the workspace's veth peer IP)
> with no container provisioning cost.

| File | Asserts | Innocent-refactor failure mode |
|---|---|---|
| `network/test_external_inbound_tcp_rejected.py` | From `unshare -n` subproc: `socket.connect((ws_ns_ip, 22))` times out or REJECTs. Workspace's bridge subnet 10.244.0.0/24 has no inbound DNAT path from non-bridge sources. | "Refactor" the MASQUERADE postrouting rule to a permissive forward chain — opens inbound by accident. |
| `network/test_external_inbound_udp_rejected.py` | Same as above but UDP socket → port 53. | Same. |
| `network/test_external_inbound_icmp_rejected.py` | Raw ICMP echo from `unshare -n` subproc to `ws_ns_ip` — no reply. | Same. |
| `network/test_daemon_host_introspection_allowed.py` | From daemon host's own netns (where `10.244.0.1` is a bridge gateway IP): `curl --max-time 3 http://{ws_ns_ip}:8080` SUCCEEDS against an iws-internal `python -m http.server 8080`. Documents that "REJECT inbound" is scoped to forward chain, not input chain — operators can still debug from the host. | Tighten by adding a blanket DROP on the bridge interface — kills daemon-host introspection. |

Enrichment: `test_arbitrary_egress_via_masquerade` gains conntrack
RELATED/ESTABLISHED assertion for a 10 MB return-traffic download (covers
ask #2's "allow outbound" half end-to-end).

Tests are gated on `cap_net_raw` capability detection; skip with explicit
reason if unavailable.

### 19.4 Tier 6 (concurrency) — 4 new tests at N=5

| File | Asserts | Innocent-refactor failure mode |
|---|---|---|
| `concurrency/test_5_concurrent_fs_no_interference.py` | 5 agents enter; each writes distinct 1 MB to `/testbed/own.bin`; each reads back its own; cross-reads (agent-A reads agent-B's `/testbed/own.bin`) impossible. | "Share" upperdir tmpfs across handles to reduce host RAM — breaks per-ws isolation. |
| `concurrency/test_5_concurrent_network_no_interference.py` | 5 agents each start `python -m http.server 8080` (same port); each curls localhost:8080 successfully (separate netns); cross-ws curls fail (bridge port isolation). | Share netns across agents — port collision returns EADDRINUSE. |
| `concurrency/test_5_concurrent_cgroup_memory_isolated.py` | 5 agents each `dd if=/dev/zero of=/dev/shm/balloon bs=1M count=100`. Per-ws `memory.current` shows ~100 MB. Killing balloon in agent-A leaves agent-B's accounting untouched. | Move balloon writes to shared host tmpfs — accounting contaminated. |
| `concurrency/test_5_concurrent_audit_events_complete.py` | 5 agents enter concurrently (asyncio.gather). Read `sandbox_events.jsonl`: exactly 5 enter events; 5 distinct handle_ids; `phases_ms.install_veth` values exhibit contention (some > median) but no event is dropped. | Dedup-by-agent-id in audit sink → events lost under contention. |

Enrichment: existing Tier 8 `test_5_concurrent_isolated_workspaces` gains
the **contention bound**: `max(phases_ms.install_veth across 5 enters)
≤ 5 × in-test median(phases_ms.install_veth)`. Median source is **in-test,
N=21 warmup-then-sample iterations within the same test, 11th sorted value**
(Critic follow-up #7).

### 19.5 Tier 7 (gc / persistence) — 4 new tests

| File | Asserts | Innocent-refactor failure mode |
|---|---|---|
| `gc_and_persistence/test_lowerdir_layer_paths_shared_across_concurrent_handles.py` | 5 concurrent enters; read each handle's persisted `manifest_root_hash` + lease record. All 5 reference the same `layer_paths` tuple (structural equality). Lease registry shows 5 active leases on one snapshot. | "Optimize" by giving each handle its own lease copy — flips `shared-layer snapshot -> per-call tree copy`. |
| `gc_and_persistence/test_lowerdir_disk_usage_is_o1.py` | Pre-enter: `du --bytes` of layer_stack root = B. After 5 concurrent enters: `du` ≤ B + 5 × `upperdir_overhead_max` (10 MB per empty upperdir). Lowerdir didn't grow N×. | Same. Behavioral backstop to the structural check above. |
| `gc_and_persistence/test_upperdir_fully_discarded_on_normal_exit.py` | After exit: `find {scratch_root}/runtime/isolated-workspace/{handle_id}/ -type f` empty; entire dir gone (not just upper/); `manager.json` has no row; lease released. | Exit's `shutil.rmtree` catches an exception silently — strays survive. |
| `gc_and_persistence/test_upperdir_discarded_on_abnormal_exit_daemon_kill.py` | Enter; write 50 MB to `/testbed/scratch.bin`; SIGKILL daemon (not graceful); restart → triggers `startup_gc`. Assert: no scratch dir for the dead handle_id; no veth; no cgroup; no leaked IP allocation. `_reap_orphans` MUST recurse into scratch_root for handles missing from manager.json. | Startup GC walks live_set but doesn't recurse into scratch_root cleanup — leftover upperdir survives crash. |

Enrichment: existing `test_daemon_restart_reaps_orphan_scratch` gains
explicit check that upperdir bytes (not just the dir) are reclaimed and
that the `gc_orphan` audit event carries `phases_ms.{discover, reap}`
per-orphan (§15.3).

### 19.6 Tier 8 (stress) — 1 new test + enrichment

| File | Asserts | Innocent-refactor failure mode |
|---|---|---|
| `stress/test_disk_at_rest_bounded.py` | Enter; idle 60 s (no tool calls); `du --bytes {scratch_root}/runtime/isolated-workspace/{handle_id}` ≤ 10 MB. Lowerdir reference NOT counted (lives in layer_stack root, separately observed). | Pre-allocate upperdir tmpfs at `EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES` size up-front — at-rest disk balloons even with no writes. |

Enrichment: `test_5_concurrent_isolated_workspaces` gains the
`lowerdir_layer_paths_shared` cross-check (single snapshot, 5 leases) and
the contention bound on `phases_ms.install_veth`.

### 19.7 Tier 9 (NEW) — performance (7 tests, all capability-gated per §18)

```
performance/
├── test_per_op_latency_within_baseline.py
├── test_enter_phase_breakdown_complete.py
├── test_exit_phase_breakdown_complete.py
├── test_tool_call_phase_breakdown_complete.py
├── test_latency_regression_band.py
├── test_baseline_collection_invariant.py
└── test_phases_ms_subset_cover_invariant.py
```

| File | Asserts | Innocent-refactor failure mode | Capability gate |
|---|---|---|---|
| `performance/test_per_op_latency_within_baseline.py` | For each of `{enter, exit, shell, read_file, write_file, edit_file, grep}`: median `duration_s` across N=21 in-test samples is within `[0.3×, 3×]` of the session baseline median AND p95 ≤ budget × 1.5. | Add synchronous network roundtrip to a single op — baseline doubles; ratio + absolute checks both flag. | `has_run_in_handle()` |
| `performance/test_enter_phase_breakdown_complete.py` | `sandbox_isolated_workspace_enter` payload has `phases_ms` dict whose key set is a subset of `{prepare_snapshot, spawn_ns_holder, open_ns_fds, install_veth, mount_overlay, configure_dns, create_cgroup}`. All values float, all ≥ 0. SUBSET-COVER invariant holds. | Drop a phase boundary `with timer.measure(...)` — key disappears. | `has_mount_overlay()` |
| `performance/test_exit_phase_breakdown_complete.py` | Same shape for exit: keys subset of `{kill_holder, teardown_veth, release_snapshot, rmtree_scratch, cgroup_rmdir}`; SUBSET-COVER holds. Existing `lifetime_s`, `upperdir_bytes_discarded` still present (back-compat). | Combine two teardown phases — key vanishes. | `has_run_in_handle()` |
| `performance/test_tool_call_phase_breakdown_complete.py` | `phases_ms` keys are subset of `{exec}` (per §15.2). `argv0`, `exit_code` populated. SUBSET-COVER holds. | Drop the exec phase boundary — key disappears. | `has_run_in_handle()` |
| `performance/test_latency_regression_band.py` | Run `enter+shell+exit` cycle 10× in single test. Median per-phase ms is within `[0.5×, 2×]` of session baseline. Synthetic regression via `EOS_ISOLATED_WORKSPACE_TEST_PHASE_DELAY=mount_overlay:100ms` knob (test-only env) trips the band. | Add `time.sleep(0.1)` to debug `mount_overlay` — caught. | `has_mount_overlay()` |
| `performance/test_baseline_collection_invariant.py` | The session-collected `latency_baseline` fixture has all expected phase keys for `enter`; each phase's baseline > 0; baseline run count = configurable (default 3 warm-up cycles via `EOS_ISOLATED_WORKSPACE_BASELINE_RUNS`). | Drop the baseline fixture — every Tier 9 ratio test can't compute. | `has_mount_overlay()` |
| `performance/test_phases_ms_subset_cover_invariant.py` | For every audit event with `phases_ms` emitted in the session, `sum(phases_ms.values()) ≤ total_ms + max(2.0, 0.05 × total_ms)`. | Add untimed call between two phases — sum drifts below total_ms by more than slack. | None (pure audit-bus assertion). |

## 20. New fixtures + invariants

Additive to `conftest.py` / `_iws_invariants.py`. No existing helpers
renamed.

`conftest.py`:
- `latency_baseline` (session-scoped, depends on `iws_sandbox`) — runs 3
  warm-up `enter+exit` cycles, computes median per-phase `_ms` from audit
  events, yields `dict[phase_name, float]`.
- `host_netns_probe` (function-scoped) — callable
  `(target_ip, port, *, proto="tcp", timeout_s=3) ->
  {connected: bool, error: str}` that spawns an `unshare -n` subproc and
  attempts the connection. Used by all 4 Tier 3 inbound-rejection tests.
- `_capability_probe` (session-scoped) — caches probe results from §18.

`_iws_invariants.py`:
- `assert_no_upperdir_remnant(scratch_root, handle_id)`
- `disk_usage_snapshot(path) -> dict[str, int]`
- `lowerdir_layer_paths_observer(handles) -> dict[handle_id, tuple[str, ...]]`
- `assert_lowerdir_o1(before_du, after_du, handle_count, overhead_max_bytes=10_485_760)`
- `assert_independent_cgroup_memory(handle_ids, balloon_mb, tolerance_mb)`
- `assert_5_enter_events_complete(jsonl_path, agent_ids)`
- `phase_timing_extractor(event_payload) -> dict[str, float]`
- `assert_within_ratio_band(value_ms, baseline_ms, *, low, high, label)`
- `assert_subset_cover(phases_ms, total_ms, *, label)` — the §14 invariant.
- `LatencyBudget` helper class with single-call
  `assert_stable_and_within_budget(samples, op_name)` running both checks.

## 21. ADR (Architecture Decision Record)

**Decision.** Adopt the HYBRID latency baseline (session medians +
checked-in `latency_budget.json`); prepend PR 0 (`mount_overlay` +
`configure_dns` live wiring) before Tier 9; ship `tool_call` with 3 phases
for v1 (deferral ticket with sunset trigger `exec` P95 > 500 ms);
per-orphan `gc_orphan` timer (not per-pass); replace `external_probe_sandbox`
with `unshare -n` host-netns probe; capability-gate every Tier 9 test;
SUBSET-COVER invariant with generalized epsilon `max(2.0 ms, 5% × total_ms)`;
forbid `0.0` placeholder values for unrun branches (P5).

**Drivers.**
1. Test surface must scale to both per-op stability AND absolute-hardware
   regression.
2. PR ordering must match dependency ordering — Tier 9 cannot certify
   against a stubbed `mount_overlay`.
3. Audit back-compat is enumerated (`recorder.py:400-412`,
   `performance_report.py:289-295`), not aspirational.

**Alternatives considered.**
- Session-only baseline — rejected: same-PR-baked regression and multi-PR
  drift undetectable.
- `latency_budget.json`-only — rejected: brittle to CI host churn.
- "Assume external PR 0 lands first" — rejected: coordination risk; same
  team owns both.
- Widen `_Runtime.run_in_handle` Protocol now for 4-phase `tool_call` —
  rejected for v1: churn cost > diagnostic value; deferred behind sunset
  trigger.
- Per-pass `gc_orphan` timer — rejected: hides per-orphan-kind regression
  signal.
- `external_probe_sandbox` second sandbox container — rejected: sweevo
  session-lock collision (provider-agnostic — applies under
  `EOS_SANDBOX_PROVIDER=docker` default and the `=daytona` fallback alike)
  + doubled container-provisioning latency; `unshare -n` is the same
  kernel surface with no container cost.
- `sum(phases_ms) == total_ms ± 5%` equality invariant — rejected:
  mathematically impossible while any phase is stubbed; subset-cover with
  conditional keys is the only sound form.
- Hard-coded ms ceilings for Tier 9 — rejected: flakes on shared CI;
  ratio-to-baseline is the correct shape.
- New `EventType` enum values for performance — rejected by user
  requirement: additive payload fields only.

**Why chosen.**
- Hybrid baseline: only design that satisfies the regression-detection
  hard requirement AND tolerates CI variance. Maintenance cost bounded by
  ~1 refresh PR per 10 feature PRs.
- PR 0 prepended (not "assume external"): makes the dependency a hard
  constraint, removes the "6 of 7 Tier 9 tests silently skip" failure
  mode.
- 3-phase `tool_call` for v1: minimum-protocol-churn path with explicit
  sunset criterion.
- Per-orphan `gc_orphan`: matches existing aggregation in
  `performance_report.py`.
- `unshare -n` probe: no second container needed, same kernel surface.
- Capability-gated Tier 9: prevents false-coverage green CI.

**Consequences.**
- Two-class assertion in latency tests (~10 LoC `LatencyBudget` helper).
- `latency_budget.json` becomes a maintained artifact with named owner +
  refresh cadence (§17).
- Tier 9 runs as live-kernel tests; CI must allocate a kernel-capable
  runner (already exists per the `EOS_TIER_RUN_ID` infrastructure).
- `tool_call` phase data coarser than ideal for v1; mitigated by deferral
  ticket with sunset trigger.
- SUBSET-COVER invariant becomes a contract every future emitter must
  respect — new emitter PRs will fail the Tier 1 fixture test if they
  violate it. Intended forcing function.

**Follow-ups.**

1. **PR 0 decomposition declaration** (Critic follow-up #1) — three
   sub-surfaces named in PR description; accept review split request
   without re-ralplan. **Acceptance criterion** (Critic follow-up #6):
   backstop test calling `_LinuxRuntime.mount_overlay` directly and
   asserting `/proc/<pid>/mountinfo` reflects the mount.
2. **`latency_budget.json` governance** (§17, Critic follow-up #2) —
   workspace-platform on-call rotation owns refreshes; CI cron opens an
   issue on staleness; warning-not-failure during stale window.
3. **Capability probe failure policy** (§18, Critic follow-up #3) —
   pre-PR-0: skip; post-PR-0 on reference CI: fail; local-dev: skip with
   loud reason.
4. **`_Runtime.run_in_handle` widening deferral ticket** (§15.2) —
   sunset trigger `tool_call.exec` P95 > 500 ms on reference host over
   rolling 7-day window of `latency_budget.json` refresh data
   (Critic follow-up #4).
5. **Subset-cover epsilon empirical defer** (§14, Critic follow-up #5) —
   ship `ε = max(2.0 ms, 5% × total_ms)` from v1 (already generalized in
   §14); revisit if any sub-3 ms phase trips false-positive in first 30
   days post-PR-1.
6. **Document SUBSET-COVER + conditional-key emission rule** in
   `audit/events.py` module docstring so future contributors don't
   reintroduce `==` framing or `0.0` placeholders.
7. **First `latency_budget.json` refresh PR (PR 7)** lands after PR 6.

## 22. Open questions — all drained

| v1 question | Resolved in |
|---|---|
| `tool_call` 4-phase vs 3-phase | §15.2 — 3 phases v1 + deferral ticket |
| `gc_orphan` per-orphan vs batch | §15.3 — per-orphan |
| Baseline storage strategy | §15.1 — HYBRID |
| Tier 9 gating mechanism | §18 — empirical probe + capability-aware skipif/fail |
| PR 0 ownership | §16 — prepended to plan; not external |
| Audit payload version field | Rejected — adds field to every event, blast radius exceeds value |

No outstanding questions.

## 23. v2 Summary

- **Volume:** 22 new tests + 1 new tier (Tier 9: performance, 7 tests).
  Total goes from 50 → 72 tests across 10 tiers (was 9).
- **New helpers:** 10 (in `_iws_invariants.py` + `conftest.py`).
- **Audit changes:** 5 events enriched with `phases_ms` + `total_ms`; 2
  new top-level fields on enter (`lowerdir_layer_count`, `tree-copy`).
  **NO new `EventType` enum values.**
- **Source code changes:** 1 file (`service/isolated_workspace.py`) —
  `_PhaseTimer` (~15 LoC), instrument 4 methods, expand 5 `_emit` sites.
  PR 0 adds `scripts/setns_overlay_mount.py`.
- **Back-compat:** preserved (additive payload fields; existing 49 macOS
  unit tests + 50 mock-tier tests untouched).
- **CI variance handling:** HYBRID baseline; hard ceilings forbidden.
- **Capability gating:** PR 0 wires the surface; Tier 9 probes detect it
  empirically; policy is fail-loud on reference CI post-PR-0, skip in
  local-dev.
