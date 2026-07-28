use std::collections::BTreeSet;

use super::stale::{compact_rust_code, rust_syntax_tokens};
use super::{StaleFacts, TrackedSource};

const PROVIDER_MANAGER_API: &[&str] = &[
    "CreateSandboxRequest",
    "CreateSandboxResult",
    "ManagerError",
    "ProgressSink",
    "SandboxDaemonEndpoint",
    "SandboxDaemonInstaller",
    "SandboxHttpEndpoint",
    "SandboxId",
    "SandboxRecord",
    "SandboxResourceMetrics",
    "SandboxResourceProfile",
    "SandboxRuntime",
    "SandboxState",
    "SharedBaseMount",
    "StartedDaemon",
];

const OBSERVABILITY_LAYERSTACK_API: &[&str] = &[
    "LayerDeltaDescription",
    "LayerDeltaEntryKind",
    "LayerRef",
    "service::StackObservation",
];

const STD_NET_ADDRESS_API: &[&str] = &[
    "net::IpAddr",
    "net::Ipv4Addr",
    "net::Ipv6Addr",
    "net::SocketAddr",
    "net::SocketAddrV4",
    "net::SocketAddrV6",
];

pub(super) fn validate(facts: &StaleFacts) -> Vec<String> {
    let mut violations = Vec::new();
    validate_protocol_ownership(facts, &mut violations);
    validate_manager_boundary(facts, &mut violations);
    validate_provider_boundary(facts, &mut violations);
    validate_observability_boundary(facts, &mut violations);
    violations
}

fn validate_protocol_ownership(facts: &StaleFacts, violations: &mut Vec<String>) {
    for file in facts
        .files
        .iter()
        .filter(|file| file.path.ends_with("Cargo.toml") || is_rust_source(file))
    {
        let rust_content = is_rust_source(file).then(|| compact_rust_code(&file.content));
        let content = rust_content.as_deref().unwrap_or(&file.content);
        let product = ["crates/sandbox-cli/", "crates/sandbox-mcp/"]
            .iter()
            .any(|root| file.path.starts_with(root));
        if product && (content.contains("sandbox-protocol") || content.contains("sandbox_protocol"))
        {
            violations.push(format!(
                "product adapter imports protocol directly in {}",
                file.path
            ));
        }
        if file.path.starts_with("crates/sandbox-provider-docker/")
            && file.path != "crates/sandbox-provider-docker/src/readiness.rs"
            && is_rust_source(file)
            && content.contains("sandbox_protocol")
        {
            violations.push(format!(
                "provider protocol usage escaped readiness.rs into {}",
                file.path
            ));
        }
    }

    let readiness = facts
        .files
        .iter()
        .find(|file| file.path == "crates/sandbox-provider-docker/src/readiness.rs")
        .map(|file| file.content.as_str())
        .unwrap_or_default();
    let roots = crate_roots(readiness, "sandbox_protocol");
    let references = roots
        .iter()
        .flat_map(|root| crate_references(readiness, root))
        .filter(|reference| reference != "self")
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from(["daemon_readiness_request_line".to_owned()]);
    if references != expected || forbidden_root_import(readiness, &roots) {
        violations.push(format!(
            "provider readiness protocol API set is {references:?}; expected {expected:?}"
        ));
    }
}

fn validate_manager_boundary(facts: &StaleFacts, violations: &mut Vec<String>) {
    for file in facts
        .files
        .iter()
        .filter(|file| file.path.starts_with("crates/sandbox-manager/") && is_rust_source(file))
    {
        let compact = compact_rust_code(&file.content);
        for forbidden in [
            "ProtocolLimits",
            "TcpSandboxDaemonClient",
            "LocalSandboxDaemonInstaller",
            "std::process::Command",
            "tokio::process",
            "tokio::net",
        ] {
            if compact.contains(forbidden) {
                violations.push(format!(
                    "manager owns forbidden adapter primitive {forbidden} in {}",
                    file.path
                ));
            }
        }
        let process_references = compact.matches("std::process").count();
        let allowed_ids = compact.matches("std::process::id()").count();
        let std_adapter = adapter_references(&file.content, "std", true);
        let tokio_adapter = adapter_references(&file.content, "tokio", false);
        let external_adapter = ["rustix", "nix", "libc", "socket2", "mio", "async_std"]
            .iter()
            .any(|root| external_adapter_references(&file.content, root));
        if process_references != allowed_ids || std_adapter || tokio_adapter || external_adapter {
            violations.push(format!(
                "manager owns forbidden process API in {}",
                file.path
            ));
        }
    }
}

