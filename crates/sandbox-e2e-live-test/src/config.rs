use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;

const RUN_ROOT_ENV: &str = "EOS_E2E_RUN_ROOT";
const MANIFEST_FILE: &str = "run-manifest.json";
const SUPPORTED_SCHEMA_VERSION: u32 = 1;

const RUN_ROOT_BASE_ENV: &str = "EOS_E2E_RUN_ROOT_BASE";
const MAX_PARALLEL_ENV: &str = "EOS_E2E_MAX_PARALLEL";
const RUN_CLOCK_ENV: &str = "EOS_E2E_RUN_CLOCK";
const RUN_SALT_ENV: &str = "EOS_E2E_RUN_SALT";
const GATEWAY_SOCKET_ENV: &str = "SANDBOX_GATEWAY_SOCKET";

const DEFAULT_IMAGE: &str = "ubuntu:24.04";
const DEFAULT_CARGO_PROFILE: &str = "package-fast";
const DEFAULT_CLI_TIMEOUT_SECS: f64 = 30.0;
const DEFAULT_GATEWAY_READY_TIMEOUT_SECS: f64 = 5.0;
const MAX_DEFAULT_PARALLELISM: usize = 8;

/// Minimal run configuration the live test side reads from the manifest under
/// `EOS_E2E_RUN_ROOT`. The orchestrator emits a superset `run-manifest.json`
/// (§5.1) of which this struct reads only the four load-bearing fields; the
/// full orchestrator config is [`RunConfig`].
pub struct ManifestConfig {
    pub run_root: PathBuf,
    pub gateway_socket: PathBuf,
    pub run_id: String,
    pub image: String,
}

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    gateway_socket: PathBuf,
    run_id: String,
    image: String,
}

impl ManifestConfig {
    /// Returns `Ok(None)` when `EOS_E2E_RUN_ROOT` is unset (the skip signal);
    /// `Ok(Some(_))` when the env is set and the manifest parses; `Err` only when
    /// the env is set but the manifest is missing/invalid (a real misconfig).
    pub fn from_env() -> anyhow::Result<Option<ManifestConfig>> {
        let Some(run_root) = std::env::var_os(RUN_ROOT_ENV) else {
            return Ok(None);
        };
        let run_root = PathBuf::from(run_root);
        let manifest_path = run_root.join(MANIFEST_FILE);
        let bytes = std::fs::read(&manifest_path)
            .with_context(|| format!("reading run manifest at {}", manifest_path.display()))?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing run manifest at {}", manifest_path.display()))?;
        if manifest.schema_version != SUPPORTED_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported run-manifest schema_version {} (expected {SUPPORTED_SCHEMA_VERSION})",
                manifest.schema_version
            );
        }
        Ok(Some(ManifestConfig {
            run_root,
            gateway_socket: manifest.gateway_socket,
            run_id: manifest.run_id,
            image: manifest.image,
        }))
    }
}

/// Which test leaves the orchestrator runs (§9). Resolved from `--test-names`
/// and `--rerun-failed-from`; the libtest filters are the `module_slug::fn`
/// thread names recorded in each `result.json`.
pub enum TestSelection {
    All,
    Names(Vec<String>),
    RerunFailedFrom(PathBuf),
}

/// Run-root removal policy (§7.2 step 3).
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum CleanupPolicy {
    Always,
    OnSuccess,
    Never,
}

impl CleanupPolicy {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            CleanupPolicy::Always => "Always",
            CleanupPolicy::OnSuccess => "OnSuccess",
            CleanupPolicy::Never => "Never",
        }
    }
}

/// Where the gateway/CLI binaries come from. Phase A build runs only for
/// `Cargo`; either variant is skipped entirely when `--gateway-socket` is given
/// (attach-only v1).
pub enum BuildSource {
    Cargo { profile: String },
    Prebuilt(PathBuf),
}

impl BuildSource {
    #[must_use]
    pub fn cargo_profile(&self) -> String {
        match self {
            BuildSource::Cargo { profile } => profile.clone(),
            BuildSource::Prebuilt(_) => "prebuilt".to_owned(),
        }
    }

    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            BuildSource::Cargo { profile } => format!("cargo:{profile}"),
            BuildSource::Prebuilt(dir) => format!("prebuilt:{}", dir.display()),
        }
    }
}

/// The `eos-e2e` command line. `preflight` is the only subcommand; everything
/// else (including `--clean-run`/`--rerun-failed-from`) is a flag on the default
/// `run` path, so the shared [`RunArgs`] are flattened and global.
#[derive(clap::Parser)]
#[command(
    name = "eos-e2e",
    about = "EphemeralOS black-box live E2E orchestrator"
)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,
    #[command(flatten)]
    pub run: RunArgs,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Run only the preflight checks (Linux, Docker, image, real-runtime probe).
    Preflight,
}

