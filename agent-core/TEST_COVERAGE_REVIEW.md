# Unit Test Coverage Review — `agent-core/crates`

_Review only — no tests were written or modified. Date: 2026-06-07._

## Scope & method

Reviewed all 16 implementation crates (`eos-testkit` is a dev-only helper crate,
excluded from grading). This is a **behavioral-coverage review anchored to the
codebase's load-bearing invariants**, not a line-percentage report. Seven
read-only agents fanned out over disjoint crate groups; each read test *bodies*
against each module's public surface and judged whether a test would actually
**fail if the behavior/invariant regressed** — as opposed to merely constructing
a value, checking it serializes, or checking "no panic".

Three test-wiring styles coexist and all were traced (a marker grep alone
undercounts):
1. inline `#[cfg(test)] mod tests { … }`;
2. `#[cfg(test)] #[path = "../tests/…/mod.rs"] mod tests;` — physically under
   `tests/` but compiled **into** the crate as unit tests with private access
   (~18 wirings);
3. true integration binaries (top-level `tests/<name>.rs`, public API only).

**Build health:** `cargo check --workspace --all-targets` → exit 0, zero
warnings. All test code compiles despite active parallel-agent edits in the
worktree. ~361 `#[test]`/`#[tokio::test]` fns total; ~25–35 of those are
architectural-guard / smoke / snapshot tests (bucketed separately below), so the
behavioral count is ≈325. **Zero `#[ignore]` / `cfg_attr(…, ignore)` anywhere in
the workspace** — so every "covered" verdict below credits a test that actually
runs, not a body that merely compiles. (The only non-running tests are the
config-/env-gated *live smoke* tests, already bucketed out.)

**Verification scope (read honestly):** coverage was assessed by **reading test
bodies** against each module's surface; the suite was **not executed** (the
worktree is mid-edit by parallel agents, so a red test would reflect someone
else's in-progress work, not a coverage gap). What is empirically confirmed:
compile-clean across all targets, and no ignored tests. "Would fail on
regression" verdicts are body-read judgments, not observed failures.

## Headline verdict

**The load-bearing invariants are genuinely guarded by behavior-asserting tests,
and assertion quality in the core is high** (byte-exact message guards, golden
snapshots that pin real serialized shapes, both-sided hook coverage, proptest
round-trips, CAS semantics, FK-cascade, column-by-column migration PRAGMA
checks). There is almost no construct-and-serialize filler.

Of the four critical-invariant areas, here is the honest "would a test fail if it
broke?" tally:

| # | Critical invariant area | Owner | Core invariant has a failing-on-regression test? |
|---|---|---|---|
| 1 | Terminal-tool enforcement + loop exit | `eos-engine`, `eos-tools` | **Split.** Enforcement (terminal-alone reject, success-stamping) = **YES** (byte-exact, `eos-tools`). Engine-internal *successful*-terminal projection + `ToolStop` loop-exit = **NO in-crate** (only via `eos-runtime` integration). |
| 2 | PLAN→RUN→CLOSED stage machine + reducer exit gate | `eos-workflow` | **YES** (`run_stage`, `reducer_is_exit_gate`, `starter` parent-running, `lifecycle` close=zero-parent-writes). Gaps are in plan-shape validation breadth + the composer. |
| 3 | Wire / DTO contracts | `eos-sandbox-port`, `eos-llm-client` | **YES for decode/parse** (`parse.rs`, `sse.rs`, `retry.rs` are golden-grade). **NO for outbound payloads** + `plugin`/`control` (untested; one confirmed schema-vs-payload divergence). |
| 4 | Destructive-shell / git-mutation prehooks | `eos-tools` | **YES** — strongest area; both *block* and *allow* sides asserted, plus advisor-approval and no-background-sessions gates. |

