# impl-eos-agent-def — agent profile definitions, loader, app-state registry, and pure validation

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §5 (`eos-agent-def`).

## 1. Purpose & Responsibility (SRP)

`eos-agent-def` owns the **static identity and declared surface of an agent
profile**: the `AgentType` / `AgentRole` vocabularies, the `AgentDefinition`
value type (all fields from `model.py`), the Markdown+frontmatter `loader`, and
the `AgentRegistry` lookup table that downstream crates query at spawn time. It
also owns the *pure* fragments of profile validation that need no other crate:
the `context_recipe` role-gating precheck and the skill-file terminal-silence
scanner.

This crate must **NOT**: build `ToolSpec`s, resolve the `model: inherit`
sentinel, filter or materialize the effective visible tool set, own the
`allowed_tools ∪ terminals` union policy, resolve notification-trigger ids, run
the `context_recipe` *catalog* check (that needs `eos-workflow`'s
`ContextEngine`), own `TERMINAL_DESCRIPTORS` (that is `eos-tools`), or introduce
any agent orchestrator above workflow attempts. The registry is a lookup table,
not a scheduler. Profile resolution, tool materialization, and lifecycle all
live in `eos-engine` / `eos-workflow` / `eos-runtime`.

## 2. Dependencies

- **Upstream crates (depends on):** `eos-types` (only for `CoreError`
  interop conventions and a potential shared error trait; the data types here
  are self-contained). This crate is a **near-leaf**.
- **Downstream consumers (used by):** `eos-engine` (agent factory resolves a
  definition into a runnable agent), `eos-workflow` (planner-submission gate
  reads `role`; composition root runs the recipe-catalog check), `eos-runtime`
  (builds the registry at startup, runs cross-crate validation).
- **External crates:**

  | Crate | Justification | rust-skills |
  |---|---|---|
  | `serde` (derive) | (De)serialize `AgentDefinition`, enums, frontmatter DTOs. | `type-no-stringly`, `api-common-traits` |
  | `serde_yaml` | Parse YAML frontmatter block of profile `.md` files. | `api-parse-dont-validate` |
  | `schemars` | `JsonSchema` derive on wire/DTO types (anchor §9); drives the Phase-0 schema-snapshot parity harness (anchor §11). | `api-common-traits` |
  | `thiserror` | The crate's single typed error enum `AgentDefError`. | `err-thiserror-lib` |

  All versions come from `[workspace.dependencies]` via inheritance
  (`proj-workspace-deps`); this crate pins nothing locally.

  **Deliberate non-dependencies (cycle avoidance):** the Python code imports
  `config.markdown.parse_markdown_frontmatter` (→ `eos-config`),
  `workflow.context_engine.engine.validate_context_recipe` (→ `eos-workflow`),
  and `tools._terminals.registry.TERMINAL_DESCRIPTORS` (→ `eos-tools`). All three
  edges point **toward crates that depend on this one**, so importing them would
  create a Cargo-illegal cycle. Resolutions: inline a small frontmatter split
  here (no `eos-config` edge); relocate the recipe-catalog check to
  `eos-workflow`; accept terminal keys as injected `&[&str]` data in the scanner
  (no `eos-tools` edge). See §5, §8, §9 and GC-eos-agent-def-04/05.

## 3. Scope & Source Mapping

