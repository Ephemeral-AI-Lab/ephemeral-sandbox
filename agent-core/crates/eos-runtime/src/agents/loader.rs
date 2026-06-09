//! Markdown+frontmatter profile loading (`agents/definition/loader.py`).
//!
//! The frontmatter split is inlined here (porting
//! `config/markdown.py:parse_markdown_frontmatter`) so this crate needs no
//! `eos-config` edge. `_*.md` files are private includes and skipped; a profile
//! directly under a `main/` directory gets `_main_role_contract.md` prepended to
//! its system prompt; `name`/`description` fall back to the file stem; a declared
//! `skill:` is resolved relative to the profile and must exist.

use std::fs;
use std::path::{Path, PathBuf};

use super::error::AgentDefError;
use super::model::{definition_from_frontmatter, AgentDefinition, RawAgentDefinition};

const MAIN_PROFILE_DIRNAME: &str = "main";
const MAIN_ROLE_CONTRACT_NAME: &str = "_main_role_contract.md";

/// Load agent definitions from `.md` files directly in `directory` (one level).
///
/// Returns an empty vec when `directory` is not a directory (parity with
/// `loader.py:load_agents_dir`).
///
/// # Errors
/// Propagates [`AgentDefError`] from any individual profile that fails to read,
/// parse, or validate.
#[cfg(test)]
fn load_agents_dir(directory: &Path) -> Result<Vec<AgentDefinition>, AgentDefError> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in read_dir(directory)? {
        let path = entry.path();
        if path.is_file() && is_markdown(&path) {
            paths.push(path);
        }
    }
    load_files(paths)
}

/// Load agent definitions from all `.md` files under `directory` (recursive).
///
/// Returns an empty vec when `directory` is not a directory (parity with
/// `loader.py:load_agents_tree`).
///
/// # Errors
/// Propagates [`AgentDefError`] from any individual profile that fails to read,
/// parse, or validate.
pub(crate) fn load_agents_tree(directory: &Path) -> Result<Vec<AgentDefinition>, AgentDefError> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    collect_markdown(directory, &mut paths)?;
    load_files(paths)
}

fn read_dir(directory: &Path) -> Result<Vec<fs::DirEntry>, AgentDefError> {
    let mut out = Vec::new();
    for entry in fs::read_dir(directory).map_err(|cause| AgentDefError::Read {
        path: directory.to_owned(),
        cause,
    })? {
        out.push(entry.map_err(|cause| AgentDefError::Read {
            path: directory.to_owned(),
            cause,
        })?);
    }
    Ok(out)
}

fn collect_markdown(directory: &Path, out: &mut Vec<PathBuf>) -> Result<(), AgentDefError> {
    for entry in read_dir(directory)? {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(&path, out)?;
        } else if is_markdown(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("md")
}

fn is_private_include(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('_'))
}

fn load_files(mut paths: Vec<PathBuf>) -> Result<Vec<AgentDefinition>, AgentDefError> {
    paths.sort();
    let mut definitions = Vec::new();
    for path in paths {
        // `_*.md` are private includes (e.g. `_main_role_contract.md`), not
        // standalone profiles.
        if is_private_include(&path) {
            continue;
        }
        definitions.push(load_one(&path)?);
    }
    Ok(definitions)
}