So: **3 of 4 areas are solidly covered for their core invariant; the 4th
(terminal enforcement) is covered on the negative/enforcement side but its
engine-internal happy-path projection + loop-exit live only in out-of-crate
integration.** The real weaknesses are concentrated and enumerable:
error/rejection branches, a few untested *success* branches (Passed-closure,
successful-terminal projection), outbound wire payloads, and a small number of
wholly-untested modules (workflow composer, sandbox-port `plugin`/`control`,
message-records `WorkflowTask`).

## Per-crate scorecard

Tier: **Strong** = core logic behavior-tested both-sided · **Mixed** = core
covered, notable enumerable gaps · **Weak** = mostly partial / contract gaps.

| Crate | src LOC | Tier | Well / Partial / Untested* | Standout strength | Biggest gap |
|---|--:|---|---|---|---|
| eos-plugin-catalog | 1908 | **Strong** | 7 / 0 / 0 | manifest reject-matrix, path-escape security | — |
| eos-config | 1493 | **Strong** | 6 / 2 / 0 | invalid-input rejection (providers/models/workflow/db-URL) | 3 `validate()` numeric-range guards lack negative tests |
| eos-agent-def | 1139 | **Strong** | 5 / 1 / 0 | parse-don't-validate, all enum tokens, real-tree loader guard | malformed-YAML error path |
| eos-skills | 741 | **Strong** | 4 / 1 / 0 | loader determinism + path-separator rejection | IO error variants |
| eos-types | 661 | **Strong** | 3 / 0 / 1† | proptest ID round-trips, UTC-normalization invariant | — (`json.rs` is a type alias) |
| eos-tools | 8356 | **Strong→Mixed** | 14 / 7 / 6 | hooks both-sided, dispatch/exec byte-exact, skill scoping (D7) | `submit_planner_outcome` validation; file-mutation executor bodies |
| eos-llm-client | 3307 | **Strong→Mixed** | 7 / 5 / 0 | SSE/retry/encode-decode + secret redaction golden | in-stream `error` events swallowed (untested); `open_stream` HTTP only via live smoke |
| eos-db | 2327 | **Mixed** | 5 / 7 / 0 | migration PRAGMA golden, CAS, FK cascade, model registry | `attempt_state_from_columns` lifecycle guards + Passed-closure untested |
| eos-audit | 934 | **Strong→Mixed** | 3 / 1 / 1 | append/ordering/flush, single-clock stamping | `Backpressure` failure mode; 4 NodeBuilder setters |
| eos-runtime | 1900 | **Mixed** | 4 / 1 / ~10‡ | request bootstrap + submit_root_outcome both branches | `default_llm_client` provider construction untested |
| eos-state | 1980 | **Mixed** | 4 / 9 / 0 | outcome projection hiding-rule, simple-enum wire strings | validator reject paths; data-carrying outcome enums |
| eos-sandbox-port | 3025 | **Mixed** | 6 / 9 / 4 | `parse.rs` + `ops.rs` golden-grade | outbound payloads unverified; `plugin`/`control` untested |
| eos-engine | 5509 | **Mixed** | 8 / 8 / 9 | notifications, supervisor ledger, provider-msg sanitization | successful-terminal exit + parent-exit async cleanup untested |
| eos-workflow | 4824 | **Mixed** | 3 / 7 / 1 | stage machine + reducer gate + compensation sagas | `composer::compose` dark; plan-shape validation breadth |
| eos-agent-message-records | 866 | **Weak** | 0 / 7 / 0 | record/event round-trip on happy path | `WorkflowTask` kind 0% exercised; `safe_segment` traversal guard + `finish()` untested |
| eos-testkit | 305 | _helper_ | — | shared test doubles (correctly has ~0 own tests) | n/a |

\* Buckets use each reviewer's significant-module count; conventions vary
slightly, so read tiers as the primary signal. † `eos-types/json.rs` is a
one-line alias (trivial by design). ‡ Most `eos-runtime` "untested" entries are
DTO/wiring/`main.rs` holders with no logic.

## Prioritized cross-cutting gaps

