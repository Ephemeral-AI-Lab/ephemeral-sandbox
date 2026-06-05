#[path = "../support/mod.rs"]
mod support;

const E2E_CONFIG: &str = "crates/eos-e2e-test/tests/layerstack/config/default.test.yml";

mod commit_to_git;
mod commit_to_workspace;
mod lease;
mod squash;
mod squash_bounds;
mod squash_deep;