| Python source | Rust target | What moves / what is dropped |
|---|---|---|
| `agents/definition/model.py` | `model.rs` | `AgentType`, `AgentRole`, `AgentDefinition` + construction-time constraints. Pydantic validators → parse-don't-validate construction. |
| `agents/definition/loader.py` | `loader.rs` | `.md` discovery, `_*.md` skip, `_main_role_contract.md` prepend, skill-path resolution, name/description defaults, **required `role`**. Frontmatter split inlined (drops `config.markdown` edge). |
| `agents/definition/registry.py` | `registry.rs` | `get` / `list` / `list_dispatchable_subagent_names`. **Drops** global mutable `_DEFINITIONS` dict and runtime `register`/`unregister` mutation (test-runner-only; see §7). Adds `AgentRegistryBuilder`. |
| `agents/definition/resolved_validation.py` | `validation.rs` | **Only** the pure `context_recipe` role-gating precheck. The `validate_context_recipe` catalog call is relocated to `eos-workflow`. |
| `agents/skills/loader.py` | `validation.rs` (`skill_lint` mod) | `scan_skill_file` with **injected** terminal keys; `submit_*` regex; aggregated violations. The `TERMINAL_DESCRIPTORS` import is dropped (keys passed in as data). |

**In-scope:** profile data model, file loader, lookup registry, pure validation.
**Out-of-scope:** model resolution, tool materialization, notification-trigger
resolution, recipe-catalog validation, `test_runner` mock definitions, any
orchestration.

## 4. File & Module Layout

```
eos-agent-def/
  src/
    lib.rs          // pub use re-exports (proj-pub-use-reexport); crate-level docs
    error.rs        // AgentDefError (single thiserror enum, err-thiserror-lib)
    model.rs        // AgentType, AgentRole, AgentName, AgentDefinition
    loader.rs       // frontmatter split + load_agents_dir / load_agents_tree
    registry.rs     // AgentRegistryBuilder + AgentRegistry (read-only, Arc-shared)
    validation.rs   // context_recipe role precheck; skill_lint submodule
  tests/
    loader_profiles.rs   // integration: load the real profile/ tree fixtures
```

`lib.rs` re-exports the public surface; `error.rs` internals stay `pub(crate)`
where not part of the API (`proj-pub-crate-internal`). `missing_docs` is warned
per workspace lints.

## 5. Contracts Owned Here

Per the Ownership Map (anchor §5), this crate owns `AgentDefinition`,
`AgentRole`, `AgentType`, `AgentRegistry`, and context-recipe metadata. Sealed
nothing — there is no trait meant for external impl here; the only seam is the
`AgentRegistry` (anchor §6, `OCP` via registration at build time).

- **`AgentDefinition`** — owned value type. Fully specified in §6.
- **`AgentRole` / `AgentType`** — owned closed enums. Fully specified in §6.
- **`AgentRegistry`** — owned read-only lookup; built via `AgentRegistryBuilder`.
  Signature sketch:

  ```rust
  pub struct AgentRegistry { /* HashMap<AgentName, Arc<AgentDefinition>> */ }

  impl AgentRegistry {
      #[must_use] pub fn get(&self, name: &AgentName) -> Option<&Arc<AgentDefinition>>;
      #[must_use] pub fn list(&self) -> impl Iterator<Item = &Arc<AgentDefinition>>;
      /// Subagent names targetable by `run_subagent`, sorted (registry.py parity).
      #[must_use] pub fn dispatchable_subagent_names(&self) -> Vec<AgentName>;
  }
  ```

  It is **not** a trait and not behind `dyn`; it is a concrete struct stored as
  `Arc<AgentRegistry>` in `AppState`. No `async`, no object-safety concern.

  *Naming note:* `registry.py`'s `get_definition` / `list_definitions` /
  `list_dispatchable_subagent_names` are renamed to `get` / `list` /
  `dispatchable_subagent_names` (idiomatic Rust; behavior preserved).

**Contracts merely USED (reference only — do not redefine):**

- `ToolName` (typed tool-name constants) — **owned by `eos-tools`**, see
  `impl-eos-tools.md` / anchor §5. This crate stores tool names as plain
  `Vec<String>`; resolution to `ToolName`/`ToolSpec` happens in `eos-engine`.
- `ToolSpec` — **owned by `eos-llm-client`**, see `impl-eos-llm-client.md`.
  Materialized at spawn by `eos-engine`, never here.
- `validate_context_recipe` (recipe-catalog check) — **owned by `eos-workflow`**,
  see `impl-eos-workflow.md`. Invoked at the composition root, not here.