#[derive(clap::Args)]
pub struct RunArgs {
    /// Explicit run id (charset `[A-Za-z0-9._-]`); derived deterministically when omitted.
    #[arg(long, global = true)]
    pub run_id: Option<String>,
    /// Libtest `--test-threads` parallelism; falls back to `EOS_E2E_MAX_PARALLEL`.
    #[arg(long, global = true)]
    pub max_parallel: Option<usize>,
    /// Restrict to these `module_slug::fn` libtest filters.
    #[arg(long = "test-names", global = true)]
    pub test_names: Vec<String>,
    /// Rerun only the `failed_tests[]` recorded in a prior `summary.json`.
    #[arg(long, global = true)]
    pub rerun_failed_from: Option<PathBuf>,
    /// Container image used to provision sandboxes.
    #[arg(long, global = true)]
    pub image: Option<String>,
    /// Run-root base dir; the resolved run root is `{base}/{run_id}`.
    #[arg(long = "run-root", global = true)]
    pub run_root: Option<PathBuf>,
    /// Attach to an already-running gateway at this socket (attach-only v1).
    #[arg(long, global = true)]
    pub gateway_socket: Option<PathBuf>,
    /// Use prebuilt gateway/CLI binaries from this dir instead of building.
    #[arg(long, global = true)]
    pub prebuilt_bin_dir: Option<PathBuf>,
    /// Cargo profile for Phase A builds (ignored when attach-only).
    #[arg(long, global = true)]
    pub cargo_profile: Option<String>,
    /// Per-CLI-call timeout in seconds.
    #[arg(long, global = true)]
    pub cli_timeout_secs: Option<f64>,
    /// Gateway readiness timeout in seconds.
    #[arg(long, global = true)]
    pub gateway_ready_timeout_secs: Option<f64>,
    /// Run-root cleanup policy.
    #[arg(long, global = true)]
    pub cleanup: Option<CleanupPolicy>,
    /// Force `CleanupPolicy::Never` (keep the run root regardless of outcome).
    #[arg(long, global = true)]
    pub keep_artifacts: bool,
    /// Re-run teardown for a prior run id, then exit.
    #[arg(long, global = true)]
    pub clean_run: Option<String>,
}

/// Validated, resolved orchestrator configuration (precedence flag > env >
/// default). Distinct from the test-side [`ManifestConfig`].
pub struct RunConfig {
    pub run_id: String,
    pub max_parallel: usize,
    pub tests: TestSelection,
    pub image: String,
    pub run_root: PathBuf,
    pub gateway_socket: PathBuf,
    pub build: BuildSource,
    pub cli_timeout: Duration,
    pub gateway_ready_timeout: Duration,
    pub cleanup: CleanupPolicy,
    pub clock: String,
}

impl RunConfig {
    /// Resolve [`RunArgs`] into a validated config. Derives the run id and clock,
    /// applies precedence, and validates the charset/non-empty/positive rules.
    pub fn resolve(args: &RunArgs) -> anyhow::Result<RunConfig> {
        let clock = resolve_clock();
        let run_id = match &args.run_id {
            Some(id) => {
                validate_run_id(id)?;
                id.clone()
            }
            None => derive_run_id(&clock)?,
        };

        let max_parallel = args
            .max_parallel
            .or_else(|| {
                std::env::var(MAX_PARALLEL_ENV)
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or_else(default_parallelism)
            .max(1);

        let tests = if let Some(path) = &args.rerun_failed_from {
            TestSelection::Names(parse_failed_tests(path)?)
        } else if args.test_names.is_empty() {
            TestSelection::All
        } else {
            TestSelection::Names(args.test_names.clone())
        };

        let image = resolve_image(args)?;
        let run_root = resolve_run_root_base(args).join(&run_id);
        let gateway_socket = resolve_gateway_socket(args)?;

        let build = match &args.prebuilt_bin_dir {
            Some(dir) => BuildSource::Prebuilt(dir.clone()),
            None => BuildSource::Cargo {
                profile: args
                    .cargo_profile
                    .clone()
                    .unwrap_or_else(|| DEFAULT_CARGO_PROFILE.to_owned()),
            },
        };

        let cli_timeout = positive_duration(
            args.cli_timeout_secs.unwrap_or(DEFAULT_CLI_TIMEOUT_SECS),
            "--cli-timeout-secs",
        )?;
        let gateway_ready_timeout = positive_duration(
            args.gateway_ready_timeout_secs
                .unwrap_or(DEFAULT_GATEWAY_READY_TIMEOUT_SECS),
            "--gateway-ready-timeout-secs",
        )?;

        let cleanup = if args.keep_artifacts {
            CleanupPolicy::Never
        } else {
            args.cleanup.unwrap_or(CleanupPolicy::OnSuccess)
        };

        Ok(RunConfig {
            run_id,
            max_parallel,
            tests,
            image,
            run_root,
            gateway_socket,
            build,
            cli_timeout,
            gateway_ready_timeout,
            cleanup,
            clock,
        })
    }
}

/// Resolve the sandbox image (precedence `--image` > default `ubuntu:24.04`).
/// Needs no gateway socket, so preflight checks 1–3 can run without one.
pub fn resolve_image(args: &RunArgs) -> anyhow::Result<String> {
    let image = args
        .image
        .clone()
        .unwrap_or_else(|| DEFAULT_IMAGE.to_owned());
    if image.trim().is_empty() {
        anyhow::bail!("--image must not be empty");
    }
    Ok(image)
}

/// Resolve the gateway socket (precedence `--gateway-socket` >
/// `SANDBOX_GATEWAY_SOCKET`). Required in attach-only v1; only the run pipeline
/// and the preflight probe (check 4) demand it.
pub fn resolve_gateway_socket(args: &RunArgs) -> anyhow::Result<PathBuf> {
    args.gateway_socket
        .clone()
        .or_else(|| std::env::var_os(GATEWAY_SOCKET_ENV).map(PathBuf::from))
        .context("--gateway-socket (or SANDBOX_GATEWAY_SOCKET) is required in attach-only v1")
}

/// Resolve the run-root base (precedence `--run-root`, then
/// `EOS_E2E_RUN_ROOT_BASE`, then `${TMPDIR:-/tmp}/eos-e2e`). The resolved run
/// root is `{base}/{run_id}`; this base is distinct from the cross-process
/// `EOS_E2E_RUN_ROOT` export.
#[must_use]
pub fn resolve_run_root_base(args: &RunArgs) -> PathBuf {
    if let Some(base) = &args.run_root {
        return base.clone();
    }
    if let Some(base) = std::env::var_os(RUN_ROOT_BASE_ENV) {
        return PathBuf::from(base);
    }
    let tmp = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    tmp.join("eos-e2e")
}

/// Parse the `failed_tests[]` libtest filters from a prior `summary.json` (§9).
/// Resolving `--rerun-failed-from` through this up front means a malformed
/// summary fails before any run root or manifest is created.
pub fn parse_failed_tests(path: &Path) -> anyhow::Result<Vec<String>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading rerun summary {}", path.display()))?;
    let summary: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing rerun summary {}", path.display()))?;
    Ok(summary
        .get("failed_tests")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default())
}

/// Reject run ids that are empty or carry a char outside the `SandboxId`
/// charset `[A-Za-z0-9._-]` (notably `:`, which the colon-free clock avoids).
pub fn validate_run_id(run_id: &str) -> anyhow::Result<()> {
    if run_id.is_empty() {
        anyhow::bail!("run id must not be empty");
    }
    if !run_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        anyhow::bail!("run id {run_id:?} must match [A-Za-z0-9._-] (no ':')");
    }
    Ok(())
}