**P0 — load-bearing, untested, silent-failure risk**

1. **`eos-engine`: successful-terminal projection + `ToolStop` loop-exit have no
   in-crate test** (`tool_call/dispatch.rs:425`, `query/loop_.rs:247`). The
   engine's primary happy-path exit. Only the negative cases (sibling-reject,
   error-no-projection, ceiling-failure) are asserted in-crate; the positive
   path is covered only by `eos-runtime` integration. A regression that fails to
   project a terminal or break the loop passes the entire `eos-engine` suite.
2. **`eos-db`: `attempt_state_from_columns` invalid-lifecycle guards + the
   Passed-closure reconstruction are untested** (`rows.rs:415–465`; integration
   only ever closes *Failed*). This is the actual home of the Attempt persisted-
   state invariant. Highest persisted-state-integrity risk: a corrupt/migration-
   skewed row reconstructs into a wrong-but-plausible state with no failing test,
   and every *successful* attempt's reload path is unexercised.
3. **`eos-engine`: parent-exit background cleanup is untested**
   (`background/handle.rs::cancel_for_parent_exit`, `background/parent_exit.rs`
   finalizer + Drop, `background/subagent.rs` async spawn). The safety net that
   cancels/drains subagents, workflows, and command sessions on parent exit —
   concurrency-sensitive multi-phase async, exactly the code most likely to wedge
   a terminal or leak resources. Backs the supervisor-cancels/drains invariant.

**P1 — meaningful contract or validation gaps**

4. **`eos-tools`: `submit_planner_outcome` structural validation is untested**
   (`validate_planner_input` + `validate_planner_structure`, ~15 branches:
   duplicate task ids, missing/extra `task_specs`). Documented invariant
   AC-tools-12 (well-formed DAG before it reaches the port). Downstream
   `eos-workflow` tests structurally cannot reach it. Sibling gap: the sandbox
   **file-mutation executor bodies** (`write_file`/`edit_file`/`multi_edit`) —
   the primary agent write path — are untested though the `FakeTransport`
   harness already exists for the read/command tools.
5. **`eos-sandbox-port`: outbound daemon payloads are unverified** (only isolated
   enter/exit assert their payload), **and `plugin.rs` (300L) + `control.rs` are
   wholly untested.** Confirmed live divergence: `read.rs` omits the
   `description` field its DTO schema declares. The `schema_snapshot` test pins
   the schemars *schema*, **not** the hand-built payload — so it gives zero
   protection against payload drift (`terminate`, `overwrite`, `cmd`, the dynamic
   `plugin.<id>.<op>` op-name).
6. **`eos-workflow`: `composer::compose` is entirely untested and also dark in
   integration** (`composer: None` everywhere). Production launch-context + skill
   assembly (file read + frontmatter strip + terminal block). Sibling gap:
   `plan_dag::validate_plan_shape` (~7 of 8 reject branches) and the
   authoring-time `assert_acyclic` check are untested (the one cycle test hits a
   *different*, persisted-state path).
7. **`eos-llm-client`: provider in-stream `error` events are silently swallowed
   by both decoders** (no `error` match arm → `_ => {}`), untested — a provider
   error frame yields a truncated-but-"clean" stream with no `Err`. And
   `client.rs::open_stream` / HTTP status + request-id plumbing is covered only
   by config-gated live smoke tests.
8. **`eos-runtime`: `default_llm_client` provider-client construction is untested**
   (`builder.rs:378–472`). Config validation is well-tested, but the runtime's
   *use* of that config to build OpenAI/Anthropic/Codex/Claude clients —
   including "secret is required" and "missing models.active" — runs no test
   because every runtime test injects a fake client.
9. **`eos-agent-message-records`: `WorkflowTask` record kind is 0% exercised**
   (its multi-segment workflow/iteration/attempt path + `extend_payload` fields +
   `WorkflowTaskRole` labels), the **`safe_segment` path-traversal guard** has
   zero coverage (a directory-escape hole if it regresses — sibling crates test
   exactly this), and **`finish()` / `node_finished`** (the record-closing
   terminal event) is never emitted in any test.