- `TERMINAL_DESCRIPTORS` keys — **owned by `eos-tools`**, see `impl-eos-tools.md`.
  Passed into `scan_skill_file` as `&[&str]` data.

## 6. Types, Fields & Schemas

### `AgentType` (closed enum, source: `model.py:AgentType`)

| Variant | serde value |
|---|---|
| `Agent` | `agent` |
| `Subagent` | `subagent` |

### `AgentRole` (closed enum, source: `model.py:AgentRole`)

| Variant | serde value | Notes |
|---|---|---|
| `Root` | `root` | the root request agent |
| `Planner` | `planner` | authors the attempt DAG |
| `Generator` | `generator` | does the work; **the `executor` profile maps here** |
| `Reducer` | `reducer` | digests / exit gate |
| `Helper` | `helper` | advisor |
| `Subagent` | `subagent` | explorer |

Both enums derive `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize,
Deserialize, JsonSchema`. **KISS call: NOT `#[non_exhaustive]`.** These are
closed vocabularies; the planner-submission gate and audit tag rely on
exhaustive `match`. Anchor §9's `#[non_exhaustive]` guidance is for enums that
"may grow" — this set is fixed by design (`type-enum-states`). `executor` stays a
**profile-name alias** carrying `role: generator`; it never enters the role
state (anchor §4: "`executor` is at most a profile alias; never enters state").

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole { Root, Planner, Generator, Reducer, Helper, Subagent }
```

### `AgentName` (newtype, format-only)

`pub struct AgentName(String)` — `type-newtype-ids` for the registry key and
`dispatchable_subagent_names` return. Construction trims and rejects empty.
Empty-rejection is a **new Rust invariant** (parse-don't-validate hardening,
anchor §9 `type-newtype-ids`): `model.py` has no name validator. To preserve
parity with `loader.py:54-55` (a missing/blank `name:` falls back to
`path.stem`), the loader applies the `path.stem` default **before** constructing
`AgentName`, so the non-empty newtype only ever sees the resolved stem, never the
raw blank. It is a **format** newtype only: it does NOT check membership against
a tool/agent catalog (that would belong to a resolver, not this leaf crate).

### `AgentDefinition` (owned struct, source: `model.py:AgentDefinition`)

| Field | Rust type | serde / schemars | Source-of-truth |
|---|---|---|---|
| `name` | `AgentName` | required | `model.py` required |
| `description` | `String` | required | `model.py` required |
| `system_prompt` | `Option<String>` | default `None` | `model.py` `str \| None` |
| `model` | `Option<String>` | default `None` | `model.py` `str \| None`; `"inherit"` sentinel resolved **in eos-engine**, kept raw here |
| `tool_call_limit` | `NonZeroU32` | required | `model.py` `Field(..., gt=0)` → parse-don't-validate |
| `role` | `AgentRole` | **required on file-parse path** (see §8) | `model.py`; loader rejects missing `role` |
| `agent_type` | `AgentType` | `#[serde(default)]` → `Agent` | `model.py` default `AGENT` |
| `allowed_tools` | `Vec<String>` | `#[serde(default)]` | `model.py` `list[str]`; **not** `Vec<ToolName>` (see §8) |
| `terminals` | `Vec<String>` (non-empty by construction) | required | `model.py` `Field(..., min_length=1)`; blanks stripped |
| `notification_triggers` | `Vec<String>` | `#[serde(default)]`; blanks stripped | `model.py` `list[str]` |
| `skill` | `Option<PathBuf>` | default `None`; absolute after loader resolution | `model.py` `Path \| None` |
| `context_recipe` | `Option<String>` | default `None` | `model.py` `str \| None` |

