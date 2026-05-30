# Class & Field Simplification Plan

**Status:** Draft / analysis
**Date:** 2026-05-30
**Scope:** `backend/src` (629 classes, 311 files, 16 top-level modules)
**Supporting data:** `docs/class_inventory/` (per-module class+field reference generated from the AST)

---

## TL;DR (read this first)

The premise — "lots of redundant fields and classes" — was tested against the full
class inventory with a duplicate-field-set scan, a dead-code scan, and a 10-cluster
investigate→adversarial-refute pass. The honest finding:

- **There is almost no *safe* class-count reduction available.** Dead code is
  effectively zero (3 weak candidates). Every same-purpose duplicate that looked
  mergeable turned out to be separated by a deliberate boundary that breaks if merged.
- **The apparent redundancy is overwhelmingly intentional**: frozen DTO ↔ ORM record
  splits, discriminated-union event/block taxonomies, cross-module decoupling
  `Protocol`s, pydantic-`Input` (agent edge) ↔ internal dataclass-`Request`, and
  several *already-completed* unification refactors.
- **What is genuinely reducible in `src` logic is small and is field-repetition, not
  class count.** Two optional shared-base extractions remove ~6–22 duplicated *field
  declarations* but each **adds** one base class. One optional `Protocol`→concrete swap
  removes exactly one class.
- **The one real class-count lever is in the test harness, not application code:** ~25
  data-only scenario *leaf* classes in `task_center_runner/scenarios` (each defines
  nothing but 4 class attributes) collapse into ~3 parametrized bases + a data table —
  a behavior-preserving test refactor (§4a).

**Bottom line:** in application `src`, the safe class-count answer is ≈ `-1` and the
structure is already well-factored. The meaningful "fewer classes" win lives in the
scenario test harness (≈ `-22…-25` classes, opt-in, behavior-preserving). Anything that
merges the §3 look-alikes trades a lower class count for higher coupling and is **not
recommended**.

The most valuable output of this plan is therefore §3 — *what looks redundant but must
stay separate, and why* — plus §4a, the one large lever that is actually safe.

---

## 0. Verification update (2026-05-30) — three plan claims corrected against the live tree

Before executing, every *actionable* lever (§2/§4a/§5) was re-grounded against the
current `backend/src`. **All three "do this" items failed verification**, which
*reinforces* the TL;DR ("almost no safe class-count reduction; structure is well-factored")
but specifically retracts the concrete actions:

- **T3 (§2) is INVALID — do not execute.** The plan claimed concrete `WorkspaceProjection`
  is "already what callers construct." The construction sites say otherwise:
  `overlay_child.py:116` passes `projection=_MountedPluginProjection(...)` (a *standalone*
  class, not a `WorkspaceProjection` subclass), `runtime_api.py:284` passes
  `_workspace_projection_for_layer_stack_root(...)`, and three tests pass
  `projection=SimpleNamespace()`. `WorkspaceProjectionLike` is a genuinely polymorphic
  structural Protocol over ≥2 distinct implementers + doubles; annotating the field
  concrete would be a **type error** at `overlay_child.py:116` under the py3.11 tooling.
  Net for T3: **keep the Protocol.** The one claimed safe `src` win evaporates.

- **§5 candidate `MultiAgentEventPrinter` is NOT dead.** The dead-code scan was
  `backend/src`-scoped and missed tests: `backend/tests/unit_test/test_message/test_event_printer.py`
  constructs and exercises it across **8 cases**. Keep. (`ToolPreHook`/`ToolPostHook` remain
  structurally-unreferenced contract Protocols — keep as interface documentation; removing
  2 contract Protocols from a hot, actively-edited file is low-value.) Net §5 yield: **0**.

