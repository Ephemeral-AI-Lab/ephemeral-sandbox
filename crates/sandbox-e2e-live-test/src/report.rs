use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::cleanup::CleanupReport;
use crate::cli_client::CallRecord;
use crate::config::RunConfig;

const EXCHANGE_SCHEMA_VERSION: u32 = 1;
/// Load-bearing: the live `ManifestConfig` reader bails unless this is `1`.
const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const RESULT_SCHEMA_VERSION: u32 = 1;
pub const SUMMARY_SCHEMA_VERSION: u32 = 1;

const RUN_MANIFEST_FILE: &str = "run-manifest.json";
const SUMMARY_FILE: &str = "summary.json";

/// Write `{run_root}/reports/{sandbox_id}/exchange.jsonl`: a `{schema_version}`
/// header line followed by one JSON object per call record. Creates the report
/// dir. Best-effort: returns `io::Result` so the caller (`Sandbox::drop`) can
/// swallow failures without aborting teardown.
pub fn write_exchange(run_root: &Path, sandbox_id: &str, records: &[CallRecord]) -> io::Result<()> {
    let report_dir = run_root.join("reports").join(sandbox_id);
    fs::create_dir_all(&report_dir)?;

    let mut body = json!({ "schema_version": EXCHANGE_SCHEMA_VERSION }).to_string();
    body.push('\n');
    for record in records {
        body.push_str(&record.to_exchange_line().to_string());
        body.push('\n');
    }

    fs::write(report_dir.join("exchange.jsonl"), body)
}

/// `{ total, failed }` assertion tally, shared by `result.json` and the summary
/// `tests[]` rollup.
#[derive(Serialize, Deserialize)]
pub struct Assertions {
    pub total: u64,
    pub failed: u64,
}

/// The per-sandbox `result.json` payload, written in `Sandbox::drop` (§5.2) and
/// re-read by the orchestrator when building the summary rollup (§5.3).
#[derive(Serialize, Deserialize)]
pub struct TestOutcome {
    pub schema_version: u32,
    pub test_name: String,
    pub sandbox_id: String,
    pub status: String,
    pub duration_ms: u128,
    pub workspace_root: String,
    pub assertions: Assertions,
    pub failure: Option<String>,
}

/// Write `{run_root}/reports/{sandbox_id}/result.json` into the same report dir
/// `write_exchange` creates. Best-effort, mirroring `write_exchange`.
pub fn write_result(run_root: &Path, outcome: &TestOutcome) -> io::Result<()> {
    let report_dir = run_root.join("reports").join(&outcome.sandbox_id);
    fs::create_dir_all(&report_dir)?;
    write_json_pretty(&report_dir.join("result.json"), outcome)
}

#[derive(Serialize)]
struct ManifestConfigSummary {
    max_parallel: usize,
    cleanup: &'static str,
    cli_timeout_secs: f64,
    build: String,
}

#[derive(Serialize)]
struct RunManifestDoc<'a> {
    schema_version: u32,
    gateway_socket: &'a Path,
    run_id: &'a str,
    image: &'a str,
    git_head: &'a str,
    config: ManifestConfigSummary,
    clock: &'a str,
}

/// Write the orchestrator-emitted `run-manifest.json` superset (§5.1). The four
/// load-bearing fields (`schema_version == 1`, `gateway_socket`, `run_id`,
/// `image`) stay readable by the live `ManifestConfig`; `git_head`/`config`/
/// `clock` are superset-only and ignored by that reader.
pub fn write_run_manifest(run_root: &Path, config: &RunConfig, git_head: &str) -> io::Result<()> {
    fs::create_dir_all(run_root)?;
    let doc = RunManifestDoc {
        schema_version: MANIFEST_SCHEMA_VERSION,
        gateway_socket: &config.gateway_socket,
        run_id: &config.run_id,
        image: &config.image,
        git_head,
        config: ManifestConfigSummary {
            max_parallel: config.max_parallel,
            cleanup: config.cleanup.as_str(),
            cli_timeout_secs: config.cli_timeout.as_secs_f64(),
            build: config.build.summary(),
        },
        clock: &config.clock,
    };
    write_json_pretty(&run_root.join(RUN_MANIFEST_FILE), &doc)
}

/// One `summary.tests[]` entry (§5.3), built from a `result.json` or synthesized
/// as `errored` for a report dir whose `result.json` is missing.
#[derive(Serialize)]
pub struct TestEntry {
    pub name: String,
    pub sandbox_id: String,
    pub status: String,
    pub duration_ms: u128,
    pub workspace_root: String,
    pub report_dir: String,
    pub assertions: Assertions,
    pub failure: Option<String>,
}

