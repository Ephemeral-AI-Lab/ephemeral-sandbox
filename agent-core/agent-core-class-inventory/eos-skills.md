# Crate `eos-skills` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-skills/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**6 types across 3 files.**

The `eos-skills` crate owns the runtime skill content exposed to agents: the
immutable `SkillDefinition` value type, the name-keyed `SkillRegistry` lookup,
and a deterministic, config-rooted loader that reads directory-based skills
(`<skill-name>/SKILL.md` plus an optional `references/*.md` set) from a single
configured skill root. Names and reference keys are lifted into validated
newtypes (`SkillName`, `ReferenceName`) whose separator/`..`/NUL rejection is
defense-in-depth, and the provenance string is lifted to the `SkillSource` enum;
load failures surface through the single `SkillLoadError` enum. As a near-leaf
crate it depends only on `eos-config` (for the shared
`parse_markdown_frontmatter` helper) plus `serde`/`serde_yaml` and `thiserror`;
it is consumed by `eos-runtime` (which calls `load_skill_registry` at the
composition root) and `eos-tools` (which reads `references` content in-memory via
the registry to serve the `load_skill_reference` tool). It deliberately does not
own that tool's spec/executor, agent-to-skill binding, the launch-time skill
message, or any runtime watch/reload.

## Contents

- **`eos-skills/src/definition.rs`** — `SkillName`, `ReferenceName`, `SkillSource`, `SkillDefinition`
- **`eos-skills/src/error.rs`** — `SkillLoadError`
- **`eos-skills/src/registry.rs`** — `SkillRegistry`

---

## `eos-skills/src/definition.rs`

#### `SkillName`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize`  ·  #[serde(transparent)]  ·  [L30]

A validated skill name — the parsed name (frontmatter `name`, else the directory name) and the registry key.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `.0` | `String` |  |

<details><summary>Methods (2)</summary>

`parse`, `as_str`

</details>

#### `ReferenceName`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize`  ·  #[serde(transparent)]  ·  [L60]

A validated reference name — a skill's `references/*.md` file stem and its map key; accepts dotted stems like `api.v2`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `.0` | `String` |  |

<details><summary>Methods (2)</summary>

`parse`, `as_str`

</details>

#### `SkillSource`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize`  ·  #[serde(rename_all = "snake_case")] · #[non_exhaustive]  ·  [L88]

Where a skill was loaded from; replaces the Python free `source: str`.

**Variants**: `Bundled`

#### `SkillDefinition`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize`  ·  #[non_exhaustive]  ·  [L100]

A loaded skill — the immutable runtime content exposed to agents.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `SkillName` | `pub` |
| `description` | `String` | `pub` |
| `content` | `String` | `pub` |
| `source` | `SkillSource` | `pub` |
| `path` | `Option<PathBuf>` | `pub` |
| `references` | `BTreeMap<ReferenceName, String>` | `pub` |

---

## `eos-skills/src/error.rs`

#### `SkillLoadError`  ·  _enum_  ·  derives: `Debug, thiserror::Error`  ·  #[non_exhaustive]  ·  [L12]

Failures raised while loading skills from the configured skill root; deliberately has no malformed-frontmatter variant.

**Variants**:
- `RootNotDir(PathBuf)` — the skill root exists but is not a directory.
- `ReadDir { path: PathBuf, #[source] cause: std::io::Error }` — listing a directory failed.
- `ReadFile { path: PathBuf, #[source] cause: std::io::Error }` — reading a `SKILL.md` or `references/*.md` file failed.
- `InvalidName(String)` — a parsed skill or reference name was empty or carried a path component.

---

## `eos-skills/src/registry.rs`

#### `SkillRegistry`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Default`  ·  [L15]

An immutable, name-keyed skill lookup over a `BTreeMap`; built once at the composition root and shared as `Arc<SkillRegistry>`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `skills` | `BTreeMap<SkillName, SkillDefinition>` | `pub(crate)` |

<details><summary>Methods (5)</summary>

`new`, `register`, `get`, `list_skills`, `load_from_dir`

</details>