- **§4a is blocked by active work and the win is smaller than "−25 classes."** Two reasons,
  both verified:
  1. **Imminent collision.** The leaf files (`background_shell.py`, `ephemeral_workspace.py`,
     `plugin.py`) are *clean right now*, but the in-flight mock event-source migration
     (`docs/plans/mock_event_source_HANDOFF_2026-05-30.md`, Item 4 — "background-probe
     rewrite, NOT implemented") will **rewrite the 14 background/ephemeral leaves** to a new
     behavior model, keyed on their exact `name` strings (already hardcoded in the dirty
     `scenarios/builder.py` `_LEGACY_RUNNER_REQUIRED_SCENARIOS`). §4a now either gets rebased
     over by the migration owner or preserves behavior that is about to be replaced.
  2. **Hard test contract shrinks the win.** `tests/mock/contracts/test_scenario_suite_imports.py`
     asserts `issubclass(cls, ScenarioBase)`, `cls()`, `cls.name == key`, and
     `{scenario_cls.__name__} == set(subpackage.__all__)`. So registry values **must be named,
     exported classes** — the "collapse to a data table / instances" framing is impossible.
     The only behavior-preserving form is `type()`-synthesized classes that are still
     enumerated in `__all__`: fewer *source* `class` blocks, **same runtime class count**, all
     names still listed. The plan already concedes "keeping them is defensible."
  - The **6 plugin leaves are not under migration**, so a non-colliding *partial* §4a is
    technically possible — but it still touches the shared registry + contract and leaves a
    half-migrated state, so it is not recommended as a standalone.
  - **Recommendation: defer §4a until the migration lands.** Doing it now violates the
    CLAUDE.md no-collision rule for marginal, soon-stale gain.

**Revised bottom line:** there is **no safe, valuable, non-colliding class/field reduction to
land right now.** The plan's analysis (§3 deliberate boundaries, §6/§7 keep) stands; its three
action items do not. Pursue §4a only after the mock event-source migration completes.

---

## 1. Method (how this was derived)

1. **Inventory** — AST extraction of all 629 classes with fields/types/defaults/bases/
   kind (`docs/class_inventory/`). 0 parse errors.
2. **Field-frequency + duplicate scan** — counted field-name recurrence and grouped
   classes by identical / near-identical (Jaccard ≥ 0.7) annotated-field sets.
3. **Dead-code scan** — whole-repo word-reference count per top-level class name.
4. **Cluster investigation** — the 10 highest-signal same-layer clusters, each
   investigated (read both classes + any owning plan + importers) and classified
   `safe-merge / shared-base / dead-delete / already-planned / deliberate-keep`, then
   **every reducible verdict adversarially refuted** (a second reviewer hunting for the
   boundary that breaks; default keep-separate when uncertain).

Guiding metric: **genuine redundancy removed, never raw class count.** Merging two
deliberately-separated classes cuts the count by one and *increases* coupling — a
regression dressed as a win.

**Method limits (disclosed):** clusters were seeded from field-name overlap (Jaccard ≥
0.7), so a duplicate with *renamed* fields would be missed. Refutation was
one-directional — every *proposed reduction* was adversarially challenged (which is why
§2 is safe), but `deliberate-keep` verdicts were **not** independently re-tested, so §3's
"it's all intentional" conclusion is conservative-by-construction and bounded by that.
The highest-volume keep (the scenario classes) was given the extra distinctness check in
§4a precisely because it is the user's likely target.

---

## 2. Genuinely reducible (optional, low-risk, low-value)

> ⚠️ None of these reduce class **count** except T3. T1/T2 remove *field repetition*
> but each adds one base class. Do them only if reducing field duplication is itself a
> goal; skip them if you want fewer classes or are honoring the per-tool flat-model
> house style.

### T1 — LSP cursor-input base (`plugins/catalog/lsp/tools`)
Three input models are byte-identical on the cursor triple:
`HoverInput`, `FindDefinitionsInput`, `FindReferencesInput`
(`file_path: str`, `line: int ge=0`, `character: int ge=0` with description
`"0-based character offset on the line."`).

- **Change:** add `CursorInput(BaseModel){file_path, line, character}` in a new
  `lsp/tools/_inputs.py`; make `HoverInput`/`FindDefinitionsInput` **empty subclasses**
  (not aliases — an alias renames the published `model_json_schema` title);
  `FindReferencesInput` subclasses it and keeps only `include_declaration`.
- **Exclude** (verified, do **not** fold in):
  - `RenameInput` — its `character` description is `"0-based character offset."`
    (no "on the line"); inheriting would silently change its published schema.
  - `CodeActionsInput` — `line`/`character` default to `0`, not required.
  - `DiagnosticsInput`, `FormatInput` — no cursor fields.
- **Net:** −6 field declarations, **+1 class** (class count goes *up* by 1). 4 files
  touched + 1 new file. No external importers (only decorated tool instances are
  imported), so the agent-facing tool schemas are unchanged **iff** `Field(description=)`
  is copied verbatim onto the base.
- **Why it's marginal / what would block:** it introduces the *only* shared input base
  in a `tools/` framework whose consistent house style is one flat `BaseModel` per tool
  (20+ models, none subclass). A reviewer enforcing that convention should prefer to
  leave it.
- **Verification:** snapshot `HoverInput/FindDefinitionsInput/FindReferencesInput
  .model_json_schema()` before and after; assert byte-equal. Run the lsp plugin tests.

### T2 — File-mutation output base (`tools/sandbox` edit/write outputs)
`EditFileOutput` and `MultiEditOutput` are byte-identical (9 fields);
`WriteFileOutput` shares the same 8-field result envelope (Jaccard 0.80).

- **Change (optional):** extract `_FileMutationOutput` in `tools/sandbox/_lib` holding
  the 8 envelope fields; the three outputs subclass it and add only
  `applied_edits` / `bytes_written`.
- **Net:** ~−16 duplicated field declarations, **+1 base class**, still **3** distinct
  public `output_model` classes (per-tool agent schemas preserved).
- **Do NOT safe-merge** `EditFileOutput`≡`MultiEditOutput`: the owning plan
  (`replace_all_and_multi_edit_PLAN.md`, **implemented**) deliberately kept per-tool
  output schemas; collapsing them erases the tool-specific JSON schema each `@tool`
  advertises and forces a cross-package import.
- **Status note:** this plan already consolidated the *load-bearing* duplication (the
  projection logic, into `project_file_mutation`). It chose not to add this base. T2 is
  a field-dedup the reviewed plan skipped — pursue only if desired; it adds an
  abstraction that plan intentionally avoided.

### T3 — `WorkspaceProjectionLike` Protocol → concrete (sandbox, 1 file) — ❌ INVALID, see §0
> **Retracted.** Verification found `projection` is polymorphic (`_MountedPluginProjection`,
> `_workspace_projection_for_layer_stack_root(...)`, `SimpleNamespace` doubles); the concrete
> swap is a type error. Keep the Protocol. The claim below is left for the record.
`WorkspaceProjectionLike` is a structural `Protocol` used only as the
`PluginOpContext.projection` annotation, with **zero external importers**; the concrete
`WorkspaceProjection` lives in the same package and is already what callers construct.

- **Change:** annotate with the concrete `WorkspaceProjection` (mirrors the documented
  `overlay->Any` precedent). **Net: −1 class.** One-file change, no importer impact.
- **Caveat:** only valid while `WorkspaceProjection` stays in the plugin package. If it
  ever moves cross-module, the Protocol becomes genuine import-avoidance again — keep
  this reversible and comment why.

**Combined safe impact: net class count −1 (T3) to +1 (with T1), field declarations −6
to −22.** That is the entire safe surface.

---

## 3. Looks redundant — but is deliberate. **Do NOT merge.**

These are the field-set "duplicates" the scan surfaced. Each was investigated and the
merge **refuted** with a concrete boundary. This table exists to prevent a future
well-intentioned but damaging consolidation.

| Look-alike pair/group | Why they must stay separate (the boundary that breaks) |
|---|---|
| `db.WorkflowRecord` ≡ `task_center.Workflow`; `AttemptRecord`~`Attempt`; `IterationRecord`~`Iteration` | **Persistence boundary.** ORM record vs frozen DTO is the architecture's mandated split (stores return frozen DTOs, never leak ORM objects). Merging re-introduces exactly the coupling the split prevents. |
| `sandbox.GrepRequest` ≡ `tools.GrepInput`; `GlobRequest`≡`GlobInput`; `SearchReplaceEdit`≡`MultiEditOp` | **Shipped API edge.** pydantic `Input` (validated, agent-facing schema) vs internal sandbox dataclass `Request`. This is the *completed* `unify_sandbox_workspace` end-state (plans executed and archived in `f1a952acf`); the two layers are intentional. |
| `GeneratorSubmission` ≡ `EvaluatorSubmission` | **Silent positional break.** Both are frozen+slots dataclasses; `outcome` is field 3 of 5 and its `Literal` differs (`blocker` only on Generator). A shared base lays base fields first, reordering the positional signature of a public-facade DTO. |
| `Scenario` (Protocol) ≡ `ScenarioBase`; 3 sandbox scenario bases | `Scenario` is a `@runtime_checkable` Protocol used via `isinstance` and as `type[Scenario]` registry/signature bound — it is the structural contract, not a duplicate of the concrete base. |
| `ThinkingDeltaEvent` ≡ `AssistantTextDeltaEvent`; `SystemNotification` | **Dispatch keys.** The distinct types *are* the routing discriminator; `message-unification-refactor.md` (implemented) explicitly rejected further taxonomy collapse. `SystemNotification` also crosses the notification↔message layer. |
| `TextBlock` ≡ `ThinkingBlock` ≡ `SystemNotificationBlock` | Deliberate pydantic discriminated-union (`ContentBlock`); the discriminator type is the point. Only shared field is `text: str`. |
| `ContextRefs` ≡ `ContextScope` | Kept separate by `agent_initial_messages_restructure_PLAN.md` (implemented); different roles (packet refs vs recipe scope). |
| `AgentNotificationRule` (Protocol) ≡ `NotificationRule` | **Dependency inversion + pydantic forward-ref cycle.** Protocol exists so `agents` need not runtime-depend on `notification` (breaks a `QueryContext` cycle). |
| `ChangesetResultLike` (Protocol) ≡ `ChangesetResult` | **Layer direction.** Collapsing forces `sandbox/shared` to import `sandbox/occ`, inverting the dependency. |
| `LayerStackSnapshotLease` ~ `WorkspaceSnapshotLease` | Dependency-inversion Protocol/concrete across the `shared`↔`layer_stack` boundary; merge forces `shared` to import `layer_stack` (breaks the no-internal-imports rule). |
| `RawExecResult` ~ `ShellResult` | Distinct result bases (raw-exec vs OCC-guarded) on separate provider/tool pipelines; only 3 trivial shared fields. |
| 3 `*BackgroundTask*Tool` classes | Shared field-set *is* the `BaseTool` ABC contract (already DRY); `input_model`/`execute` bodies fully diverge. |

---

## 4. Already addressed by shipped plans (no action)

- `replace_all_and_multi_edit_PLAN.md` (implemented) — owns the edit/write-output cluster.
- `message-unification-refactor.md` (implemented 2026-05-27) — owns the event/block taxonomy.
- `agent_initial_messages_restructure_PLAN.md` (implemented) — owns `ContextRefs`/`ContextScope`.
- `task_center_naming_refactor_PLAN.md` — already relocated the submission DTOs into `submissions.py`.
- `unify_sandbox_workspace*.md` (executed, archived) — the sandbox `Request`↔tool `Input` layering.

---

## 4a. The one real class-count lever: scenario parametrization (test harness) — ⏸ DEFER, see §0
> **Blocked now.** The 14 background/ephemeral leaves are slated for imminent rewrite by the
> in-flight mock event-source migration, and `test_scenario_suite_imports.py` forces named
> exported classes (so the win is `type()`-synthesis, not a data table — same runtime class
> count). Defer until that migration lands. Detail below stands as the design.

`task_center_runner/scenarios` holds **80 of the module's 150 classes**. Inspecting
their methods/bases (not guessing) splits them cleanly:

| Group | Count | Evidence | Verdict |
|---|---|---|---|
| **Distinct-behavior scenarios** | ~42 | define real scripted bodies — `planner_response` (40), `executor_actions` (41), `evaluator_response` (40), `verifier_response`, `hooks` | **Keep** — genuinely distinct test logic |
| **Value-object specs** | ~13 | rich field dataclasses used *by* scenarios: `ToolCallSpec`, `ScenarioContext`, `LspExpectation`, `RefactorPass`, `Patch`, `FixtureFile`, `RequirementItem`, `WorkPackage`, … | **Keep** — typed building blocks |
| **Data-only leaf scenarios** | **~25** | subclass `_BackgroundShellScenarioBase` (13) / `_EphemeralWorkspaceScenarioBase` (6) / `_PluginScenarioBase` (6); **0 methods, 0 annotated fields**, body is only `name, action_id, action_spec, summary_path_hint` class attrs | **Collapsible → parametrized table** |

**The lever:** those ~25 leaves are pure data. Each is literally
`class BackgroundShellStop(_BackgroundShellScenarioBase): name=...; action_id=...;
action_spec=...; summary_path_hint=...`. They can collapse into the 3 existing bases
driven by a list of `(name, action_id, action_spec, summary_path_hint)` records — a
behavior-preserving refactor that removes ~22–25 class definitions while keeping every
test case.

**Caveat / sizing (why it's a scoped effort, not a one-liner):**
- Scenarios are addressed by **name** through an explicit `SCENARIO_REGISTRY: dict[str,
  type[Scenario]]` in `scenarios/__init__.py`. The registry currently maps name → class;
  it would map name → a constructed/parametrized scenario. Every leaf's `name` (the
  registry key) must be preserved exactly.
- Each leaf is referenced in ~3–4 files (its def, the registry, 1–2 tests/matrices).
  Total touch is moderate and mechanical, but real.
- This is a **test-harness** change with no application-logic risk; verify by running the
  mock scenario suite and asserting the set of registry keys + each scenario's
  `expected_event_sequence` is unchanged before/after.

**Recommendation:** this is the single biggest honest answer to "reduce the number of
classes." It is opt-in and orthogonal to §2/§3. If class count is the goal, **do this
one**; it dwarfs every `src` change combined. If the per-scenario-class style is valued
for readability/grep-ability of the test matrix, keeping them is also defensible — but
now that trade-off is explicit.

---

## 5. Dead code: effectively none — verified yield 0, see §0

The "whole-repo" scan was actually `backend/src`-scoped and surfaced 3 weak candidates.
After checking `backend/tests`:
- `message.MultiAgentEventPrinter` — **NOT dead**: exercised by 8 cases in
  `backend/tests/unit_test/test_message/test_event_printer.py`. Keep.
- `tools._framework.core.hooks.ToolPreHook` / `ToolPostHook` — structurally-unreferenced
  contract Protocols in a hot, actively-edited file (`tool_call.py`). Keep as interface
  documentation; removing them is low-value and risky against in-flight edits.

**Verified yield: 0 classes.** Not a lever.

---

## 6. The field-repetition lever (correlation IDs) — and why it is *not* recommended

The real "many repeated fields" signal is the **correlation-id cluster**: `task_id` (17
classes), `run_id`/`attempt_id`/`tool_use_id` (12 each), `workflow_id`/`agent_id`/
`lease_id` (10 each), threaded through many flat DTOs. An identity envelope (the
`audit.AuditNode` pattern, generalized) would cut this repetition.

**Not recommended as a blanket change.** These DTOs are deliberately flat, frozen,
slotted, and individually serialized; an embedded envelope adds nesting/indirection to
hot-path packets and event objects, churns every construction and serialization site,
and fights the established design for a cosmetic field-count win. If field repetition is
a hard requirement to reduce, scope it to **one** new DTO family as a spike and measure
the construction/serialization cost before generalizing — do not sweep it across
existing state/event types.

The `expected_event_sequence` field (41 classes) is the test-scenario DSL — see §7.

---

## 7. Long-tail triage (categorical, not per-class)

- **68 single-annotated-field classes** — value objects / typed config (e.g.
  `ReadFileRequest`, `CommitOptions`, config sections). Keep: they carry type identity
  and validation; inlining trades a name for a primitive. **Out of scope.**
- **41 `Protocol`s + `*Like` shims** — overwhelmingly cross-module decoupling seams
  (dependency inversion / cycle avoidance). Keep, except the single T3 case. Do **not**
  treat "single-impl Protocol" as removable.
- **5 `Mixin`s** — compositional lifecycle splits of large sandbox classes; keep.
- **`task_center_runner` scenario classes** — analyzed in **§4a** (not hand-waved):
  ~42 carry distinct scripted behavior (keep), ~13 are value-object specs (keep), and
  **~25 are data-only leaves that are the one real class-count lever** (parametrize).
  See §4a for the breakdown, sizing, and registry caveat.

---

## 8. Recommendation

> **Superseded by §0 (2026-05-30 verification).** Post-verification, the only standing action
> is "defer §4a until the mock event-source migration lands; land no `src` change now (T3 is a
> type error, §5 yield is 0)." The original ordering is kept below for the record.

