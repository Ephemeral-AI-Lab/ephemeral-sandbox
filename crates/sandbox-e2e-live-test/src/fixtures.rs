use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use crate::assertion;
use crate::cli_client::{CallRecord, CliClient, CLI_BIN};
use crate::config::ManifestConfig;
use crate::gateway;
use crate::report;

/// Lazy harness singleton: env → manifest → `CliClient`, plus the per-test
/// provisioning entry point. Owns the one `CliClient` every leaf shares.
pub struct Harness {
    cli: CliClient,
    run_root: PathBuf,
    run_id: String,
    image: String,
}

impl Harness {
    /// Lazy singleton. Reads `EOS_E2E_RUN_ROOT` → `run-manifest.json` once.
    /// Returns `None` when `EOS_E2E_RUN_ROOT` is unset (skip signal for every
    /// leaf); panics only when the env is set but the manifest is
    /// missing/invalid (a real misconfiguration), never on the unset path.
    pub fn get() -> Option<&'static Harness> {
        static HARNESS: OnceLock<Option<Harness>> = OnceLock::new();
        HARNESS.get_or_init(Harness::init).as_ref()
    }

    fn init() -> Option<Harness> {
        let config = match ManifestConfig::from_env() {
            Ok(config) => config?,
            Err(error) => panic!("invalid EOS_E2E_RUN_ROOT run-manifest.json: {error:#}"),
        };
        if let Err(error) =
            gateway::await_ready(&config.gateway_socket, gateway::DEFAULT_READY_TIMEOUT)
        {
            panic!("gateway not ready: {error:#}");
        }
        let cli = CliClient::new(PathBuf::from(CLI_BIN), config.gateway_socket);
        Some(Harness {
            cli,
            run_root: config.run_root,
            run_id: config.run_id,
            image: config.image,
        })
    }

    #[must_use]
    pub fn cli(&self) -> &CliClient {
        &self.cli
    }

    /// The run root that owns this run's `reports/` tree. Exposed so
    /// `Sandbox::drop` can resolve `reports/{id}/exchange.jsonl` without copying a
    /// `PathBuf` onto every `Sandbox`.
    #[must_use]
    pub fn run_root(&self) -> &Path {
        &self.run_root
    }

    /// Provision via the public manager CLI — the same path as the system under
    /// test. Creates `{run_root}/work/{run_id}-{slug}` as an absolute dir, then
    /// `sandbox-cli manager create_sandbox --image {image} --workspace-root {ws}`.
    /// The id is read from the create response `/id` (runtime-assigned,
    /// round-tripped), never predicted. Returns the RAII `Sandbox` and the create
    /// `CallRecord` so a leaf asserts on the one creation it made — no second
    /// `create_sandbox` is ever issued.
    pub fn provision_sandbox(&self, slug: &str, image: Option<&str>) -> (Sandbox, CallRecord) {
        let image = image.unwrap_or(&self.image);
        let workspace_root = self
            .run_root
            .join("work")
            .join(format!("{}-{slug}", self.run_id));
        std::fs::create_dir_all(&workspace_root).unwrap_or_else(|error| {
            panic!(
                "failed to create workspace root {}: {error}",
                workspace_root.display()
            )
        });
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|error| panic!("failed to canonicalize workspace root: {error}"));
        let workspace_root_arg = workspace_root.to_string_lossy().into_owned();

        let record = self.cli.manager(
            "create_sandbox",
            &["--image", image, "--workspace-root", &workspace_root_arg],
        );
        let resp = record.response();
        assertion::ok(resp);
        let id = assertion::field(resp, "/id")
            .as_str()
            .expect("create_sandbox response /id is a string")
            .to_owned();

        let test_name = std::thread::current()
            .name()
            .map(str::to_owned)
            .unwrap_or_else(|| slug.to_owned());
        let sandbox = Sandbox {
            id,
            workspace_root,
            started: Instant::now(),
            test_name,
            exchange: RefCell::new(vec![record.clone()]),
        };
        (sandbox, record)
    }
}

/// RAII sandbox handle. On drop, flushes this sandbox's `exchange.jsonl`, then
/// issues `sandbox-cli manager destroy_sandbox --sandbox-id {id}` (idempotent),
/// making teardown panic-safe even when an assertion fails. The exchange buffer
/// is seeded with the create record and grows via `record`.
pub struct Sandbox {
    pub id: String,
    pub workspace_root: PathBuf,
    started: Instant,
    test_name: String,
    exchange: RefCell<Vec<CallRecord>>,
}

impl Sandbox {
    /// Append a leaf-issued call to this sandbox's exchange buffer; the buffer
    /// flushes to `reports/{id}/exchange.jsonl` on drop. Recording is additive —
    /// the caller keeps `rec` and still reads `rec.response()` or the assertion
    /// helpers on it.
    pub fn record(&self, rec: &CallRecord) {
        self.exchange.borrow_mut().push(rec.clone());
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(harness) = Harness::get() {
            let records = self.exchange.borrow();
            let _ = report::write_exchange(harness.run_root(), &self.id, &records);

            let panicking = std::thread::panicking();
            let outcome = report::TestOutcome {
                schema_version: report::RESULT_SCHEMA_VERSION,
                test_name: self.test_name.clone(),
                sandbox_id: self.id.clone(),
                status: if panicking { "failed" } else { "passed" }.to_owned(),
                duration_ms: self.started.elapsed().as_millis(),
                workspace_root: self.workspace_root.to_string_lossy().into_owned(),
                assertions: report::Assertions {
                    total: assertion::assertion_count(),
                    failed: u64::from(panicking),
                },
                failure: panicking.then(|| "assertion panicked".to_owned()),
            };
            let _ = report::write_result(harness.run_root(), &outcome);

            let _ = harness
                .cli()
                .manager("destroy_sandbox", &["--sandbox-id", &self.id]);
        }
    }
}