Derives `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`. The struct
maps `extra="forbid"` → `#[serde(deny_unknown_fields)]` (GC-eos-agent-def-02). It
is **not** `#[non_exhaustive]` (it is a fully-specified config record consumed by
exhaustive constructors). No `Default` impl: `name`, `description`,
`tool_call_limit`, `terminals` have no sensible default (`api-default-impl` —
"when some fields have no sensible default, don't implement Default").

Construction enforces invariants so an invalid `AgentDefinition` is
unrepresentable (`api-parse-dont-validate`): `tool_call_limit` is `NonZeroU32`;
`terminals` is validated non-empty after stripping blanks; blank
`notification_triggers` are dropped. A serde-deserialized struct funnels through
a `TryFrom<RawAgentDefinition>` step that runs these checks:

```rust
impl AgentDefinition {
    /// Parse a frontmatter map into a validated definition.
    /// # Errors
    /// Returns [`AgentDefError`] when `terminals` is empty, `tool_call_limit`
    /// is not positive, or `role` is absent on the file-parse path.
    pub(crate) fn from_frontmatter(raw: RawAgentDefinition) -> Result<Self, AgentDefError> { /* ... */ }
}
```

### Error type (`error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentDefError {
    #[error("agent profile {path} is missing required 'role' frontmatter")]
    MissingRole { path: PathBuf },
    #[error("agent definition terminals must be non-empty")]
    EmptyTerminals,
    #[error("tool_call_limit must be positive")]
    NonPositiveToolCallLimit,
    #[error("could not read agent profile {path}")]
    Read { path: PathBuf, #[source] cause: std::io::Error },
    #[error("invalid frontmatter in {path}")]
    Frontmatter { path: PathBuf, #[source] cause: serde_yaml::Error },
    #[error("agent profile {path} declares skill {declared}, but {resolved} does not exist")]
    SkillNotFound { path: PathBuf, declared: String, resolved: PathBuf },
    #[error("agent {agent} declares context_recipe {recipe}, but role {role} has no context builder")]
    RecipeRoleMismatch { agent: String, recipe: String, role: AgentRole },
    #[error("skill-file lint failed")]
    SkillLint { violations: Vec<String> },
}
```

Messages are lowercase, no trailing punctuation (`err-lowercase-msg`); `#[source]`
chains the underlying cause (`err-source-chain`); the enum is `#[non_exhaustive]`
because it *may* grow (`api-non-exhaustive`), unlike the closed role/type vocab.

## 7. Concurrency & State Ownership

- **Runtime-agnostic.** This crate spawns nothing and holds no runtime; the
  loader's filesystem reads are synchronous and run at startup (or via
  `spawn_blocking` if the caller in `eos-runtime` chooses — not this crate's
  concern). All public methods take `&self`.
- **`AgentRegistry` is built-once then immutable-shared.** `eos-runtime` builds
  it via `AgentRegistryBuilder::add(def)` during startup, finalizes with
  `.build()`, and stores `Arc<AgentRegistry>` in `AppState` (anchor §7
  "shared immutable state: `Arc<T>`, cloned cheaply", `own-arc-shared`). Reads
  (`get`/`list`/`dispatchable_subagent_names`) are lock-free `&self` lookups
  against an internal `HashMap`. **No `Mutex`/`RwLock`, no lock-across-await
  concern** because there is no shared mutation after build.
- **Runtime register/unregister is dropped (YAGNI).** The Python global
  `_DEFINITIONS` dict with `register`/`unregister` is mutated only by
  `test_runner` (verified in this checkout: the sole runtime
  `register_definition` callers are `test_runner/core/bootstrap.py:59` and
  `test_runner/agent/mock/definitions.py`; production code only *reads* the
  global registry — `runtime/entry.py:186` `get_definition("root")`,
  `tools/subagent/_factory.py`, `workflow/attempt/launch.py`, etc.). Python has
  **no** production startup-build phase: `runtime/entry.py` never populates the
  registry, so today the only path that fills it is the `test_runner` bootstrap.
  The Rust redesign **introduces** that explicit startup-build via
  `eos-runtime`'s `AgentRegistryBuilder` (a phase Python lacked) and exposes no
  runtime mutation seam (anchor §2 KISS/YAGNI). If a future need appears, the
  seam is "rebuild a new `Arc<AgentRegistry>` and swap", not in-place mutation.