/// Colon-free UTC stamp (`YYYYMMDDThhmmssZ`) field-formatted from
/// `OffsetDateTime::now_utc()`. `time` ships no formatting feature in this
/// workspace, so the stamp is built by hand (precedent: `transcript.rs:42-53`).
#[must_use]
pub fn utc_stamp() -> String {
    let now = OffsetDateTime::now_utc();
    format!(
        "{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z",
        year = now.year(),
        month = now.month() as u8,
        day = now.day(),
        hour = now.hour(),
        minute = now.minute(),
        second = now.second(),
    )
}

fn resolve_clock() -> String {
    std::env::var(RUN_CLOCK_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(utc_stamp)
}

fn default_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(MAX_DEFAULT_PARALLELISM)
}

fn positive_duration(secs: f64, flag: &str) -> anyhow::Result<Duration> {
    if !secs.is_finite() || secs <= 0.0 {
        anyhow::bail!("{flag} must be > 0 (got {secs})");
    }
    Ok(Duration::from_secs_f64(secs))
}

fn derive_run_id(clock: &str) -> anyhow::Result<String> {
    let git_head = git_head()?;
    let manifest_hash = test_manifest_hash();
    let salt = std::env::var(RUN_SALT_ENV).unwrap_or_default();

    let mut hasher = Sha256::new();
    hasher.update(git_head.as_bytes());
    hasher.update(manifest_hash.as_bytes());
    hasher.update(salt.as_bytes());
    let digest = hasher.finalize();

    let slug = hex_encode(&digest[..4]);
    Ok(format!("r{clock}-{slug}"))
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// `git rev-parse HEAD` for the working tree, recorded in `run-manifest.json`
/// and `summary.json` and folded into the deterministic `run_id` digest.
pub fn git_head() -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .context("spawning `git rev-parse HEAD`")?;
    if !output.status.success() {
        anyhow::bail!(
            "`git rev-parse HEAD` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn test_manifest_hash() -> String {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut leaves = Vec::new();
    for scope in ["manager", "runtime"] {
        collect_test_leaves(
            &manifest_dir.join("tests").join(scope),
            manifest_dir,
            &mut leaves,
        );
    }
    leaves.sort();
    let joined = leaves.join("\n");

    let mut hasher = Sha256::new();
    hasher.update(joined.as_bytes());
    let digest = hasher.finalize();
    hex_encode(&digest)
}

fn collect_test_leaves(dir: &Path, base: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_test_leaves(&path, base, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            if let Ok(relative) = path.strip_prefix(base) {
                out.push(relative.to_string_lossy().into_owned());
            }
        }
    }
}