**P2 — defensive / lower-likelihood branches**

10. **Config `validate()` numeric-range guards lack negative tests**:
    `attempt.max_concurrent_task_runs < 1`, `database.pool_size < 1`,
    `RetryConfig` negative delay — each guard could be deleted and the suite
    still passes (contrast the well-tested providers/models/workflow rejections).
11. **`eos-audit`: `Backpressure` is never exercised** — the bounded-queue-full
    drop-not-block contract is the entire reason `BufferedJsonlSink` exists; a
    regression to a blocking send would deadlock audit under load undetected.
12. **Store-mutator not-found arms are uniformly untested** across `eos-db`
    repositories (every integration assertion runs on rows that exist),
    including the real `set_task_status_if_current` missing-vs-mismatch split.

## What's genuinely strong (keep as the bar)

- **Security-sensitive surfaces are the best-covered**: destructive-shell/git
  prehooks (both block *and* allow), skill scoping isolation (D7), plugin
  path-escape, advisor-approval gate, secret redaction (Debug + Serialize).
- **Wire decode/parse contracts**: `sandbox-port/parse.rs` and
  `llm-client/sse.rs`/`retry.rs`/encode-decode are golden-grade — fixture replay,
  proptest boundary invariance, cross-provider substitutability.
- **Persistence fundamentals**: migration column-by-column PRAGMA assertions, FK
  ON DELETE CASCADE, CAS hit/mismatch, idempotent terminal finish, JSON-col
  NULL-handling + malformed-input error path.
- **Workflow lifecycle**: stage machine, reducer-as-exit-gate, concurrency-cap
  saturation, and both compensation sagas (start-failure rollback) assert final
  state, not just "no error".

## Non-behavioral tests (bucketed out — not counted as coverage)

Architectural guards: `eos-audit/no_downstream_deps`, `eos-llm-client/no_legacy_surface`,
`eos-state` source-scan guards (role naming, no class-path dispatch),
`eos-tools/tests/tools/mod.rs` security-wiring locks + totality,
`eos-plugin-catalog` no-module-import / retired-fragment guards.
Snapshots (do pin real shapes): `eos-tools` default tool specs, `eos-sandbox-port`
schema_snapshot (DTO schema only — see P1#5), `eos-state` submission schemas,
`eos-skills` SkillDefinition, `eos-plugin-catalog` LSP input schemas,
`eos-engine` prompt_report golden.
Live smoke (skip in CI, pin nothing offline): `eos-llm-client` codex / claude
coding-plan smokes — also the *only* coverage of malformed-JWT branches +
`open_stream`, but only when configured with a real token.

## Recommended next steps (if you want gaps closed)

Ordered by value/effort. Most are cheap because the harnesses already exist
(`eos-testkit` `tool_use_turn`, `FakeTransport`, `MemoryStores`, temp-SQLite):

1. P0 #1 — drive a terminal `tool_use_turn` to assert `terminal_result` is
   projected and the loop exits `ToolStop` (in-crate `eos-engine`).
2. P0 #2 — table-test `attempt_state_from_columns` over each invalid column combo
   + add a Passed-closure round-trip to `eos-db` integration.
3. P0 #3 — unit-test `cancel_for_parent_exit` / finalizer ordering with fakes.
4. P1 #4–#6 — `submit_planner_outcome` validation table; file-mutation executors
   via `FakeTransport`; a direct `composer::compose` test (+ a `with_composer`
   path in `MemoryStores`); `validate_plan_shape` reject table + `assert_acyclic`.
5. P1 #5/#7 — assert outbound payloads (catch the `read.rs` `description`
   divergence today) and add an `error`-frame decode test to both clients.
6. P2 — three config-range negative tests; one audit `Backpressure` test.
