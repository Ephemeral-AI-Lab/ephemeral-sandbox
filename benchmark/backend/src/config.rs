use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const WORKSPACE_ENV: &str = "EPHEMERAL_SANDBOX_TEST_WORKSPACE";
pub const ROOT_MARKER: &str = ".eos-benchmark-root";

#[derive(Debug, Clone)]
pub struct StartupConfig {
    pub repo: PathBuf,
    pub bind: SocketAddr,
    pub web_root: PathBuf,
    pub paths: BenchmarkPaths,
    pub settings_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ResolveInput {
    pub repo: PathBuf,
    pub bind: SocketAddr,
    pub web_root: Option<PathBuf>,
    pub workspace_override: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct BenchmarkPaths {
    pub root: PathBuf,
    pub benchmark: PathBuf,
    pub fixtures: PathBuf,
    pub runs: PathBuf,
    pub results: PathBuf,
    pub runtime: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsResponse {
    pub schema_version: u32,
    pub test_workspace_root: String,
    pub source: WorkspaceRootSource,
    pub writable: bool,
    pub path_health: PathHealth,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRootSource {
    CommandLine,
    Environment,
    Persisted,
    SiblingDefault,
    ApiUpdate,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathHealth {
    pub canonical: bool,
    pub root_marker: bool,
    pub outside_repository: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsUpdate {
    pub test_workspace_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    schema_version: u32,
    test_workspace_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootMarker {
    schema_version: u32,
    owner: String,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("repository path is not an EphemeralOS checkout: {0}")]
    InvalidRepository(PathBuf),
    #[error("benchmark service may bind only to loopback, received {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("test workspace root must be outside the repository: {0}")]
    WorkspaceInsideRepository(PathBuf),
    #[error("test workspace root is not a directory: {0}")]
    WorkspaceNotDirectory(PathBuf),
    #[error("benchmark root marker is invalid at {path}: {reason}")]
    InvalidRootMarker { path: PathBuf, reason: String },
    #[error("unsupported settings schema version {0}")]
    UnsupportedSettingsVersion(u32),
    #[error("failed to parse settings {path}: {source}")]
    ParseSettings {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("filesystem operation failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl StartupConfig {
    pub fn resolve(input: ResolveInput) -> Result<(Self, WorkspaceRootSource), ConfigError> {
        if !input.bind.ip().is_loopback() {
            return Err(ConfigError::NonLoopbackBind(input.bind));
        }

        let repo = canonical_repository(&input.repo)?;
        let settings_path = settings_path();
        let (workspace_root, source) =
            resolve_workspace_root(input.workspace_override, &settings_path, &repo)?;
        let paths = BenchmarkPaths::initialize(&workspace_root, &repo)?;
        let web_root = input
            .web_root
            .unwrap_or_else(|| repo.join("benchmark/web/dist"));

        Ok((
            Self {
                repo,
                bind: input.bind,
                web_root,
                paths,
                settings_path,
            },
            source,
        ))
    }

    pub fn settings_response(&self, source: WorkspaceRootSource) -> SettingsResponse {
        SettingsResponse {
            schema_version: 1,
            test_workspace_root: self.paths.root.display().to_string(),
            source,
            writable: writable_probe(&self.paths.benchmark),
            path_health: PathHealth {
                canonical: self.paths.root.is_absolute(),
                root_marker: self.paths.benchmark.join(ROOT_MARKER).is_file(),
                outside_repository: !self.paths.root.starts_with(&self.repo),
            },
        }
    }

    pub fn persist_workspace_root(&mut self, root: &Path) -> Result<(), ConfigError> {
        let paths = BenchmarkPaths::initialize(root, &self.repo)?;
        let value = SettingsFile {
            schema_version: 1,
            test_workspace_root: paths.root.clone(),
        };
        write_json_atomic(&self.settings_path, &value)?;
        self.paths = paths;
        Ok(())
    }
}

impl BenchmarkPaths {
    pub fn initialize(root: &Path, repo: &Path) -> Result<Self, ConfigError> {
        fs::create_dir_all(root).map_err(|source| io_error(root, source))?;
        let root = root
            .canonicalize()
            .map_err(|source| io_error(root, source))?;
        if !root.is_dir() {
            return Err(ConfigError::WorkspaceNotDirectory(root));
        }
        if root.starts_with(repo) || repo.starts_with(&root) {
            return Err(ConfigError::WorkspaceInsideRepository(root));
        }

        let benchmark = root.join("benchmark");
        let fixtures = benchmark.join("fixtures");
        let runs = benchmark.join("runs");
        let results = benchmark.join("results");
        let runtime = benchmark.join("runtime");
        for path in [&benchmark, &fixtures, &runs, &results, &runtime] {
            fs::create_dir_all(path).map_err(|source| io_error(path, source))?;
        }
        ensure_root_marker(&benchmark)?;

        Ok(Self {
            root,
            benchmark,
            fixtures,
            runs,
            results,
            runtime,
        })
    }
}

fn canonical_repository(repo: &Path) -> Result<PathBuf, ConfigError> {
    let canonical = repo
        .canonicalize()
        .map_err(|source| io_error(repo, source))?;
    if !canonical.join("Cargo.toml").is_file()
        || !canonical.join("crates").is_dir()
        || !canonical.join("benchmark").is_dir()
    {
        return Err(ConfigError::InvalidRepository(canonical));
    }
    Ok(canonical)
}

fn resolve_workspace_root(
    cli: Option<PathBuf>,
    persisted_path: &Path,
    repo: &Path,
) -> Result<(PathBuf, WorkspaceRootSource), ConfigError> {
    if let Some(path) = cli {
        return Ok((path, WorkspaceRootSource::CommandLine));
    }
    if let Some(path) = env::var_os(WORKSPACE_ENV).map(PathBuf::from) {
        return Ok((path, WorkspaceRootSource::Environment));
    }
    if persisted_path.is_file() {
        let bytes = fs::read(persisted_path).map_err(|source| io_error(persisted_path, source))?;
        let settings: SettingsFile =
            serde_json::from_slice(&bytes).map_err(|source| ConfigError::ParseSettings {
                path: persisted_path.to_path_buf(),
                source,
            })?;
        if settings.schema_version != 1 {
            return Err(ConfigError::UnsupportedSettingsVersion(
                settings.schema_version,
            ));
        }
        return Ok((settings.test_workspace_root, WorkspaceRootSource::Persisted));
    }

    let sibling = repo
        .parent()
        .unwrap_or(repo)
        .join("ephemeral-sandbox-test-workspace");
    Ok((sibling, WorkspaceRootSource::SiblingDefault))
}

fn settings_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        return home_dir().join("Library/Application Support/EphemeralOS/benchmark/settings.json");
    }
    if cfg!(target_os = "windows") {
        return env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home_dir)
            .join("EphemeralOS/benchmark/settings.json");
    }
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("EphemeralOS/benchmark/settings.json")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
}

fn ensure_root_marker(benchmark: &Path) -> Result<(), ConfigError> {
    let marker_path = benchmark.join(ROOT_MARKER);
    if marker_path.exists() {
        let bytes = fs::read(&marker_path).map_err(|source| io_error(&marker_path, source))?;
        let marker: RootMarker =
            serde_json::from_slice(&bytes).map_err(|error| ConfigError::InvalidRootMarker {
                path: marker_path.clone(),
                reason: error.to_string(),
            })?;
        if marker.schema_version != 1 || marker.owner != "ephemeralos-benchmark" {
            return Err(ConfigError::InvalidRootMarker {
                path: marker_path,
                reason: "marker identity does not match".to_owned(),
            });
        }
        return Ok(());
    }

    write_json_atomic(
        &marker_path,
        &RootMarker {
            schema_version: 1,
            owner: "ephemeralos-benchmark".to_owned(),
        },
    )
}

fn writable_probe(directory: &Path) -> bool {
    let path = directory.join(format!(".writable-{}", std::process::id()));
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            let result = file.write_all(b"ok").and_then(|()| file.sync_all());
            drop(file);
            let _ = fs::remove_file(path);
            result.is_ok()
        }
        Err(_) => false,
    }
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), ConfigError> {
    let parent = path
        .parent()
        .ok_or_else(|| ConfigError::InvalidRootMarker {
            path: path.to_path_buf(),
            reason: "path has no parent".to_owned(),
        })?;
    fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    let temporary = parent.join(format!(".{}.tmp-{}", file_name(path), std::process::id()));
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|error| ConfigError::InvalidRootMarker {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error(&temporary, source))?;
    fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
    sync_directory(parent)?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), ConfigError> {
    let directory = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    directory
        .sync_all()
        .map_err(|source| io_error(path, source))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("settings")
        .to_owned()
}

fn io_error(path: &Path, source: io::Error) -> ConfigError {
    ConfigError::Io {
        path: path.to_path_buf(),
        source,
    }
}
