# Phase 0 behavioral characterization

Baseline revision: `df37fe31a229bdfaf029ac994599f088efabd8b3` on
2026-07-10. This file freezes the externally observable operation behavior
that later phases compare against. The four behavior changes explicitly
approved by the specification are the only permitted differences.

`Recorded` below means an output or committed pre-existing fixture exists.
`Passed in workspace baseline` means the pre-existing test is included in
`cargo-test-workspace-baseline.txt`. `Passed in focused run` means a Phase 0
test or fixture was added after that workspace baseline and its named command
passed in the referenced characterization log.

## Characterization matrix

| Surface | Frozen behavior | Fixture or test | Verification state |
| --- | --- | --- | --- |
| Compatibility catalog JSON | Compact JSON for the management, runtime, and observability catalogs, including all CLI projection fields and operation order | `crates/sandbox-cli/tests/fixtures/compatibility-catalog.json`; `all_feature_compatibility_catalog_matches_phase_zero_fixture` | Passed in focused run (`characterization-cli.txt`, 2/2 with CLI error test) |
| CLI help | Exact aggregate help bytes, names, order, program names, usage, and examples for all three binaries | `crates/sandbox-cli/tests/fixtures/{manager,runtime,observability}-help.txt`; the three `help_lists_exact_*_catalog` tests | Passed in workspace baseline |
| CLI unknown operation | Exit 2, empty stdout, one exact `invalid_request` JSON line on stderr for all three binaries | `crates/sandbox-cli/tests/fixtures/unknown-operation-errors.json`; `unknown_operation_errors_and_exit_codes_match_phase_zero_fixture` | Passed in focused run (`characterization-cli.txt`, 2/2 with catalog test) |
| CLI request projection | Scope, defaults, native edit arrays, scoped observability rewrite to `get_observability`, and stable validation errors | `crates/sandbox-cli/tests/request_builder.rs`, `manager.rs`, `runtime.rs`, `observability.rs` | Passed in workspace baseline |
| MCP schemas | Exact `tools/list` schemas, public operation order, defaults, and absence of hidden operations for all three sets | `crates/sandbox-mcp/tests/fixtures/*-tools-list.json`; `lifecycle_and_tools_list_match_all_three_catalogs` | Passed in workspace baseline |
| Console catalog | `/api/catalog` is local, has exactly `management`, `runtime`, and `observability`, and exposes 6/7/5 public operations | `console-catalog.json`, `console-catalog.pretty.json`; `catalog_returns_all_three_execution_spaces` | Recorded and passed in workspace baseline |
| Console known RPC | `list_sandboxes` is forwarded and its success body is returned unchanged with HTTP 200 | `console-rpc-list.json`; `one_shot_injects_credentials_server_side_and_passes_result_through` | Recorded and passed in workspace baseline |
| Console unknown RPC | The current console forwards an unknown name to the gateway, then returns the gateway `unknown_op` envelope unchanged with HTTP 200 | `console-rpc-unknown.json`; `unknown_operation_is_forwarded_to_the_gateway` | Passed in focused run (`characterization-console-unknown.txt`, 1/1) |
| Console body bound | Current maximum is exactly 16 MiB; an oversized body is rejected locally with HTTP 400 and `request_too_large`, without gateway I/O | `body_over_protocol_limit_is_rejected_before_gateway_transport` | Passed in focused run (`characterization-console-limit.txt`, 1/1) |
| Daemon handshake/auth/scope | `sandbox_daemon_ready` response, daemon sandbox-scope rejection, and TCP auth stripping/error vocabulary | `crates/sandbox-daemon/tests/unit/dispatch.rs` | Passed in workspace baseline |
| Daemon observability RPC | `get_observability` dispatches cgroup/events/trace and preserves their response shapes | `crates/sandbox-daemon/tests/unit/observability.rs` | Passed in workspace baseline |
| Daemon snapshot/layerstack RPC | The same `get_observability` wire name dispatches snapshot and layerstack and returns their current top-level shape | `get_observability_wire_op_dispatches_snapshot_and_layerstack` | Passed in focused run (`characterization-daemon-observability.txt`, 1/1) |
| Runtime internal operations | Workspace-session create/destroy, squash, export/chunk, and HTTP `file_list` remain runtime-dispatchable while absent from public CLI/MCP surfaces | `workspace_session.rs`, `layerstack_squash.rs`, `layerstack_export.rs`, `file_operations.rs`, CLI/MCP hidden-operation assertions | Passed in workspace baseline |