## 8. Behavior & Invariants

Semantics preserved from the Python source (cite per item):

1. **Effective visible tool set = `allowed_tools ∪ terminals`, materialized as
   concrete `ToolSpec`s at spawn — NOT here.** `engine/agent/factory.py:228`
   (in `_build_agent_tool_registry`) computes
   `sorted(set(allowed_tools) | set(terminals))` and hands it to
   `_register_requested_tools`, which **skips unknown names with a warning**
   (`engine/agent/factory.py:252`) at spawn. This crate therefore stores both
   lists as plain `Vec<String>` and performs **no** union, no registry check, no
   `ToolSpec` build. Encoding tool names as a registry-validated `ToolName` here
   would reject names Python deliberately tolerates and would require an illegal
   `eos-tools` edge. The union + spawn-time `Vec<ToolSpec>` materialization is an
   **eos-engine invariant** (see `impl-eos-engine.md`). Final Rust startup
   validation is an **eos-runtime** responsibility after tool/agent registries
   are both built: unknown tool names fail fast unless an explicit compatibility
   mode is enabled.
   This is the anchor §2 non-goal: **no tool visibility enum, no lazy loader.**
2. **`role` is required on the file-parse path; the struct default is
   test-only.** `loader.py:64` raises when frontmatter omits `role`. The Pydantic
   `role = GENERATOR` default exists only so direct test construction stays
   terse. In Rust there are two construction paths: file-parse (`role` absent →
   `AgentDefError::MissingRole`) and direct in-test construction (caller supplies
   `role` explicitly). `role` is therefore **not** `#[serde(default)]`.
3. **`extra="forbid"` → `#[serde(deny_unknown_fields)]`.** Unknown frontmatter
   keys are rejected at parse (`model.py:100`).
4. **Loader file rules** (`loader.py`): files named `_*.md` are private includes
   and skipped (`load_agents_*` iterate sorted); for a non-underscore profile
   directly under a `main/` directory, prepend `_main_role_contract.md` body to
   `system_prompt` (`<contract>\n\n<body>`); `name` defaults to file stem,
   `description` defaults to `Agent: <name>`; a declared `skill:` is resolved
   relative to the profile file, made absolute, and **must exist** else
   `SkillNotFound`. `load_agents_dir` globs one level; `load_agents_tree`
   recurses.
5. **`model` sentinel stays raw.** `model: "inherit"` is preserved verbatim;
   `engine/agent/factory.py:189` (in `_resolve_agent_identity`) resolves it
   against the launching context. This crate never interprets it.
6. **`context_recipe` role-gating precheck is pure and stays here.**
   `resolved_validation.py:42` rejects a `context_recipe` declared by a role
   outside `{planner, generator, reducer}` → `AgentDefError::RecipeRoleMismatch`.
   The *catalog* validity check (`validate_context_recipe`) is owned by
   `eos-workflow` and invoked at the composition root after registry build
   (GC-eos-agent-def-04).
7. **Skill terminal-silence lint is pure with injected keys.**
   `skills/loader.py` scans each declared skill body for `submit_*` tokens and
   for any `TERMINAL_DESCRIPTORS` key as a substring, aggregating violations.
   The Rust `scan_skill_file(body: &str, terminal_keys: &[&str]) -> Vec<String>`
   keeps the `submit_[A-Za-z0-9_]+` regex and the substring scan but takes the
   terminal keys as **data** (passed by `eos-runtime` from `eos-tools`), removing
   the cyclic edge (GC-eos-agent-def-05). Unlike Python's `scan_skill_file(path)`,
   which reads the file and calls `parse_markdown_frontmatter` internally, the
   Rust scanner takes an already-stripped `body`: the **loader** (which already
   performs the frontmatter split for profiles, §4/§8.4) supplies the body, so
   author metadata cannot false-positive. The scanner owns no file I/O.

