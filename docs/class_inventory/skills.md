# Module `skills` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/skills/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**2 classes across 2 files.**

The skills module owns the loading and in-memory cataloging of agent "skills" — directory-based instruction bundles (a SKILL.md file with optional YAML frontmatter plus an optional references/ folder) that live under backend/config/skills. Its two core data/storage classes are the frozen SkillDefinition dataclass (name, description, markdown content, source, path, and a name-to-content references map) and SkillRegistry, a name-keyed store with register/get/list_skills accessors. The loader layer (core/loader.load_skill_registry plus bundled/get_bundled_skills) walks the config directory, parses each skill's frontmatter and reference files into SkillDefinitions, and populates a fresh registry, while the package __init__ exposes these via lazy attribute resolution.

## Contents

- **`skills/core/registry.py`** — `SkillRegistry`
- **`skills/core/types.py`** — `SkillDefinition`

---

## `skills/core/registry.py`

#### `SkillRegistry`  ·  _class_  ·  [L8]

Store loaded skills by name.

**Instance attributes**: `_skills`

<details><summary>Methods (4)</summary>

`__init__`, `register`, `get`, `list_skills`

</details>

---

## `skills/core/types.py`

#### `SkillDefinition`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L9]

A loaded skill.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `description` | `str` |  |
| `content` | `str` |  |
| `source` | `str` |  |
| `path` | `str \| None` | `None` |
| `references` | `dict[str, str]` | `field(default_factory=dict)` |

