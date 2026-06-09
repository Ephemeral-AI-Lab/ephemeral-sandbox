use workspace_guard::{
    public_declared_identifiers, read_to_string, relative_to, rust_files_under, SourceFile,
    Workspace,
};

const CANONICAL_SERVICE_REPLACEMENTS: &[&str] =
    &["Runtime", "Handles", "Context", "Client", "Records"];

#[derive(Debug)]
struct ServiceCandidate {
    crate_name: String,
    relative_path: String,
    symbol: String,
}

#[test]
fn final_service_named_surfaces_have_sibling_consumers() {
    let workspace = Workspace::load();
    let files = workspace.source_files();
    let consumer_files = consumer_files(&workspace, &files);
    let candidates = service_candidates(&files);
    let violations = candidates
        .iter()
        .filter(|candidate| !has_sibling_reference(candidate, &consumer_files))
        .map(|candidate| {
            format!(
                "{}: `{}` uses service vocabulary without a sibling-crate consumer; suggested replacement: {}",
                candidate.relative_path,
                candidate.symbol,
                suggested_replacement(candidate)
            )
        })
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "service_boundaries rule violated:\n{}",
        violations.join("\n")
    );
}

#[test]
fn service_boundary_scanner_finds_current_candidates() {
    let workspace = Workspace::load();
    let candidates = service_candidates(&workspace.source_files());
    assert!(
        !candidates.is_empty(),
        "service_boundaries rule violated: scanner found no service candidates to evaluate"
    );
}

fn service_candidates(files: &[workspace_guard::SourceFile]) -> Vec<ServiceCandidate> {
    let mut candidates = Vec::new();
    for file in files {
        let service_path = file.relative_path.ends_with("/service.rs")
            || file.relative_path.ends_with("/services.rs")
            || file.relative_path.contains("/services/");
        let public_symbols = public_declared_identifiers(&file.text);
        let has_service_symbol = public_symbols
            .iter()
            .any(|symbol| symbol.ends_with("Service") || symbol.ends_with("Services"));
        for symbol in public_symbols {
            if symbol.ends_with("Service") || symbol.ends_with("Services") {
                candidates.push(ServiceCandidate {
                    crate_name: file.crate_name.clone(),
                    relative_path: file.relative_path.clone(),
                    symbol,
                });
            }
        }
        if service_path && !has_service_symbol {
            candidates.push(ServiceCandidate {
                crate_name: file.crate_name.clone(),
                relative_path: file.relative_path.clone(),
                symbol: "service module".to_owned(),
            });
        }
    }
    candidates
}

fn consumer_files(workspace: &Workspace, files: &[SourceFile]) -> Vec<SourceFile> {
    let mut consumers = files.to_vec();
    let Some(repo_root) = workspace.root().parent() else {
        return consumers;
    };
    let backend_src = repo_root.join("backend-server").join("crates");
    for path in rust_files_under(&backend_src) {
        consumers.push(SourceFile {
            crate_name: "backend-server".to_owned(),
            relative_path: relative_to(&path, repo_root),
            text: read_to_string(&path),
            path,
        });
    }
    consumers
}

fn has_sibling_reference(candidate: &ServiceCandidate, files: &[SourceFile]) -> bool {
    if candidate.symbol == "service module" {
        return false;
    }
    files
        .iter()
        .filter(|file| file.crate_name != candidate.crate_name)
        .any(|file| file.text.contains(&candidate.symbol))
}

fn suggested_replacement(candidate: &ServiceCandidate) -> &'static str {
    let path = candidate.relative_path.as_str();
    let symbol = candidate.symbol.as_str();
    if path.contains("record") {
        "Records"
    } else if path.contains("client") || symbol.contains("Provider") {
        "Client"
    } else if path.contains("context") || path.contains("request") {
        "Context"
    } else if path.contains("runtime") || path.contains("tool") {
        "Runtime"
    } else {
        "Handles"
    }
}

#[test]
fn service_replacement_suggestions_stay_canonical() {
    let candidates = [
        ServiceCandidate {
            crate_name: "eos-agent-run".to_owned(),
            relative_path: "crates/eos-agent-run/src/records.rs".to_owned(),
            symbol: "MessageRecordService".to_owned(),
        },
        ServiceCandidate {
            crate_name: "eos-llm-client".to_owned(),
            relative_path: "crates/eos-llm-client/src/client.rs".to_owned(),
            symbol: "ProviderService".to_owned(),
        },
        ServiceCandidate {
            crate_name: "eos-workflow".to_owned(),
            relative_path: "crates/eos-workflow/src/context.rs".to_owned(),
            symbol: "ContextService".to_owned(),
        },
        ServiceCandidate {
            crate_name: "eos-tool".to_owned(),
            relative_path: "crates/eos-tool/src/tools.rs".to_owned(),
            symbol: "ToolService".to_owned(),
        },
        ServiceCandidate {
            crate_name: "eos-agent-core-server".to_owned(),
            relative_path: "crates/eos-agent-core-server/src/service.rs".to_owned(),
            symbol: "AgentCoreService".to_owned(),
        },
    ];

    let violations = candidates
        .iter()
        .map(suggested_replacement)
        .filter(|replacement| !CANONICAL_SERVICE_REPLACEMENTS.contains(replacement))
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "service_boundaries rule violated: non-canonical replacement suggestions: {violations:?}"
    );
}

#[test]
fn same_crate_references_do_not_satisfy_service_rule() {
    let workspace = Workspace::load();
    let file = workspace
        .source_files()
        .into_iter()
        .find(|source| source.relative_path.ends_with("/service.rs"))
        .unwrap_or_else(|| panic!("expected at least one current service.rs candidate"));
    let candidate = ServiceCandidate {
        crate_name: file.crate_name.clone(),
        relative_path: file.relative_path.clone(),
        symbol: public_declared_identifiers(&file.text)
            .into_iter()
            .find(|symbol| symbol.ends_with("Service") || symbol.ends_with("Services"))
            .unwrap_or_else(|| "service module".to_owned()),
    };
    let same_crate_file = workspace_guard::SourceFile {
        crate_name: candidate.crate_name.clone(),
        path: file.path.clone(),
        relative_path: file.relative_path.clone(),
        text: format!("use crate::service::{};", candidate.symbol),
    };

    assert!(
        !has_sibling_reference(&candidate, &[same_crate_file]),
        "service_boundaries rule violated: same-crate references must not satisfy sibling-use guard"
    );
}
