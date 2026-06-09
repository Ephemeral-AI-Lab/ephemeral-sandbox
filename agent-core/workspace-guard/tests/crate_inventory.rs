use workspace_guard::{str_set, Workspace, RETIRED_CRATES, RETIRED_CRATE_RULES, TARGET_CRATES};

#[test]
fn workspace_crates_match_target_set() {
    let workspace = Workspace::load();
    let crate_names = workspace.crate_names();
    let target = str_set(TARGET_CRATES);
    let extra = crate_names.difference(&target).cloned().collect::<Vec<_>>();
    let missing = target.difference(&crate_names).cloned().collect::<Vec<_>>();

    assert!(
        extra.is_empty() && missing.is_empty(),
        "crate_inventory rule violated: crate map drifted; extra={extra:?}, missing={missing:?}"
    );
}

#[test]
fn retired_crate_names_do_not_include_nonexistent_facades() {
    assert!(
        !str_set(RETIRED_CRATES).contains("eos-agent-api"),
        "crate_inventory rule violated: eos-agent-api never existed; retired crate list must name real crates"
    );
}

#[test]
fn retired_crates_are_absent_from_target_workspace() {
    let workspace = Workspace::load();
    let crate_names = workspace.crate_names();
    let mut violations = Vec::new();

    for rule in RETIRED_CRATE_RULES {
        if crate_names.contains(rule.retired) {
            violations.push(format!(
                "`{}` still exists; successor `{}` owns the target: {}",
                rule.retired, rule.successor, rule.target
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "crate_inventory rule violated:\n{}",
        violations.join("\n")
    );
}