fn validate_provider_boundary(facts: &StaleFacts, violations: &mut Vec<String>) {
    for file in facts.files.iter().filter(|file| {
        file.path.starts_with("crates/sandbox-provider-docker/") && is_rust_source(file)
    }) {
        let roots = crate_roots(&file.content, "sandbox_manager");
        for root in &roots {
            for reference in crate_references(&file.content, root) {
                if reference == "self" {
                    continue;
                }
                if !allowed_reference(&reference, PROVIDER_MANAGER_API) {
                    violations.push(format!(
                        "provider imports forbidden manager API {reference} in {}",
                        file.path
                    ));
                }
            }
        }
        if forbidden_root_import(&file.content, &roots) {
            violations.push(format!(
                "provider imports sandbox_manager through a root, glob, or alias in {}",
                file.path
            ));
        }
    }
}

fn validate_observability_boundary(facts: &StaleFacts, violations: &mut Vec<String>) {
    for file in facts.files.iter().filter(|file| {
        (file
            .path
            .starts_with("crates/sandbox-observability/query/src/")
            || file.path == "crates/sandbox-observability/query/build.rs")
            && is_rust_source(file)
    }) {
        let roots = crate_roots(&file.content, "sandbox_runtime_layerstack");
        for root in &roots {
            for reference in crate_references(&file.content, root) {
                if reference == "self" {
                    continue;
                }
                if !allowed_reference(&reference, OBSERVABILITY_LAYERSTACK_API) {
                    violations.push(format!(
                        "observability application imports forbidden layerstack API {reference} in {}",
                        file.path
                    ));
                }
            }
        }
        if forbidden_root_import(&file.content, &roots) {
            violations.push(format!(
                "observability application imports sandbox_runtime_layerstack through a root, glob, or alias in {}",
                file.path
            ));
        }
    }
}

fn crate_references(source: &str, crate_name: &str) -> BTreeSet<String> {
    let source = rust_syntax_tokens(source).join(" ");
    let mut references = BTreeSet::new();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find(crate_name) {
        let start = cursor + offset;
        let end = start + crate_name.len();
        cursor = end;
        if source[..start]
            .chars()
            .next_back()
            .is_some_and(identifier_character)
            || source[end..]
                .chars()
                .next()
                .is_some_and(identifier_character)
        {
            continue;
        }
        let rest = source[end..].trim_start();
        let Some(tree) = rest.strip_prefix("::") else {
            continue;
        };
        expand_reference_tree(tree.trim_start(), "", &mut references);
    }
    references
}

fn crate_root_imported(source: &str, crate_name: &str) -> bool {
    let compact = compact_rust_code(source);
    let markers = [
        format!("use{crate_name}"),
        format!("use::{crate_name}"),
        format!("use{{{crate_name}"),
        format!("use{{::{crate_name}"),
        format!(",{crate_name}"),
        format!(",::{crate_name}"),
    ];
    let extern_marker = format!("externcrate{crate_name}");
    compact
        .match_indices(&extern_marker)
        .any(|(start, _)| compact[start + extern_marker.len()..].starts_with(';'))
        || markers.iter().any(|marker| {
            compact.match_indices(marker).any(|(start, _)| {
                let suffix = &compact[start + marker.len()..];
                suffix.starts_with(';')
                    || suffix.starts_with('}')
                    || suffix.starts_with(',')
                    || suffix.starts_with("::*")
            })
        })
}

fn forbidden_root_import(source: &str, roots: &BTreeSet<String>) -> bool {
    roots
        .iter()
        .any(|root| crate_root_imported(source, root) || crate_root_reexported(source, root))
}

fn crate_root_reexported(source: &str, crate_name: &str) -> bool {
    rust_syntax_tokens(source)
        .split(|token| token == ";")
        .filter_map(|statement| {
            statement
                .windows(2)
                .position(|tokens| tokens == ["pub", "use"])
                .map(|start| statement[start + 2..].concat())
        })
        .any(|body| use_body_reexports_root(&body, crate_name))
}

fn use_body_reexports_root(body: &str, crate_name: &str) -> bool {
    let body = body.strip_prefix("::").unwrap_or(body);
    if let Some(group) = body
        .strip_prefix('{')
        .and_then(|body| body.strip_suffix('}'))
    {
        return split_reference_group(group)
            .into_iter()
            .any(|item| use_item_reexports_root(item, crate_name));
    }
    use_item_reexports_root(body, crate_name)
}

fn use_item_reexports_root(item: &str, crate_name: &str) -> bool {
    let item = item.strip_prefix("::").unwrap_or(item);
    let Some(suffix) = item.strip_prefix(crate_name) else {
        return false;
    };
    if let Some(alias) = suffix.strip_prefix("as") {
        return !alias.is_empty() && alias.chars().all(identifier_character);
    }
    if suffix.chars().next().is_some_and(identifier_character) {
        return false;
    }
    let Some(group) = suffix
        .strip_prefix("::{")
        .and_then(|group| group.strip_suffix('}'))
    else {
        return false;
    };
    split_reference_group(group).into_iter().any(|item| {
        item == "self"
            || item
                .strip_prefix("selfas")
                .is_some_and(|alias| !alias.is_empty() && alias.chars().all(identifier_character))
    })
}

