use std::ffi::OsString;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use sandbox_benchmark::config::{
    BenchmarkPaths, ConfigError, ResolveInput, StartupConfig, WorkspaceRootSource, ROOT_MARKER,
    WORKSPACE_ENV,
};

use crate::support::{create_fake_repository, TestRoot};

#[test]
fn workspace_root_precedence_is_cli_then_environment_then_persisted_then_sibling() {
    let test_root = TestRoot::new("config-precedence");
    let repo = test_root.join("repository");
    create_fake_repository(&repo);

    let cli_root = test_root.join("workspaces/cli");
    let environment_root = test_root.join("workspaces/environment");
    let persisted_root = test_root.join("workspaces/persisted");
    let config_home = test_root.join("config-home");
    let _environment = EnvironmentGuard::replace([
        ("HOME", Some(config_home.clone().into_os_string())),
        (
            "XDG_CONFIG_HOME",
            Some(config_home.clone().into_os_string()),
        ),
        ("APPDATA", Some(config_home.clone().into_os_string())),
        (
            WORKSPACE_ENV,
            Some(environment_root.clone().into_os_string()),
        ),
    ]);

    let (cli, source) = resolve(&repo, Some(cli_root.clone())).expect("resolve CLI root");
    assert!(matches!(source, WorkspaceRootSource::CommandLine));
    assert_eq!(cli.paths.root, canonical(&cli_root));

    let settings_path = cli.settings_path.clone();
    fs::create_dir_all(settings_path.parent().expect("settings parent"))
        .expect("create settings parent");
    let settings = serde_json::json!({
        "schema_version": 1,
        "test_workspace_root": persisted_root,
    });
    fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&settings).expect("serialize settings"),
    )
    .expect("write persisted settings");

    let (environment, source) = resolve(&repo, None).expect("resolve environment root");
    assert!(matches!(source, WorkspaceRootSource::Environment));
    assert_eq!(environment.paths.root, canonical(&environment_root));

    std::env::remove_var(WORKSPACE_ENV);
    let (persisted, source) = resolve(&repo, None).expect("resolve persisted root");
    assert!(matches!(source, WorkspaceRootSource::Persisted));
    assert_eq!(persisted.paths.root, canonical(&persisted_root));

    fs::remove_file(settings_path).expect("remove persisted settings");
    let (sibling, source) = resolve(&repo, None).expect("resolve sibling root");
    assert!(matches!(source, WorkspaceRootSource::SiblingDefault));
    assert_eq!(
        sibling.paths.root,
        canonical(&test_root.join("ephemeral-sandbox-test-workspace"))
    );
    assert!(sibling.bind.ip().is_loopback());
}

#[test]
fn startup_rejects_non_loopback_bind_before_creating_a_workspace() {
    let test_root = TestRoot::new("config-loopback");
    let repo = test_root.join("repository");
    create_fake_repository(&repo);
    let workspace = test_root.join("workspace");
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);

    let error = StartupConfig::resolve(ResolveInput {
        repo,
        bind,
        web_root: None,
        workspace_override: Some(workspace.clone()),
    })
    .expect_err("non-loopback bind must fail");

    assert!(matches!(error, ConfigError::NonLoopbackBind(address) if address == bind));
    assert!(!workspace.exists());
}

#[test]
fn benchmark_paths_are_separate_and_reject_repository_ancestors_or_descendants() {
    let test_root = TestRoot::new("config-paths");
    let repo = test_root.join("repository");
    create_fake_repository(&repo);
    let repo = canonical(&repo);
    let workspace = test_root.join("workspace");

    let paths = BenchmarkPaths::initialize(&workspace, &repo).expect("initialize benchmark paths");
    assert_eq!(paths.root, canonical(&workspace));
    assert_eq!(paths.benchmark, paths.root.join("benchmark"));
    assert_eq!(paths.fixtures, paths.benchmark.join("fixtures"));
    assert_eq!(paths.runs, paths.benchmark.join("runs"));
    assert_eq!(paths.results, paths.benchmark.join("results"));
    assert_eq!(paths.runtime, paths.benchmark.join("runtime"));
    assert!(paths.benchmark.join(ROOT_MARKER).is_file());

    let inside_repo = repo.join("benchmark-workspace");
    let error = BenchmarkPaths::initialize(&inside_repo, &repo)
        .expect_err("workspace inside repository must fail");
    assert!(matches!(error, ConfigError::WorkspaceInsideRepository(_)));

    let error = BenchmarkPaths::initialize(test_root.path(), &repo)
        .expect_err("workspace containing repository must fail");
    assert!(matches!(error, ConfigError::WorkspaceInsideRepository(_)));
}

fn resolve(
    repo: &Path,
    workspace_override: Option<PathBuf>,
) -> Result<(StartupConfig, WorkspaceRootSource), ConfigError> {
    StartupConfig::resolve(ResolveInput {
        repo: repo.to_path_buf(),
        bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        web_root: None,
        workspace_override,
    })
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().expect("canonical test path")
}

struct EnvironmentGuard {
    original: Vec<(&'static str, Option<OsString>)>,
}

impl EnvironmentGuard {
    fn replace<const N: usize>(changes: [(&'static str, Option<OsString>); N]) -> Self {
        let mut original = Vec::with_capacity(N);
        for (key, value) in changes {
            original.push((key, std::env::var_os(key)));
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        Self { original }
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (key, value) in self.original.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
