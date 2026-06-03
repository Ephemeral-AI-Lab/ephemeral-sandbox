# Crate `eos-agent-def` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-agent-def/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**8 types across 3 files.**

The `eos-agent-def` crate owns the static identity of an agent profile: the `AgentType` / `AgentRole` vocabularies, the `AgentName` newtype, and the `AgentDefinition` value type, together with the Markdown+frontmatter loader (`load_agents_dir` / `load_agents_tree`), the read-only `AgentRegistry` (built via `AgentRegistryBuilder`), and the pure fragments of profile validation (`check_context_recipe_role`, `skill_lint::scan_skill_file`). Construction follows parse-don't-validate: the `pub(crate)` serde DTO `RawAgentDefinition` deserializes the YAML frontmatter and funnels through `AgentDefinition::from_frontmatter`, so an invalid definition is unrepresentable (`NonZeroU32` limit, non-empty `terminals`). `AgentDefError` is the single typed error enum. It is a near-leaf crate depending only on `eos-types`, and is consumed by `eos-engine`, `eos-workflow`, and `eos-runtime`; it deliberately does not build `ToolSpec`s, resolve the `model: inherit` sentinel, own the `allowed_tools ∪ terminals` union policy, or run the `context_recipe` catalog check (those live downstream).

## Contents

- **`eos-agent-def/src/error.rs`** — `AgentDefError`
- **`eos-agent-def/src/model.rs`** — `AgentType`, `AgentRole`, `AgentName`, `RawAgentDefinition`, `AgentDefinition`
- **`eos-agent-def/src/registry.rs`** — `AgentRegistryBuilder`, `AgentRegistry`

---

## `eos-agent-def/src/error.rs`

#### `AgentDefError`  ·  _enum_  ·  derives: `Debug, thiserror::Error`  ·  #[non_exhaustive]  ·  [L15]

Failures raised when loading, parsing, or validating an agent profile.

**Variants**:
- `MissingRole { path: PathBuf }` — a profile `.md` omitted the required `role:` frontmatter field.
- `EmptyName` — a resolved agent `name` was empty after trimming whitespace.
- `EmptyTerminals` — `terminals` was empty (or all-blank); every agent must declare ≥1 terminal-capable tool.
- `NonPositiveToolCallLimit` — `tool_call_limit` was not strictly positive.
- `Read { path: PathBuf, cause: std::io::Error (#[source]) }` — the profile file could not be read from disk.
- `Frontmatter { path: PathBuf, cause: serde_yaml::Error (#[source]) }` — the YAML frontmatter failed to parse, or carried an unknown key.
- `SkillNotFound { path: PathBuf, declared: String, resolved: PathBuf }` — a declared `skill:` path did not resolve to an existing file.
- `RecipeRoleMismatch { agent: String, recipe: String, role: AgentRole }` — a `context_recipe` was declared by a role with no context builder.
- `SkillLint { violations: Vec<String> }` — one or more declared skill files violated the terminal-silence contract.

---

## `eos-agent-def/src/model.rs`

#### `AgentType`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  #[serde(rename_all = "snake_case")]  ·  [L23]

Runtime class of an agent profile (`model.py:AgentType`).

**Variants**: `Agent`, `Subagent`

**Trait impls**: `Default`

#### `AgentRole`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema`  ·  #[serde(rename_all = "snake_case")]  ·  [L45]

Canonical category of an agent profile; a closed vocabulary (deliberately not `#[non_exhaustive]`).

**Variants**: `Root`, `Planner`, `Generator`, `Reducer`, `Helper`, `Subagent`

**Trait impls**: `Display`

<details><summary>Methods (1)</summary>

`as_str`

</details>

#### `AgentName`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema`  ·  #[serde(transparent)]  ·  #[schemars(transparent)]  ·  [L93]

A registry key / dispatchable name, validated non-empty after trimming (format-only newtype).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `.0` | `String` |  |

**Trait impls**: `Display`

<details><summary>Methods (2)</summary>

`new`, `as_str`

</details>

#### `RawAgentDefinition`  ·  _struct_  ·  derives: `Debug, Default, Deserialize`  ·  #[serde(deny_unknown_fields)]  ·  pub(crate)  ·  [L131]

The serde DTO for the YAML frontmatter block (`extra="forbid"` → `deny_unknown_fields`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `Option<String>` | `pub` · `#[serde(default)]` |
| `description` | `Option<String>` | `pub` · `#[serde(default)]` |
| `system_prompt` | `Option<String>` | `pub` · `#[serde(default)]` |
| `model` | `Option<String>` | `pub` · `#[serde(default)]` |
| `tool_call_limit` | `u32` | `pub` · `#[serde(default)]` |
| `role` | `Option<AgentRole>` | `pub` · `#[serde(default)]` |
| `agent_type` | `AgentType` | `pub` · `#[serde(default)]` |
| `allowed_tools` | `Vec<String>` | `pub` · `#[serde(default)]` |
| `terminals` | `Vec<String>` | `pub` · `#[serde(default)]` |
| `notification_triggers` | `Vec<String>` | `pub` · `#[serde(default)]` |
| `skill` | `Option<PathBuf>` | `pub` · `#[serde(default)]` |
| `context_recipe` | `Option<String>` | `pub` · `#[serde(default)]` |

#### `AgentDefinition`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  #[serde(deny_unknown_fields)]  ·  [L167]

Full agent definition with all configuration fields; construction enforces invariants so an invalid value is unrepresentable.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `AgentName` | `pub` |
| `description` | `String` | `pub` |
| `system_prompt` | `Option<String>` | `pub` · `#[serde(default)]` |
| `model` | `Option<String>` | `pub` · `#[serde(default)]` |
| `tool_call_limit` | `NonZeroU32` | `pub` |
| `role` | `AgentRole` | `pub` |
| `agent_type` | `AgentType` | `pub` · `#[serde(default)]` |
| `allowed_tools` | `Vec<String>` | `pub` · `#[serde(default)]` |
| `terminals` | `Vec<String>` | `pub` |
| `notification_triggers` | `Vec<String>` | `pub` · `#[serde(default)]` |
| `skill` | `Option<PathBuf>` | `pub` · `#[serde(default)]` |
| `context_recipe` | `Option<String>` | `pub` · `#[serde(default)]` |

<details><summary>Methods (1)</summary>

`from_frontmatter`

</details>

---

## `eos-agent-def/src/registry.rs`

#### `AgentRegistryBuilder`  ·  _struct_  ·  derives: `Debug, Default`  ·  #[must_use]  ·  [L19]

Accumulates definitions, then finalizes an immutable `AgentRegistry`; `add` overwrites a same-named entry.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `definitions` | `HashMap<AgentName, Arc<AgentDefinition>>` |  |

<details><summary>Methods (3)</summary>

`new`, `add`, `build`

</details>

#### `AgentRegistry`  ·  _struct_  ·  derives: `Debug`  ·  [L56]

Immutable name → definition lookup, shared as `Arc<AgentRegistry>`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `definitions` | `HashMap<AgentName, Arc<AgentDefinition>>` |  |

**Trait impls**: `FromIterator<AgentDefinition>`

<details><summary>Methods (3)</summary>

`get`, `list`, `dispatchable_subagent_names`

</details>