Ordered by value for the stated goal (fewer classes/fields):

1. **If you want a real class-count reduction: do §4a** — parametrize the ~25 data-only
   scenario leaves (≈ −22…−25 classes, behavior-preserving, test-harness only,
   registry-key-preserving). This is the single biggest safe lever and the most likely
   match for the perceived redundancy.
2. **Application `src` structural changes: default to none.** It is already well-factored;
   the §3 look-alikes are deliberate boundaries. Adopt §3 as a guardrail so they aren't
   "simplified" later.
3. **Small safe `src` wins (optional):** **T3** (−1 class, trivial) and optionally
   **T1**/**T2** (field-dedup, each +1 base class). Verify published JSON schemas are
   byte-unchanged.
4. **Verify** the 3 dead-code candidates (§5); remove only if confirmed unused.
5. **Do not** pursue §6 broadly or any §3 merge.

**Net realistic safe impact:**
- Application `src`: classes −1 to +1, field declarations −6 to −22 — i.e. negligible;
  the structure is sound.
- Test harness (§4a, opt-in): classes ≈ −22 to −25, no coverage change.

The honest conclusion: there is no large simplification to make in application code
without trading factoring for coupling. The one substantial "fewer classes" win is the
scenario parametrization in the test harness, which is safe and isolated.

---

### Appendix: reproduction
Analysis scripts (ephemeral, under `/tmp`): `eos_class_extract.py` (inventory),
`eos_redundancy.py` (duplicate scan), `eos_clusters.py` (clusters + dead-code),
`eos_deadcode_strict.py`. The durable inventory is `docs/class_inventory/`.