fn load_one(path: &Path) -> Result<AgentDefinition, AgentDefError> {
    let content = fs::read_to_string(path).map_err(|cause| AgentDefError::Read {
        path: path.to_owned(),
        cause,
    })?;
    let (frontmatter, body) = split_frontmatter(&content);
    // An empty / absent frontmatter block deserializes as the all-default DTO.
    let frontmatter = frontmatter.filter(|f| !f.trim().is_empty());
    let mut raw: RawAgentDefinition = serde_yaml::from_str(frontmatter.as_deref().unwrap_or("{}"))
        .map_err(|cause| AgentDefError::Frontmatter {
            path: path.to_owned(),
            cause,
        })?;

    // name defaults to the file stem when blank (`loader.py:54-55`).
    let name = raw
        .name
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| file_stem(path));
    // description defaults to `Agent: <name>` when blank (`loader.py:56`).
    let description = raw
        .description
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| format!("Agent: {name}"));
    raw.name = Some(name);
    raw.description = Some(description);

    // system_prompt = optional main-role contract + body (`loader.py:57-63`).
    let contract = main_role_contract_text(path);
    let body = (!body.is_empty()).then_some(body);
    raw.system_prompt = match (contract, body) {
        (Some(contract), Some(body)) => Some(format!("{contract}\n\n{body}")),
        (Some(contract), None) => Some(contract),
        (None, Some(body)) => Some(body),
        (None, None) => raw.system_prompt,
    };

    // skill: relative to the profile, made absolute, must exist (`loader.py:70-78`).
    if let Some(declared) = raw.skill.clone().filter(|p| !p.as_os_str().is_empty()) {
        let joined = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&declared);
        match fs::canonicalize(&joined) {
            Ok(resolved) if resolved.is_file() => raw.skill = Some(resolved),
            _ => {
                return Err(AgentDefError::SkillNotFound {
                    path: path.to_owned(),
                    declared: declared.to_string_lossy().into_owned(),
                    resolved: joined,
                });
            }
        }
    }

    definition_from_frontmatter(raw)
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Return the trimmed `_main_role_contract.md` body for an in-`main/` profile,
/// or `None` when out of scope (`loader.py:_main_role_contract_text`).
fn main_role_contract_text(profile_path: &Path) -> Option<String> {
    let in_main = profile_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some(MAIN_PROFILE_DIRNAME);
    if !in_main || is_private_include(profile_path) {
        return None;
    }
    let contract_path = profile_path.parent()?.join(MAIN_ROLE_CONTRACT_NAME);
    if !contract_path.is_file() {
        return None;
    }
    fs::read_to_string(&contract_path)
        .ok()
        .map(|s| s.trim_end().to_owned())
}