**Non-goals respected:** no orchestrator above attempts (GC-eos-agent-def-03);
no tool visibility abstraction (GC-eos-agent-def-06); registry is lookup only.

## 9. SOLID & Principles Applied

- **SRP:** the crate is exactly "profile identity + load + lookup + pure
  validation." Tool materialization, model resolution, recipe-catalog checks,
  and orchestration live elsewhere (§1).
- **OCP:** new agent profiles are added by *registering a definition* into the
  `AgentRegistryBuilder` (anchor §6 `AgentRegistry` seam) — never by editing a
  `match`. Adding a profile is a data change, not a code change.
- **ISP:** `AgentRegistry` exposes three small read methods; consumers depend
  only on what they call. No god-object.
- **LSP:** `AgentRole`/`AgentType` are exhaustive enums so every consumer
  handles every variant; substitutability holds across all profiles.
- **DIP:** the cyclic Python imports are inverted by **data injection** (terminal
  keys passed in) and **relocation** (recipe-catalog check moves to its owner).
  This crate depends on no downstream crate, keeping the DAG acyclic.
- **KISS/YAGNI/DRY:** no runtime registry mutation (test-only), no `Default` for
  a required-field record, no tool-name resolution, closed enums not
  `#[non_exhaustive]`. One definition per contract (anchor §5).

## 10. Gap Closeouts (tracked requirements)

- **GC-eos-agent-def-01** — *Keep root/planner/generator/reducer roles explicit.*
  `AgentRole` is a closed enum with all six variants including `Generator`;
  `executor` is a profile name with `role: generator`, never a role variant.
- **GC-eos-agent-def-02** — *`extra="forbid"` fidelity.* `AgentDefinition`
  carries `#[serde(deny_unknown_fields)]`; unknown frontmatter keys are rejected.
- **GC-eos-agent-def-03** — *No agent orchestrator above attempts.* This crate
  exposes only data + lookup; no scheduler, no run loop, no cross-agent
  coordination type.
- **GC-eos-agent-def-04** — *No tool visibility abstraction; registry is lookup
  only.* `allowed_tools`/`terminals` are `Vec<String>`; the `∪`-to-`ToolSpec`
  materialization is an eos-engine invariant documented in §8.1, not implemented
  here. Cross-registry unknown-name validation is performed by eos-runtime at
  startup once the tool registry exists.
- **GC-eos-agent-def-05** — *Break cyclic validation edges.* The recipe-catalog
  check relocates to `eos-workflow` (invoked at composition root); the skill lint
  takes terminal keys as injected `&[&str]`; frontmatter parsing is inlined (no
  `eos-config` edge). Only the pure recipe role-gating precheck stays here.
- **GC-eos-agent-def-06** — *`role` required on file-parse, default test-only.*
  Missing `role` in frontmatter yields `AgentDefError::MissingRole`; `role` is
  not `#[serde(default)]`.

## 11. Acceptance Criteria

TDD: write each test first, confirm it fails for the right reason, then
implement. Maps to anchor §11 "Tests to Port First"
(`test_registry_validation.py`, `test_submission_main_role_terminals.py`,
`test_routing_acceptance.py`).

- **AC-eos-agent-def-01** — Loading a profile `.md` with no `role:` frontmatter
  returns `AgentDefError::MissingRole`. *Test:* `loader_rejects_missing_role`
  (mirrors `loader.py:64`).
- **AC-eos-agent-def-02** — Frontmatter with an unknown key is rejected. *Test:*
  `definition_rejects_unknown_field` (mirrors `extra="forbid"`).