fn allowed_reference(reference: &str, allowed: &[&str]) -> bool {
    allowed.iter().any(|allowed| {
        reference == *allowed
            || reference
                .strip_prefix(allowed)
                .is_some_and(|suffix| suffix.starts_with("::"))
    })
}

fn expand_reference_tree(source: &str, prefix: &str, references: &mut BTreeSet<String>) {
    let source = source.trim_start();
    if let Some(group) = source.strip_prefix('{') {
        let Some(close) = matching_brace(group) else {
            references.insert("<unterminated-group>".to_owned());
            return;
        };
        for item in split_reference_group(&group[..close]) {
            expand_reference_tree(item, prefix, references);
        }
        return;
    }
    let Some(path) = reference_path(source) else {
        return;
    };
    let qualified = if prefix.is_empty() {
        path.clone()
    } else {
        format!("{prefix}::{path}")
    };
    let remainder = source[path_source_length(source)..].trim_start();
    if let Some(group) = remainder
        .strip_prefix("::")
        .map(str::trim_start)
        .and_then(|remainder| remainder.strip_prefix('{'))
    {
        let Some(close) = matching_brace(group) else {
            references.insert("<unterminated-group>".to_owned());
            return;
        };
        for item in split_reference_group(&group[..close]) {
            expand_reference_tree(item, &qualified, references);
        }
    } else {
        references.insert(qualified);
    }
}

fn reference_path(source: &str) -> Option<String> {
    let length = path_source_length(source);
    let path = source[..length]
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .trim_end_matches(':')
        .to_owned();
    (!path.is_empty()).then_some(path)
}

fn path_source_length(source: &str) -> usize {
    let mut cursor = source.len() - source.trim_start().len();
    loop {
        let segment = source[cursor..]
            .chars()
            .take_while(|character| identifier_character(*character))
            .map(char::len_utf8)
            .sum::<usize>();
        let segment = if segment == 0 && source[cursor..].starts_with('*') {
            1
        } else {
            segment
        };
        if segment == 0 {
            return cursor;
        }
        cursor += segment;
        let separator = cursor + source[cursor..].len() - source[cursor..].trim_start().len();
        if !source[separator..].starts_with("::") {
            return cursor;
        }
        let next = separator + 2;
        if source[next..].trim_start().starts_with('{') {
            return cursor;
        }
        cursor = next + source[next..].len() - source[next..].trim_start().len();
    }
}