/// Split a Markdown document into its YAML frontmatter block and body text.
///
/// Inlined port of `config/markdown.py:parse_markdown_frontmatter`. Returns
/// `(None, full_content)` when there is no `---`-delimited frontmatter; otherwise
/// `(Some(frontmatter), stripped_body)`.
fn split_frontmatter(content: &str) -> (Option<String>, String) {
    let lines: Vec<&str> = content.lines().collect();
    if lines.first().map(|l| l.trim()) != Some("---") {
        return (None, content.to_owned());
    }
    let Some(end) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, line)| line.trim() == "---")
        .map(|(i, _)| i)
    else {
        return (None, content.to_owned());
    };
    let frontmatter = lines[1..end].join("\n");
    let body = lines[end + 1..].join("\n").trim().to_owned();
    (Some(frontmatter), body)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // unwrap is permitted in tests (err-no-unwrap-prod)
    use super::*;
    use eos_types::{AgentName, AgentRegistry, AgentType};

    /// A throwaway directory under the system temp dir, unique per test name and
    /// recreated empty. Removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("eos-agent-def-{name}"));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write(&self, rel: &str, body: &str) -> PathBuf {
            let path = self.0.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, body).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn loader_rejects_role_frontmatter() {
        let s = Scratch::new("role-frontmatter");
        s.write(
            "role.md",
            "---\nname: with_role\ndescription: d\ntool_call_limit: 5\nrole: generator\nterminals: [submit_x]\n---\nbody\n",
        );
        let err = load_agents_dir(&s.0).unwrap_err();
        assert!(matches!(err, AgentDefError::Frontmatter { .. }), "{err:?}");
    }

    // AC-eos-agent-def-04: `_*.md` is skipped; a main/ profile gets the contract
    // prepended to its system prompt.
    #[test]
    fn loader_skips_includes_and_prepends_contract() {
        let s = Scratch::new("main-contract");
        s.write("main/_main_role_contract.md", "CONTRACT TEXT\n");
        s.write(
            "main/worker.md",
            "---\nname: worker\ndescription: d\ntool_call_limit: 5\nterminals: [submit_x]\n---\nBODY TEXT\n",
        );
        let defs = load_agents_tree(&s.0).unwrap();
        // The `_main_role_contract.md` include is skipped — only `worker` loads.
        assert_eq!(defs.len(), 1);
        let worker = &defs[0];
        assert_eq!(worker.name.as_str(), "worker");
        assert_eq!(
            worker.system_prompt.as_deref(),
            Some("CONTRACT TEXT\n\nBODY TEXT")
        );
    }

    #[test]
    fn loader_defaults_name_and_description_from_stem() {
        let s = Scratch::new("defaults");
        // No name/description in frontmatter -> stem + "Agent: <stem>".
        s.write(
            "subagent.md",
            "---\ntool_call_limit: 5\nagent_type: subagent\nterminals: [submit_x]\n---\nbody\n",
        );
        let defs = load_agents_dir(&s.0).unwrap();
        assert_eq!(defs[0].name.as_str(), "subagent");
        assert_eq!(defs[0].description, "Agent: subagent");
    }

    // AC-eos-agent-def-05: a missing skill path errors; an existing one resolves
    // to an absolute path.
    #[test]
    fn loader_resolves_and_requires_skill() {
        let s = Scratch::new("skill");
        s.write(
            "agent.md",
            "---\nname: a\ndescription: d\ntool_call_limit: 5\nterminals: [submit_x]\nskill: ./missing/SKILL.md\n---\nbody\n",
        );
        let err = load_agents_dir(&s.0).unwrap_err();
        assert!(
            matches!(err, AgentDefError::SkillNotFound { .. }),
            "{err:?}"
        );

        // Now create the skill file and confirm it resolves absolute.
        s.write("skills/SKILL.md", "# skill\n");
        s.write(
            "agent.md",
            "---\nname: a\ndescription: d\ntool_call_limit: 5\nterminals: [submit_x]\nskill: ./skills/SKILL.md\n---\nbody\n",
        );
        let defs = load_agents_dir(&s.0).unwrap();
        let skill = defs[0].skill.as_ref().unwrap();
        assert!(
            skill.is_absolute(),
            "resolved skill must be absolute: {skill:?}"
        );
        assert!(skill.ends_with("SKILL.md"));
    }

    #[test]
    fn non_directory_yields_empty() {
        let missing = Path::new("/no/such/agent/profile/dir");
        assert!(load_agents_dir(missing).unwrap().is_empty());
        assert!(load_agents_tree(missing).unwrap().is_empty());
    }

    #[test]
    fn split_frontmatter_handles_absent_and_present() {
        let (fm, body) = split_frontmatter("no frontmatter here");
        assert!(fm.is_none());
        assert_eq!(body, "no frontmatter here");

        let (fm, body) = split_frontmatter("---\nagent_type: agent\n---\nthe body\n");
        assert_eq!(fm.as_deref(), Some("agent_type: agent"));
        assert_eq!(body, "the body");
    }

    #[test]
    fn non_main_profile_uses_body_only() {
        let s = Scratch::new("non-main");
        s.write(
            "helper/advisor.md",
            "---\nname: advisor\ndescription: d\ntool_call_limit: 5\nterminals: [submit_x]\n---\nADVISOR BODY\n",
        );
        let defs = load_agents_tree(&s.0).unwrap();
        assert_eq!(defs[0].system_prompt.as_deref(), Some("ADVISOR BODY"));
    }

    fn bundled_profile_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.eos-agents/profile")
    }

    #[test]
    fn loads_bundled_profiles() {
        let dir = bundled_profile_dir();
        assert!(
            dir.is_dir(),
            "bundled profile tree not found at {}",
            dir.display()
        );

        let definitions = load_agents_tree(&dir).expect("load bundled profiles");
        let registry: AgentRegistry = definitions.into_iter().collect();

        for name in [
            "root", "planner", "executor", "reducer", "advisor", "subagent",
        ] {
            let key = AgentName::new(name).expect("non-empty name");
            assert!(
                registry.get(&key).is_some(),
                "missing bundled profile {name}"
            );
        }

        let executor = registry
            .get(&AgentName::new("executor").unwrap())
            .expect("executor present");
        assert_eq!(executor.agent_type, AgentType::Agent);
        let skill = executor.skill.as_ref().expect("executor skill resolved");
        assert!(skill.is_absolute());

        let advisor = registry
            .get(&AgentName::new("advisor").unwrap())
            .expect("advisor present");
        assert_eq!(advisor.agent_type, AgentType::Advisor);

        let dispatchable: Vec<String> = registry
            .dispatchable_subagent_names()
            .iter()
            .map(|n| n.as_str().to_owned())
            .collect();
        assert_eq!(dispatchable, vec!["subagent".to_owned()]);

        let root = registry.get(&AgentName::new("root").unwrap()).unwrap();
        assert!(
            root.system_prompt
                .as_deref()
                .is_some_and(|p| p.contains("Main-Agent Operating Contract")),
            "root should have the main-role contract prepended"
        );
    }
}
