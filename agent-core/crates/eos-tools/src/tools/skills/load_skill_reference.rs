//! The `load_skill_reference` tool — serves one named `references/*.md` document
//! from the bound agent's own skill. The skill *content* comes from the shared
//! [`SkillRegistry`](eos_skills::SkillRegistry) captured by this executor; the
//! per-agent **allowlist** ([`CallerScope::skill_slug`](super::CallerScope),
//! baked in at registration) scopes which skill the caller may read: an agent
//! reads only its own skill's references, and a not-found error lists only that
//! skill.

use std::sync::Arc;

use async_trait::async_trait;
use eos_skills::{ReferenceName, SkillName};
use eos_types::JsonObject;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::CallerScope;
use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::{OutputShape, ToolResult};
use crate::registry::config::ToolConfigSet;
use crate::registry::spec::text_spec;
use crate::registry::ToolRegistry;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;
use crate::tools::SkillToolService;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct LoadSkillReferenceInput {
    /// Name of the skill that owns the reference.
    skill_name: String,
    /// Exact reference document name to load.
    reference_name: String,
}

/// The `load_skill_reference` executor, scoped to the caller's own skill(s). The
/// `allowed` list is built from the bound agent's declared skill at
/// registration; empty ⇒ a no-op tool that errors on every call.
struct LoadSkillReference {
    allowed: Vec<SkillName>,
    service: SkillToolService,
}