/// `summary.counts` rollup; `skipped` is always `0` under the orchestrator (§6).
#[derive(Serialize)]
pub struct Counts {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errored: usize,
}

impl Counts {
    #[must_use]
    pub fn tally(tests: &[TestEntry]) -> Counts {
        let mut counts = Counts {
            total: tests.len(),
            passed: 0,
            failed: 0,
            skipped: 0,
            errored: 0,
        };
        for test in tests {
            match test.status.as_str() {
                "passed" => counts.passed += 1,
                "failed" => counts.failed += 1,
                "errored" => counts.errored += 1,
                _ => {}
            }
        }
        counts
    }
}

#[derive(Serialize)]
pub struct BuildTiming {
    pub gateway_build_ms: u128,
    pub cli_build_ms: u128,
    pub cargo_profile: String,
    pub cache_hit: bool,
}

#[derive(Serialize)]
pub struct RunnerTiming {
    pub wall_ms: u128,
    pub gateway_attach_ms: u128,
    pub test_process_ms: u128,
    pub teardown_ms: u128,
    pub max_parallel: usize,
}

#[derive(Serialize)]
pub struct PerTest {
    pub name: String,
    pub sandbox_id: String,
    pub total_ms: u128,
}

#[derive(Serialize)]
pub struct Timing {
    pub build: BuildTiming,
    pub runner: RunnerTiming,
    pub per_test: Vec<PerTest>,
}

/// The `summary.json` rollup (§5.3). Built solely from globbed `result.json`
/// plus the cargo-test exit code — never from libtest stdout.
#[derive(Serialize)]
pub struct Summary {
    pub schema_version: u32,
    pub run_id: String,
    pub git_head: String,
    pub started_at: String,
    pub finished_at: String,
    pub max_parallel: usize,
    pub status: String,
    pub counts: Counts,
    pub tests: Vec<TestEntry>,
    pub failed_tests: Vec<String>,
    pub artifacts_root: String,
    pub timing: Timing,
    pub cleanup: CleanupReport,
}

/// Write `{run_root}/summary.json`.
pub fn write_summary(run_root: &Path, summary: &Summary) -> io::Result<()> {
    fs::create_dir_all(run_root)?;
    write_json_pretty(&run_root.join(SUMMARY_FILE), summary)
}

/// An `errored` `tests[]` entry keyed on the report dir name, used when a
/// `result.json` is absent or unreadable — no test identity is recoverable.
fn errored_entry(id: String, report_dir: String, failure: String) -> TestEntry {
    TestEntry {
        name: id.clone(),
        sandbox_id: id,
        status: "errored".to_owned(),
        duration_ms: 0,
        workspace_root: String::new(),
        report_dir,
        assertions: Assertions {
            total: 0,
            failed: 0,
        },
        failure: Some(failure),
    }
}

/// Build `summary.tests[]` by globbing `{run_root}/reports/*/`. A dir whose
/// `result.json` parses yields its recorded entry; a missing `result.json`
/// yields an `errored` entry (`"result.json missing"`) and an unparsable one an
/// `errored` entry naming the parse error (§5.3).
#[must_use]
pub fn build_tests(run_root: &Path) -> Vec<TestEntry> {
    let reports = run_root.join("reports");
    let Ok(read_dir) = fs::read_dir(&reports) else {
        return Vec::new();
    };
    let mut dirs: Vec<_> = read_dir
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .collect();
    dirs.sort_by_key(std::fs::DirEntry::file_name);

    let mut tests = Vec::with_capacity(dirs.len());
    for entry in dirs {
        let id = entry.file_name().to_string_lossy().into_owned();
        let report_dir = entry.path();
        let report_dir_str = report_dir.to_string_lossy().into_owned();
        let test = match fs::read(report_dir.join("result.json")) {
            Ok(bytes) => match serde_json::from_slice::<TestOutcome>(&bytes) {
                Ok(outcome) => TestEntry {
                    name: outcome.test_name,
                    sandbox_id: outcome.sandbox_id,
                    status: outcome.status,
                    duration_ms: outcome.duration_ms,
                    workspace_root: outcome.workspace_root,
                    report_dir: report_dir_str,
                    assertions: outcome.assertions,
                    failure: outcome.failure,
                },
                Err(error) => errored_entry(
                    id,
                    report_dir_str,
                    format!("result.json unparsable: {error}"),
                ),
            },
            Err(_) => errored_entry(id, report_dir_str, "result.json missing".to_owned()),
        };
        tests.push(test);
    }
    tests
}

fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    fs::write(path, bytes)
}