## Exact catalog fixture

Capture commands for the raw console representation:

```bash
curl -fsS http://127.0.0.1:7880/api/catalog > \
  docs/obsidian/ephemeral-os/implementation_plan/operation-migration/evidence/phase-0/console-catalog.json
jq . \
  docs/obsidian/ephemeral-os/implementation_plan/operation-migration/evidence/phase-0/console-catalog.json > \
  docs/obsidian/ephemeral-os/implementation_plan/operation-migration/evidence/phase-0/console-catalog.pretty.json
```

The compact body is 21,269 bytes and has these keys and names:

```text
management:    create_sandbox, destroy_sandbox, list_sandboxes,
               inspect_sandbox, squash_layerstacks, export_changes
runtime:       exec_command, write_command_stdin, read_command_lines,
               file_read, file_write, file_edit, file_blame
observability: snapshot, trace, events, cgroup, layerstack
```

The executable compatibility fixture is the same 21,269 bytes followed by
one newline. Its test strips that newline before performing an exact string
comparison. The equivalence check is:

```bash
cmp -s \
  docs/obsidian/ephemeral-os/implementation_plan/operation-migration/evidence/phase-0/console-catalog.json \
  <(dd if=crates/sandbox-cli/tests/fixtures/compatibility-catalog.json \
       bs=1 count=21269 2>/dev/null)
cargo test -p sandbox-cli --all-features --test compatibility \
  all_feature_compatibility_catalog_matches_phase_zero_fixture -- --exact
```

The `cmp` succeeds. The two compatibility tests passed 2/2; command output is
recorded in `characterization-cli.txt`.

## Exact CLI output

The existing exact help tests are reproduced with:

```bash
cargo test -p sandbox-cli --all-features --test manager \
  help_lists_exact_management_catalog -- --exact
cargo test -p sandbox-cli --all-features --test runtime \
  help_lists_exact_runtime_catalog -- --exact
cargo test -p sandbox-cli --all-features --test observability \
  help_lists_exact_observability_catalog -- --exact
```

All three passed in the captured workspace baseline. Manual unknown-operation
captures used these commands:

```bash
cargo run --quiet -p sandbox-cli --features manager \
  --bin sandbox-manager-cli -- phase0_unknown_operation
cargo run --quiet -p sandbox-cli --features runtime \
  --bin sandbox-runtime-cli -- \
  --sandbox-id eos-phase0 phase0_unknown_operation
cargo run --quiet -p sandbox-cli --features observability \
  --bin sandbox-observability-cli -- phase0_unknown_operation
```

Each invocation produced exit code 2, zero stdout bytes, and the same
106-byte stderr line:

```json
{"error":{"kind":"invalid_request","message":"unknown operation: phase0_unknown_operation","details":{}}}
```

The newline-terminated stderr line hashes to
`eb7851b29ade87d19bb227ea408e34ece08e00ce972ff5fe6ae80efbff940214`.
The executable fixture uses a deliberately unreachable gateway address to
prove validation occurs before I/O:

```bash
cargo test -p sandbox-cli --all-features --test compatibility \
  unknown_operation_errors_and_exit_codes_match_phase_zero_fixture -- --exact
```

The catalog and unknown-operation compatibility tests passed 2/2; command
output is recorded in `characterization-cli.txt`.

## MCP schema fixtures

The schemas are checked as parsed JSON because JSON object member ordering is
not part of the MCP contract. Array order, names, descriptions, required
fields, types, and defaults are exact.

```bash
cargo test -p sandbox-mcp --test server \
  lifecycle_and_tools_list_match_all_three_catalogs -- --exact
```