- **AC-eos-agent-def-03** — `terminals: []` (or all-blank) fails construction
  with `EmptyTerminals`; `tool_call_limit: 0` fails with
  `NonPositiveToolCallLimit`. *Test:* `definition_enforces_terminals_and_limit`.
- **AC-eos-agent-def-04** — A `_*.md` file is skipped; a `main/` profile gets
  `_main_role_contract.md` prepended to `system_prompt`. *Test:*
  `loader_skips_includes_and_prepends_contract` (mirrors `loader.py:25-63`).
- **AC-eos-agent-def-05** — A declared but missing `skill:` path yields
  `SkillNotFound`; an existing one resolves to an absolute path. *Test:*
  `loader_resolves_and_requires_skill`.
- **AC-eos-agent-def-06** — `dispatchable_subagent_names()` returns only
  `AgentType::Subagent` names, sorted. *Test:*
  `registry_lists_dispatchable_subagents` (mirrors `registry.py:34`,
  `test_routing_acceptance.py`).
- **AC-eos-agent-def-07** — A `context_recipe` on a role outside
  `{planner, generator, reducer}` yields `RecipeRoleMismatch`; a recipe on an
  in-scope role passes the precheck (catalog check is eos-workflow's). *Test:*
  `recipe_role_precheck` (mirrors `resolved_validation.py:42`,
  `test_registry_validation.py`).
- **AC-eos-agent-def-08** — `scan_skill_file` flags a body containing
  `submit_planner_outcome` and a body containing an injected terminal key, and
  returns empty for a terminal-silent body. *Test:* `skill_lint_detects_terminals`
  (mirrors `skills/loader.py`).
- **AC-eos-agent-def-09** — The `JsonSchema` for `AgentDefinition` matches the
  Phase-0 snapshot of the current Pydantic JSON schema on **field names and enum
  values**. The **required set diverges by exactly one field, `role`**, by
  design: Pydantic's `role = GENERATOR` default makes it optional in the Python
  schema, but §8.2/GC-06 make `role` required on the Rust file-parse path. The
  snapshot comparator carries an explicit `{role}` allowlist for this delta;
  field-name and enum-value parity must still hold exactly. *Test:*
  `agent_definition_schema_snapshot` (anchor §11 parity harness).
- **AC-eos-agent-def-10** — Loading the real `agents/profile/` tree (root,
  planner, executor, reducer, explorer, advisor) succeeds and the `executor`
  profile resolves to `role == AgentRole::Generator`. *Test:*
  `loads_bundled_profiles` (integration, `tests/loader_profiles.rs`).

## 12. Implementation Checklist

1. `error.rs`: `AgentDefError` enum → verify `cargo check` compiles the variants.
2. `model.rs`: `AgentType`, `AgentRole` (closed, snake_case), `AgentName` newtype
   → AC-06 enum-value test.
3. `model.rs`: `RawAgentDefinition` (serde DTO, `deny_unknown_fields`) +
   `AgentDefinition` + `from_frontmatter` invariants → AC-02, AC-03.
4. `loader.rs`: inline frontmatter split; `load_agents_dir`/`load_agents_tree`
   with skip/prepend/skill-resolution/defaults; required `role` → AC-01, AC-04,
   AC-05.
5. `registry.rs`: `AgentRegistryBuilder` + read-only `AgentRegistry`
   (`Arc`-stored) → AC-06.
6. `validation.rs`: recipe role-gating precheck → AC-07; `skill_lint` submodule
   with injected keys → AC-08.
7. `schemars` derive + schema-snapshot parity test → AC-09.
8. Integration test loading bundled profiles → AC-10.
9. `cargo fmt --check` + `clippy -D warnings`.

---
**On completion:** update the Progress Tracker in `./overview.md` for row
`eos-agent-def` per spec-conventions.md §13 (status + date + note + commit ref).
Do not edit other crates' rows. (Note: `overview.md` does not yet exist; create
the tracker row only when that shared file is established by the workspace/index
author — do not author the shared file from this crate's spec.)