fn split_reference_group(source: &str) -> Vec<&str> {
    let mut depth = 0;
    let mut start = 0;
    let mut items = Vec::new();
    for (index, character) in source.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(&source[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    items.push(&source[start..]);
    items
}

fn matching_brace(source: &str) -> Option<usize> {
    let mut depth = 1;
    for (index, character) in source.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn crate_aliases(source: &str, crate_name: &str) -> BTreeSet<String> {
    let compact = compact_rust_code(source);
    let mut aliases = BTreeSet::new();
    for marker in [
        format!("use{crate_name}as"),
        format!("use::{crate_name}as"),
        format!("use{{{crate_name}as"),
        format!("use{{::{crate_name}as"),
        format!(",{crate_name}as"),
        format!(",::{crate_name}as"),
        format!("externcrate{crate_name}as"),
    ] {
        for (start, _) in compact.match_indices(&marker) {
            let alias = compact[start + marker.len()..]
                .chars()
                .take_while(|character| identifier_character(*character))
                .collect::<String>();
            if !alias.is_empty() {
                aliases.insert(alias);
            }
        }
    }
    for marker in [
        format!("use{crate_name}::{{"),
        format!("use::{crate_name}::{{"),
    ] {
        for (start, _) in compact.match_indices(&marker) {
            let group = &compact[start + marker.len()..];
            let Some(close) = matching_brace(group) else {
                continue;
            };
            for item in split_reference_group(&group[..close]) {
                let Some(alias) = item.strip_prefix("selfas") else {
                    continue;
                };
                if !alias.is_empty() && alias.chars().all(identifier_character) {
                    aliases.insert(alias.to_owned());
                }
            }
        }
    }
    aliases
}

fn crate_roots(source: &str, crate_name: &str) -> BTreeSet<String> {
    let mut roots = BTreeSet::from([crate_name.to_owned()]);
    loop {
        let aliases = roots
            .iter()
            .flat_map(|root| crate_aliases(source, root))
            .collect::<BTreeSet<_>>();
        let previous = roots.len();
        roots.extend(aliases);
        if roots.len() == previous {
            return roots;
        }
    }
}

fn adapter_references(source: &str, crate_name: &str, allow_process_id: bool) -> bool {
    let roots = crate_roots(source, crate_name);
    roots.into_iter().any(|root| {
        crate_references(source, &root).iter().any(|reference| {
            let process = (reference == "process" || reference.starts_with("process::"))
                && !(allow_process_id && reference == "process::id");
            let network = if crate_name == "std" {
                (reference == "net" || reference.starts_with("net::"))
                    && !allowed_reference(reference, STD_NET_ADDRESS_API)
            } else {
                reference == "net" || reference.starts_with("net::")
            };
            process
                || network
                || reference.starts_with("os::unix::net::")
                || reference.starts_with("os::windows::net::")
        })
    })
}

fn external_adapter_references(source: &str, crate_name: &str) -> bool {
    let roots = crate_roots(source, crate_name);
    let direct = roots.into_iter().any(|root| {
        crate_references(source, &root)
            .iter()
            .any(|reference| match crate_name {
                "rustix" => {
                    reference == "process"
                        || reference.starts_with("process::")
                        || reference == "net"
                        || reference.starts_with("net::")
                }
                "nix" => {
                    reference.starts_with("sys::socket")
                        || reference.starts_with("unistd::fork")
                        || reference.starts_with("unistd::exec")
                        || reference.starts_with("spawn")
                }
                "libc" => libc_adapter_symbol(reference),
                "socket2" => true,
                "mio" | "async_std" => reference == "net" || reference.starts_with("net::"),
                _ => false,
            })
    });
    direct || crate_name == "nix" && nix_intermediate_adapter_references(source)
}

fn nix_intermediate_adapter_references(source: &str) -> bool {
    [
        ("sys", &["socket"] as &[&str]),
        ("unistd", &["fork", "exec"] as &[&str]),
    ]
    .into_iter()
    .any(|(module, forbidden)| {
        crate_roots(source, "nix")
            .into_iter()
            .flat_map(|root| module_aliases(source, &root, module))
            .flat_map(|alias| crate_roots(source, &alias))
            .any(|alias| {
                crate_references(source, &alias).iter().any(|reference| {
                    forbidden.iter().any(|forbidden| {
                        reference == forbidden || reference.starts_with(&format!("{forbidden}::"))
                    })
                })
            })
    })
}

fn module_aliases(source: &str, crate_name: &str, module: &str) -> BTreeSet<String> {
    let compact = compact_rust_code(source);
    let mut aliases = BTreeSet::new();
    for marker in [
        format!("use{crate_name}::{module}"),
        format!("use::{crate_name}::{module}"),
    ] {
        for (start, _) in compact.match_indices(&marker) {
            let suffix = &compact[start + marker.len()..];
            if let Some(alias) = suffix.strip_prefix("as") {
                let alias = alias
                    .chars()
                    .take_while(|character| identifier_character(*character))
                    .collect::<String>();
                if !alias.is_empty() {
                    aliases.insert(alias);
                }
            } else if suffix.starts_with(';') {
                aliases.insert(module.to_owned());
            } else if let Some(group) = suffix.strip_prefix("::{") {
                let Some(close) = matching_brace(group) else {
                    continue;
                };
                for item in split_reference_group(&group[..close]) {
                    if let Some(alias) = item.strip_prefix("selfas") {
                        if !alias.is_empty() && alias.chars().all(identifier_character) {
                            aliases.insert(alias.to_owned());
                        }
                    }
                }
            }
        }
    }
    for marker in [
        format!("use{crate_name}::{{"),
        format!("use::{crate_name}::{{"),
    ] {
        for (start, _) in compact.match_indices(&marker) {
            let group = &compact[start + marker.len()..];
            let Some(close) = matching_brace(group) else {
                continue;
            };
            for item in split_reference_group(&group[..close]) {
                if item == module {
                    aliases.insert(module.to_owned());
                } else if let Some(alias) = item.strip_prefix(&format!("{module}as")) {
                    if !alias.is_empty() && alias.chars().all(identifier_character) {
                        aliases.insert(alias.to_owned());
                    }
                } else if let Some(alias) = item.strip_prefix(&format!("{module}::{{selfas")) {
                    if let Some(alias) = alias.strip_suffix('}') {
                        if !alias.is_empty() && alias.chars().all(identifier_character) {
                            aliases.insert(alias.to_owned());
                        }
                    }
                }
            }
        }
    }
    aliases
}

fn libc_adapter_symbol(reference: &str) -> bool {
    [
        "accept",
        "accept4",
        "bind",
        "connect",
        "exec",
        "fork",
        "listen",
        "posix_spawn",
        "socket",
        "socketpair",
        "system",
        "vfork",
    ]
    .iter()
    .any(|symbol| reference == *symbol || reference.starts_with(&format!("{symbol}_")))
}

fn identifier_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn is_rust_source(file: &TrackedSource) -> bool {
    file.path.ends_with(".rs")
}