This test passed in the workspace baseline. It also cross-checks fixture
lengths against the selected catalogs and excludes `file_list`,
`create_workspace_session`, `destroy_workspace_session`,
`get_observability`, `squash_layerstack`, `export_layerstack`, and
`read_export_chunk` from public tools.

## Console RPC captures

Body capture commands, with HTTP status printed separately, are:

```bash
curl -sS -o \
  docs/obsidian/ephemeral-os/implementation_plan/operation-migration/evidence/phase-0/console-rpc-list.json \
  -w '%{http_code}\n' -H 'content-type: application/json' \
  --data '{"op":"list_sandboxes","scope":{"kind":"system"},"args":{}}' \
  http://127.0.0.1:7880/api/rpc
curl -sS -o \
  docs/obsidian/ephemeral-os/implementation_plan/operation-migration/evidence/phase-0/console-rpc-unknown.json \
  -w '%{http_code}\n' -H 'content-type: application/json' \
  --data '{"op":"phase0_unknown_operation","scope":{"kind":"system"},"args":{}}' \
  http://127.0.0.1:7880/api/rpc
```

Both statuses were 200. Exact bodies:

```json
{"sandboxes":[]}
{"error":{"kind":"unknown_op","message":"unknown operation","details":{}}}
```

Focused verification for both new console chokepoint characterizations:

```bash
cargo test -p sandbox-console --test console \
  rpc_tests::unknown_operation_is_forwarded_to_the_gateway -- --exact
cargo test -p sandbox-console --test console \
  rpc_tests::body_over_protocol_limit_is_rejected_before_gateway_transport -- --exact
```

Both focused commands passed 1/1. Their outputs are recorded in
`characterization-console-unknown.txt` and
`characterization-console-limit.txt`.

## Internal daemon RPC and hidden runtime behavior

The existing daemon tests freeze handshake, authentication, scope, and the
`get_observability` cgroup/events/trace dispatch paths. The added wire test
closes the snapshot/layerstack dispatch gap.

```bash
cargo test -p sandbox-daemon --test unit dispatch_tests::
cargo test -p sandbox-daemon --test unit observability_tests::
cargo test -p sandbox-daemon --test unit \
  observability_tests::get_observability_wire_op_dispatches_snapshot_and_layerstack \
  -- --exact
```

The first two test groups passed in the workspace baseline except that the
new snapshot/layerstack test did not yet exist. Its focused command passed
1/1; output is recorded in `characterization-daemon-observability.txt`.

The runtime-side hidden routes are frozen by:

```bash
cargo test -p sandbox-runtime --test workspace_session
cargo test -p sandbox-runtime --test layerstack_squash --test layerstack_export
cargo test -p sandbox-runtime --test file_operations \
  dispatch_file_list_shapes_json_and_validates -- --exact
cargo test -p sandbox-mcp --test server \
  invalid_and_hidden_calls_fail_before_gateway_dispatch -- --exact
```

These tests passed as part of the workspace baseline. The route audit is the
name/class inventory; these tests are the executable behavior evidence.

## Fixture integrity

Command:

```bash
shasum -a 256 \
  crates/sandbox-cli/tests/fixtures/compatibility-catalog.json \
  crates/sandbox-cli/tests/fixtures/unknown-operation-errors.json \
  crates/sandbox-cli/tests/fixtures/*-help.txt \
  crates/sandbox-mcp/tests/fixtures/*-tools-list.json \
  docs/obsidian/ephemeral-os/implementation_plan/operation-migration/evidence/phase-0/console-*.json
```

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `compatibility-catalog.json` | 21,270 | `5e0cd1a132ebfa8eff6c045c6c976633897b0eebabc311085c7adaf4cd284282` |
| `unknown-operation-errors.json` | 937 | `e27274e85c1e286dc686551174c25cd5beb51f866775921a707f5273c5b0fc87` |
| `manager-help.txt` | 589 | `fd9f2f416508dc61e076e8ae2d79ff25d5b6b8144310db0e7688b5cc231a3a96` |
| `runtime-help.txt` | 622 | `c949d4af72a972e1aef2e8639d8f2c1d638820d8c506c7f88e451e914f101cf0` |
| `observability-help.txt` | 426 | `a8b3893a629d5bb7a42a89bcab9d6c1219de23217080592763f9ee533b017d01` |
| `management-tools-list.json` | 3,671 | `135fa04a40b312ffba6a8125d9a10e1475e3d2f471a9ce0f3805aab8148ec24c` |
| `runtime-tools-list.json` | 8,298 | `3d6a609a0b387c97a61ff94eef00eef2e37d482300085dee37439e96601da0a8` |
| `observability-tools-list.json` | 4,369 | `0a16b29eda17bb75cacd1c2cf52e4501cf4f9caf6b4b75e6577a0e3ad865ecc7` |
| `console-catalog.json` | 21,269 | `15c8c3703f4ef2955cb948d12b4cd361f98a08cfbfc57cf43bb04ba23a0e17ea` |
| `console-catalog.pretty.json` | 32,431 | `f11f9e6b5b45e1ee34986e9f200217513dfe312da5f6c006608cb434bc13f175` |
| `console-rpc-list.json` | 16 | `0a0d12c311ceffa8c435575d257737154766bce62588a6bc781d982cd2c2d0df` |
| `console-rpc-unknown.json` | 74 | `1a44406a4f9406fb24ce76ff547e0791ca5fc7af376a456a3febf64047a51337` |

## Suite evidence and open status

The pre-addition Rust baseline command was:

```bash
cargo test --workspace --all-features 2>&1 | tee \
  docs/obsidian/ephemeral-os/implementation_plan/operation-migration/evidence/phase-0/cargo-test-workspace-baseline.txt
```

Result: exit 0, 730 passed, 0 failed, 1 ignored across 112 result groups.
The log hash is
`2fceff547a4eeba53d41f10dae25d2b7dc808575300556c8827d365424ddbd94`.
The compatibility, console, and daemon tests added afterward all passed in
their focused logs. The phase standing gate still must be run.

The repository-required gateway baseline was rebuilt with:

```bash
bin/start-sandbox-docker-gateway --rebuild-binary
```

The redacted log is `gateway-rebuild-baseline.txt`, SHA-256
`383283ce633faaec87b58e298268c936533f51621eccedde73b197cc82c46c72`.

The live E2E baseline command is:

```bash
cd cli-operation-e2e-live-test
python3 -m pytest 2>&1 | tee \
  ../docs/obsidian/ephemeral-os/implementation_plan/operation-migration/evidence/phase-0/pytest-live-baseline.txt
```

The run collected 363 tests and completed in 1,226.97 seconds: 355 passed,
6 skipped, 1 failed, and 1 errored. The failure was RUN-04's in-sandbox
package installation after the external Ubuntu package index returned 404s;
the error was a one-off 129-second sandbox creation failure while Docker had
not published the expected gateway port. The gateway process remained alive,
and a subsequent sandbox creation completed in 0.36 seconds.

Both exact red nodes were then rerun together:

```bash
cd cli-operation-e2e-live-test
python3 -m pytest -vv \
  'manager/management/export/test_export_runnable.py::test_export_runnable_catalog[RUN-04]' \
  'runtime/file/smoke/test_read_smoke.py::test_session_read_of_file_created_by_session_file_write'
```

Result: 2 passed in 25.12 seconds. The full first-run evidence is
`pytest-live-baseline.txt` (SHA-256
`41ec50ef4613213e7d3576ebfad4d57dd96c99d35911888207879a00db45a21e`);
the exact rerun is `pytest-live-targeted-rerun.txt` (SHA-256
`639a9467eacd4a4d80363e9a687aa138ce3a27bef2cd2280548afe371dfd9500`).
These are recorded as environmental baseline transients, not specification
deviations. After relocation, the root-resolver tests passed 2/2 and the
broader smoke tier passed 19/19 from `e2e/`; normalized output is in
`relocation-tests.txt` (SHA-256
`63e84581b0cf7778e6927fca6208c93124937b797f1c3f3a75baf200c4cb0b5e`).
The Phase 0 standing gate remains to run.
