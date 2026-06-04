// AC-audit-07: the crate's dependency set is exactly {eos-types,
// eos-obs-contract} among EphemeralOS crates. It must not depend on downstream
// agent-core crates (no eos-tools / eos-engine / eos-workflow / ...), which
// would create the eos-audit -> eos-engine -> eos-audit cycle banned by
// GC-audit-05. The workspace-wide dependency topology is also guarded centrally
// by workspace-guard; this is the crate-local proof.
//
// Plain `//` comments throughout so clippy::doc_markdown never fires on crate
// identifiers in a test file.

use std::collections::BTreeSet;
use std::process::Command;

#[test]
fn eos_audit_internal_deps_are_base_contracts_only() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version=1", "--no-deps"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run `cargo metadata`");
    assert!(output.status.success(), "`cargo metadata` exited non-zero");

    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse cargo metadata json");
    let packages = meta["packages"].as_array().expect("packages array");

    let audit = packages
        .iter()
        .find(|p| p["name"].as_str() == Some("eos-audit"))
        .expect("eos-audit package present");

    let internal_deps: BTreeSet<&str> = audit["dependencies"]
        .as_array()
        .expect("dependencies array")
        .iter()
        .filter_map(|d| d["name"].as_str())
        .filter(|n| n.starts_with("eos-"))
        .collect();

    let expected: BTreeSet<&str> = ["eos-obs-contract", "eos-types"].into_iter().collect();
    assert_eq!(
        internal_deps, expected,
        "eos-audit must depend only on base contracts among EphemeralOS crates"
    );
}