#[async_trait]
impl ToolExecutor for LoadSkillReference {
    async fn execute(
        &self,
        input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: LoadSkillReferenceInput = match parse_input(ToolName::LoadSkillReference, input)
        {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        // Only the agent's own declared skill(s), resolved against the shared
        // registry, are readable.
        let available: Vec<String> = self
            .allowed
            .iter()
            .filter_map(|slug| self.service.skill_registry.get(slug))
            .map(|s| s.name.as_str().to_owned())
            .collect();

        if !available.iter().any(|name| name == &parsed.skill_name) {
            return Ok(ToolResult::error(
                json!({
                    "error": format!("Skill '{}' not found.", parsed.skill_name),
                    "available": available,
                })
                .to_string(),
            ));
        }

        let skill = SkillName::parse(parsed.skill_name.clone())
            .ok()
            .and_then(|name| self.service.skill_registry.get(&name));
        let Some(skill) = skill else {
            return Ok(ToolResult::error(format!(
                "Skill '{}' not found in registry.",
                parsed.skill_name
            )));
        };

        let content = ReferenceName::parse(parsed.reference_name.clone())
            .ok()
            .and_then(|reference| skill.references.get(&reference));
        match content {
            Some(content) => Ok(ToolResult::ok(content.clone())),
            None => {
                let available_references: Vec<String> = skill
                    .references
                    .keys()
                    .map(|r| r.as_str().to_owned())
                    .collect();
                Ok(ToolResult::error(
                    json!({
                        "error": format!(
                            "Reference '{}' not found in skill '{}'.",
                            parsed.reference_name, parsed.skill_name
                        ),
                        "available_references": available_references,
                    })
                    .to_string(),
                ))
            }
        }
    }
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    caller: &CallerScope,
    skill_service: SkillToolService,
) {
    // Scope to the caller's own skill folder slug (0-or-1 entries).
    let allowed: Vec<SkillName> = caller
        .skill_slug
        .as_deref()
        .and_then(|slug| SkillName::parse(slug.to_owned()).ok())
        .into_iter()
        .collect();
    let cfg = config.get(ToolName::LoadSkillReference);
    super::super::register_tool(
        registry,
        ToolName::LoadSkillReference,
        cfg,
        text_spec(
            ToolName::LoadSkillReference,
            &cfg.description,
            schema_for!(LoadSkillReferenceInput),
        ),
        OutputShape::Text,
        Arc::new(LoadSkillReference {
            allowed,
            service: skill_service,
        }),
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // unwrap permitted in tests (err-no-unwrap-prod)

    use std::fs;
    use std::path::{Path, PathBuf};

    use eos_skills::SkillRegistry;
    use serde_json::Value;

    use super::*;
    use crate::support::metadata;

    /// Throwaway skill root under the temp dir, removed on drop (the loader is
    /// filesystem-backed and `SkillDefinition` is `#[non_exhaustive]`, so a real
    /// registry must come from disk). Mirrors the eos-skills test scratch.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("eos-tools-{name}-{}-{nonce}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write(&self, rel: &str, body: &str) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A two-skill registry: `a` (reference `ref_a`) and `b` (reference `secret`).
    fn two_skill_registry(scratch: &Scratch) -> SkillRegistry {
        scratch.write(
            "a/SKILL.md",
            "---\nname: a\ndescription: skill a\n---\nbody a\n",
        );
        scratch.write("a/references/ref_a.md", "REF A CONTENT");
        scratch.write(
            "b/SKILL.md",
            "---\nname: b\ndescription: skill b\n---\nbody b\n",
        );
        scratch.write("b/references/secret.md", "SECRET B CONTENT");
        SkillRegistry::load_from_dir(scratch.path()).unwrap()
    }

    fn service_with(registry: SkillRegistry) -> SkillToolService {
        SkillToolService::new(Arc::new(registry))
    }

    fn input(skill: &str, reference: &str) -> JsonObject {
        let mut m = JsonObject::new();
        m.insert("skill_name".to_owned(), Value::String(skill.to_owned()));
        m.insert(
            "reference_name".to_owned(),
            Value::String(reference.to_owned()),
        );
        m
    }

    fn scoped_to(slug: &str, service: SkillToolService) -> LoadSkillReference {
        LoadSkillReference {
            allowed: vec![SkillName::parse(slug.to_owned()).unwrap()],
            service,
        }
    }

    // D7: an agent scoped to skill `a` cannot read skill `b`'s references; the
    // content never leaks and the not-found error lists only `a` (never `b`,
    // never the whole registry).
    #[tokio::test]
    async fn scoped_agent_cannot_read_other_skill() {
        let scratch = Scratch::new("d7-isolation");
        let ctx = metadata();
        let res = scoped_to("a", service_with(two_skill_registry(&scratch)))
            .execute(&input("b", "secret"), &ctx)
            .await
            .unwrap();

        assert!(res.is_error, "reading another skill is denied");
        assert!(
            !res.output.contains("SECRET B CONTENT"),
            "content never leaks: {}",
            res.output
        );
        let body: Value = serde_json::from_str(&res.output).unwrap();
        assert_eq!(
            body["available"],
            json!(["a"]),
            "error lists only the agent's own skill, not all bundled skills"
        );
    }

    // D7: the agent CAN read its own skill's reference.
    #[tokio::test]
    async fn scoped_agent_reads_own_reference() {
        let scratch = Scratch::new("d7-own");
        let ctx = metadata();
        let res = scoped_to("a", service_with(two_skill_registry(&scratch)))
            .execute(&input("a", "ref_a"), &ctx)
            .await
            .unwrap();

        assert!(!res.is_error, "own reference is served: {}", res.output);
        assert_eq!(res.output, "REF A CONTENT");
    }

    // D7: a skill-less agent (empty allowlist) is a no-op tool — every call errors
    // with an empty `available` (Python `allowed_slugs=[]`).
    #[tokio::test]
    async fn skill_less_agent_has_empty_allowlist() {
        let scratch = Scratch::new("d7-noskill");
        let ctx = metadata();
        let res = LoadSkillReference {
            allowed: vec![],
            service: service_with(two_skill_registry(&scratch)),
        }
        .execute(&input("a", "ref_a"), &ctx)
        .await
        .unwrap();

        assert!(res.is_error);
        let body: Value = serde_json::from_str(&res.output).unwrap();
        assert_eq!(body["available"], json!([]), "no skill in scope");
    }

    // D7 wiring: `register` builds the allowlist from `CallerScope::skill_slug`
    // and sources its config (intent/terminal/hooks/description) from the set.
    #[test]
    fn register_scopes_to_caller_skill_slug() {
        let mut registry = ToolRegistry::new();
        register(
            &mut registry,
            &crate::tools::repo_tools_config(),
            &CallerScope {
                dispatchable_subagents: vec![],
                skill_slug: Some("a".to_owned()),
            },
            SkillToolService::new(Arc::new(SkillRegistry::new())),
        );
        assert!(
            registry.get(ToolName::LoadSkillReference).is_some(),
            "load_skill_reference is registered for a skill-bound caller"
        );
    }
}
