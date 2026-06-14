//! `xtask`: build and release tooling for the workspace.
//!
//! Invariant: dev-only tooling, never linked into the runtime. `anyhow` is
//! allowed here (binary). Runtime crates must stay free of this packaging code.
#![forbid(unsafe_code)]

use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

const AMD64_TARGET: &str = "x86_64-unknown-linux-musl";
const ARM64_TARGET: &str = "aarch64-unknown-linux-musl";

fn main() -> Result<()> {
    let mut args = env::args_os();
    let _argv0 = args.next();
    match args
        .next()
        .and_then(|arg| arg.into_string().ok())
        .as_deref()
    {
        Some("package") => package(&PackageArgs::parse(args)?),
        Some("check-contract") => check_contract(),
        Some("gen-docs") => gen_docs(),
        Some("help" | "--help" | "-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => bail!("unknown xtask command {other:?}; run `cargo run -p xtask -- help`"),
    }
}

/// Regenerate `docs/API.md` from the committed
/// `crates/daemon/operation/ops.json`.
fn gen_docs() -> Result<()> {
    let root = workspace_root()?;
    let rendered = render_api_doc(&root)?;
    let path = root.join("docs").join("API.md");
    fs::write(&path, rendered).with_context(|| format!("write {}", path.display()))?;
    println!("gen-docs: wrote docs/API.md");
    Ok(())
}

/// Render the API doc deterministically from
/// `crates/daemon/operation/ops.json`.
fn render_api_doc(root: &Path) -> Result<String> {
    let ops_path = op_catalog_path(root);
    let document: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&ops_path).with_context(|| format!("read {}", ops_path.display()))?,
    )
    .context("parse crates/daemon/operation/ops.json")?;
    let protocol_version = document
        .get("protocol_version")
        .and_then(serde_json::Value::as_i64)
        .context("ops.json missing protocol_version")?;
    let ops = document
        .get("ops")
        .and_then(serde_json::Value::as_array)
        .context("ops.json missing ops array")?;

    let field = |op: &serde_json::Value, key: &str| -> Result<String> {
        Ok(op
            .get(key)
            .and_then(serde_json::Value::as_str)
            .with_context(|| format!("catalog op missing {key}"))?
            .to_owned())
    };
    let mut sections: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for op in ops {
        let name = field(op, "name")?;
        let visibility = field(op, "visibility")?;
        let served_by = field(op, "served_by")?;
        let family = field(op, "family")?;
        let args_schema = field(op, "args_schema")?;
        let response_schema = field(op, "response_schema")?;
        let summary = field(op, "summary")?;
        let mutates = op
            .get("mutates_state")
            .and_then(serde_json::Value::as_bool)
            .context("catalog op missing mutates_state")?;
        let key = match visibility.as_str() {
            "public" => "public",
            "operator" => "operator",
            "internal" => "internal",
            "test" => "test",
            other => bail!("unknown visibility {other:?}"),
        };
        sections.entry(key).or_default().push(format!(
            "| `{name}` | {served_by} | {family} | {} | `{args_schema}` | `{response_schema}` | {summary} |",
            if mutates { "yes" } else { "no" }
        ));
    }

    let mut body = String::new();
    body.push_str("# Sandbox API — op catalog\n\n");
    body.push_str(
        "GENERATED from `crates/daemon/operation/ops.json` by `cargo run -p xtask -- gen-docs`.\n\
         Do not edit by hand: `cargo run -p xtask -- check-contract` fails when\n\
         this file drifts from the committed catalog.\n\n",
    );
    let _ = writeln!(&mut body, "Protocol version: **{protocol_version}**\n");
    for (key, title, blurb) in [
        (
            "public",
            "Public ops (client socket)",
            "The complete public vocabulary served on the `gateway` client socket.",
        ),
        (
            "operator",
            "Operator ops (operator socket)",
            "Served only on the operator socket beside the client socket; never the client socket.",
        ),
        (
            "internal",
            "Internal ops",
            "Reserved for the host recovery machine; not served from any socket.",
        ),
        (
            "test",
            "Test ops",
            "Daemon-side test hooks; refused by `gateway` and exercised only by direct-daemon test harnesses.",
        ),
    ] {
        let Some(rows) = sections.get(key) else {
            continue;
        };
        let _ = writeln!(&mut body, "## {title}\n\n{blurb}\n");
        body.push_str("| Op | Served by | Family | Mutates | Args DTO | Response DTO | Summary |\n");
        body.push_str("|---|---|---|---|---|---|---|\n");
        for row in rows {
            body.push_str(row);
            body.push('\n');
        }
        body.push('\n');
    }
    body.push_str(
        "## Plugin providers\n\nFirst-party plugin providers are static catalog entries under \
         `sandbox.plugin.*`. The initial provider is `sandbox.plugin.pyright_lsp.*`; dynamic \
         plugin-op forwarding is not part of the public API.\n",
    );
    Ok(body)
}

/// The CI drift gate for the sandbox contract:
/// 1. `eosd dump-ops` must equal the committed
///    `crates/daemon/operation/ops.json`.
/// 2. Name integrity: canonical names unique.
/// 3. Both sides' conformance test suites pass against owner-local fixtures.
fn check_contract() -> Result<()> {
    let root = workspace_root()?;

    let committed_path = op_catalog_path(&root);
    let committed = fs::read_to_string(&committed_path)
        .with_context(|| format!("read {}", committed_path.display()))?;
    let generated = capture_stdout(
        &root,
        &["run", "--quiet", "-p", "eosd", "--", "dump-ops"],
        "eosd dump-ops",
    )?;
    if committed != generated {
        bail!(
            "crates/daemon/operation/ops.json is stale: regenerate with \
             `cargo run -p eosd -- dump-ops > crates/daemon/operation/ops.json`"
        );
    }
    check_name_integrity(&committed)?;

    let api_doc_path = root.join("docs").join("API.md");
    let committed_doc = fs::read_to_string(&api_doc_path)
        .with_context(|| format!("read {}", api_doc_path.display()))?;
    if committed_doc != render_api_doc(&root)? {
        bail!("docs/API.md is stale: regenerate with `cargo run -p xtask -- gen-docs`");
    }
    check_live_docs_do_not_teach_stale_terms(&root)?;

    for suite in CONFORMANCE_SUITES {
        check_conformance_tests_are_present(&root, suite)?;
        let mut cargo_args = vec![
            "test".to_owned(),
            "--quiet".to_owned(),
            "-p".to_owned(),
            suite.package.to_owned(),
        ];
        push_feature_args(&mut cargo_args, suite.features);
        for test in suite.tests {
            cargo_args.extend(["--test".to_owned(), (*test).to_owned()]);
        }
        run_cargo(&root, &cargo_args, suite.package)?;
    }

    println!("check-contract: ops.json in sync, names sound, docs clean, conformance suites green");
    Ok(())
}

/// Conformance suites the gate runs, host side and box side. Extend this list
/// as contract tests land in new crates.
struct ConformanceSuite {
    package: &'static str,
    tests: &'static [&'static str],
    features: &'static [&'static str],
}

const CONFORMANCE_SUITES: &[ConformanceSuite] = &[
    // Box side: daemon wire-message conformance + the 18 golden CAS cases.
    ConformanceSuite {
        package: "daemon",
        tests: &["contract"],
        features: &[],
    },
    ConformanceSuite {
        package: "layerstack",
        tests: &["cas_fixtures"],
        features: &[],
    },
    // Host side: request-fixture encoding + router/visibility coverage.
    ConformanceSuite {
        package: "host",
        tests: &["contract"],
        features: &["e2e-support"],
    },
    ConformanceSuite {
        package: "gateway",
        tests: &[],
        features: &[],
    },
];

fn check_conformance_tests_are_present(root: &Path, suite: &ConformanceSuite) -> Result<()> {
    for test in suite.tests {
        let mut cargo_args = vec![
            "test".to_owned(),
            "--quiet".to_owned(),
            "-p".to_owned(),
            suite.package.to_owned(),
        ];
        push_feature_args(&mut cargo_args, suite.features);
        cargo_args.extend([
            "--test".to_owned(),
            (*test).to_owned(),
            "--".to_owned(),
            "--list".to_owned(),
        ]);
        let label = format!("{} --test {test} -- --list", suite.package);
        let stdout = capture_stdout(root, &cargo_args, &label)?;
        let test_count = listed_test_count(&stdout);
        if test_count == 0 {
            bail!("conformance suite {label} listed zero tests");
        }
    }
    Ok(())
}

fn push_feature_args(cargo_args: &mut Vec<String>, features: &[&str]) {
    if !features.is_empty() {
        cargo_args.extend(["--features".to_owned(), features.join(",")]);
    }
}

fn listed_test_count(stdout: &str) -> usize {
    stdout
        .lines()
        .filter(|line| line.trim_end().ends_with(": test"))
        .count()
}

fn check_name_integrity(ops_json: &str) -> Result<()> {
    let document: serde_json::Value =
        serde_json::from_str(ops_json).context("parse crates/daemon/operation/ops.json")?;
    let ops = document
        .get("ops")
        .and_then(serde_json::Value::as_array)
        .context("crates/daemon/operation/ops.json must carry an `ops` array")?;

    let mut names = std::collections::BTreeSet::new();
    for op in ops {
        let name = op
            .get("name")
            .and_then(serde_json::Value::as_str)
            .context("catalog op missing `name`")?;
        if !names.insert(name) {
            bail!("canonical name claimed twice in crates/daemon/operation/ops.json: {name}");
        }
        let served_by = op
            .get("served_by")
            .and_then(serde_json::Value::as_str)
            .with_context(|| format!("catalog op {name} missing `served_by`"))?;
        match served_by {
            "host" => {
                let host_verb = op
                    .get("host_verb")
                    .and_then(serde_json::Value::as_str)
                    .with_context(|| format!("host catalog op {name} missing `host_verb`"))?;
                if host_verb.trim().is_empty() {
                    bail!("host catalog op {name} has empty `host_verb`");
                }
            }
            "daemon" => {
                if !op.get("host_verb").is_some_and(serde_json::Value::is_null) {
                    bail!("daemon catalog op {name} must set `host_verb` to null");
                }
            }
            other => bail!("catalog op {name} has unknown `served_by` {other:?}"),
        }
        for key in ["args_schema", "response_schema"] {
            let value = op
                .get(key)
                .and_then(serde_json::Value::as_str)
                .with_context(|| format!("catalog op {name} missing `{key}`"))?;
            if value.trim().is_empty() {
                bail!("catalog op {name} has empty `{key}`");
            }
        }
    }
    Ok(())
}

const STALE_LIVE_DOC_TERMS: &[&str] = &[
    "api.v1",
    "api.runtime",
    "api.audit",
    "meta.protocol_version",
    "command-session",
    "command_session",
    "CommandSession",
    "sandbox.audit",
    "eos-protocol",
    "backend/src/eos-sandbox",
];

const HISTORICAL_DOC_PATHS: &[&str] = &[
    "docs/SPEC-operation-core.md",
    "docs/SPEC-operation-unified-op-io.md",
    "docs/SPEC-operation-v3-unified-contract.md",
    "docs/command-naming-update-note.md",
    "docs/sandbox-bridge-findings.md",
    "docs/sandbox-bridge-issues.md",
    "docs/sandbox-bridge-spec.md",
    "docs/sandbox-crates-code-review.md",
    "docs/sandbox-crates-refactor-plan.md",
    "docs/sandbox-event-tracing-response-plan.md",
    "docs/sandbox-response-observability-findings.md",
    "improvement.spec.md",
];

const HISTORICAL_DOC_PREFIXES: &[&str] = &["docs/contract/"];

fn check_live_docs_do_not_teach_stale_terms(root: &Path) -> Result<()> {
    let mut docs = Vec::new();
    collect_markdown_docs(&root.join("docs"), &mut docs)?;
    collect_root_markdown_docs(root, &mut docs)?;
    docs.sort();

    let mut violations = Vec::new();
    for path in docs {
        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("strip workspace root from {}", path.display()))?;
        let relative = relative
            .to_str()
            .with_context(|| format!("doc path is not valid UTF-8: {}", path.display()))?;
        if is_historical_doc(relative) {
            continue;
        }

        let body = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        for (line_index, line) in body.lines().enumerate() {
            for term in STALE_LIVE_DOC_TERMS {
                if line.contains(term) {
                    violations.push(format!("{relative}:{} contains {term:?}", line_index + 1));
                }
            }
        }
    }

    if !violations.is_empty() {
        violations.sort();
        bail!(
            "live docs contain stale sandbox vocabulary:\n{}",
            violations.join("\n")
        );
    }
    Ok(())
}

fn collect_root_markdown_docs(root: &Path, docs: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", root.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("read file type for {}", path.display()))?;
        if file_type.is_file() && path.extension().is_some_and(|extension| extension == "md") {
            docs.push(path);
        }
    }
    Ok(())
}

fn collect_markdown_docs(dir: &Path, docs: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("read file type for {}", path.display()))?;
        if file_type.is_dir() {
            collect_markdown_docs(&path, docs)?;
        } else if path.extension().is_some_and(|extension| extension == "md") {
            docs.push(path);
        }
    }
    Ok(())
}

fn is_historical_doc(relative: &str) -> bool {
    HISTORICAL_DOC_PATHS.contains(&relative)
        || HISTORICAL_DOC_PREFIXES
            .iter()
            .any(|prefix| relative.starts_with(prefix))
}

fn capture_stdout<S: AsRef<std::ffi::OsStr>>(
    root: &Path,
    cargo_args: &[S],
    what: &str,
) -> Result<String> {
    let output = Command::new("cargo")
        .args(cargo_args)
        .current_dir(root)
        .output()
        .with_context(|| format!("spawn {what}"))?;
    if !output.status.success() {
        bail!(
            "{what} failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).with_context(|| format!("{what} produced non-UTF-8 output"))
}

fn run_cargo<S: AsRef<std::ffi::OsStr>>(root: &Path, cargo_args: &[S], what: &str) -> Result<()> {
    let status = Command::new("cargo")
        .args(cargo_args)
        .current_dir(root)
        .status()
        .with_context(|| format!("spawn cargo for {what}"))?;
    if !status.success() {
        bail!("conformance suite failed for {what} with {status}");
    }
    Ok(())
}

#[derive(Debug)]
struct PackageArgs {
    target: String,
    out_dir: PathBuf,
    no_build: bool,
    builder: String,
    sign: bool,
    minisign_key: Option<PathBuf>,
}

impl PackageArgs {
    fn parse<I>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut target: Option<String> = None;
        let mut out_dir = PathBuf::from("dist");
        let mut no_build = false;
        let mut builder = env::var("EOS_XTASK_BUILDER").unwrap_or_else(|_| "rust-lld".to_owned());
        let mut sign = false;
        let mut minisign_key: Option<PathBuf> = None;

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            let arg = arg
                .into_string()
                .map_err(|_| anyhow::anyhow!("xtask arguments must be valid UTF-8"))?;
            match arg.as_str() {
                "--target" => target = Some(next_string(&mut iter, "--target")?),
                "--out-dir" => out_dir = PathBuf::from(next_string(&mut iter, "--out-dir")?),
                "--no-build" => no_build = true,
                "--builder" => builder = next_string(&mut iter, "--builder")?,
                "--sign" => sign = true,
                "--minisign-key" => {
                    minisign_key = Some(PathBuf::from(next_string(&mut iter, "--minisign-key")?));
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown package option {other:?}"),
            }
        }

        let target = target.unwrap_or_else(|| AMD64_TARGET.to_owned());
        arch_for_target(&target)?;
        Ok(Self {
            target,
            out_dir,
            no_build,
            builder,
            sign,
            minisign_key,
        })
    }
}

fn package(args: &PackageArgs) -> Result<()> {
    let root = workspace_root()?;
    let out_dir = absolutize(&root, &args.out_dir);
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("create artifact dir {}", out_dir.display()))?;

    if !args.no_build {
        run_build(&root, &args.builder, &args.target)?;
    }

    let arch = arch_for_target(&args.target)?;
    let built = cargo_target_dir(&root)
        .join(&args.target)
        .join("release")
        .join("eosd");
    let artifact_name = format!("eosd-linux-{arch}");
    let artifact = out_dir.join(&artifact_name);
    fs::copy(&built, &artifact)
        .with_context(|| format!("copy {} to {}", built.display(), artifact.display()))?;

    #[cfg(unix)]
    set_executable(&artifact)?;

    let sha = sha256_file(&artifact)?;
    let protocol_version = read_protocol_version(&root)?;
    write_protocol_version(&out_dir, protocol_version)?;
    write_checksums(&out_dir)?;
    write_manifest(
        &out_dir,
        &args.target,
        arch,
        &artifact_name,
        &sha,
        protocol_version,
    )?;

    if args.sign {
        let key = args
            .minisign_key
            .as_deref()
            .context("--sign requires --minisign-key <path>")?;
        sign_artifact(&artifact, key)?;
    }

    println!(
        "packaged {artifact_name} target={} sha256={} protocol_version={}",
        args.target, sha, protocol_version
    );
    Ok(())
}

fn next_string<I>(iter: &mut I, flag: &str) -> Result<String>
where
    I: Iterator<Item = OsString>,
{
    iter.next()
        .context(format!("{flag} requires a value"))?
        .into_string()
        .map_err(|_| anyhow::anyhow!("{flag} value must be valid UTF-8"))
}

fn workspace_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("xtask manifest directory has no parent")
}

fn op_catalog_path(root: &Path) -> PathBuf {
    root.join("crates")
        .join("daemon")
        .join("operation")
        .join("ops.json")
}

fn absolutize(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn cargo_target_dir(root: &Path) -> PathBuf {
    env::var_os("CARGO_TARGET_DIR").map_or_else(
        || root.join("target"),
        |target_dir| absolutize(root, Path::new(&target_dir)),
    )
}

fn arch_for_target(target: &str) -> Result<&'static str> {
    match target {
        AMD64_TARGET => Ok("amd64"),
        ARM64_TARGET => Ok("arm64"),
        _ => bail!(
            "unsupported release target {target:?}; expected {AMD64_TARGET} or {ARM64_TARGET}"
        ),
    }
}

fn run_build(root: &Path, builder: &str, target: &str) -> Result<()> {
    let mut command = match builder {
        "rust-lld" => {
            let mut command = cargo_build_command(target);
            command.env("RUSTFLAGS", rustflags_with_rust_lld());
            command
        }
        "cargo" => cargo_build_command(target),
        "cross" => {
            let mut command = Command::new("cross");
            command.args(["build", "--release", "-p", "eosd", "--target", target]);
            command
        }
        other => bail!("unsupported builder {other:?}; expected rust-lld, cargo, or cross"),
    };
    let status = command
        .current_dir(root)
        .status()
        .with_context(|| format!("spawn {builder} build"))?;
    if !status.success() {
        bail!("{builder} build failed for {target} with {status}");
    }
    Ok(())
}

fn cargo_build_command(target: &str) -> Command {
    let mut command = Command::new("cargo");
    command.args(["build", "--release", "-p", "eosd", "--target", target]);
    command
}

fn rustflags_with_rust_lld() -> String {
    let existing = env::var("RUSTFLAGS").unwrap_or_default();
    if existing
        .split_whitespace()
        .any(|flag| flag == "linker=rust-lld")
        || existing.contains("-C linker=rust-lld")
    {
        existing
    } else if existing.is_empty() {
        "-C linker=rust-lld".to_owned()
    } else {
        format!("{existing} -C linker=rust-lld")
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).with_context(|| format!("chmod 755 {}", path.display()))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn read_protocol_version(root: &Path) -> Result<i64> {
    let path = op_catalog_path(root);
    let document: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .context("parse crates/daemon/operation/ops.json")?;
    document
        .get("protocol_version")
        .and_then(serde_json::Value::as_i64)
        .context("crates/daemon/operation/ops.json missing protocol_version")
}

fn write_protocol_version(out_dir: &Path, protocol_version: i64) -> Result<()> {
    fs::write(
        out_dir.join("protocol_version"),
        format!("{protocol_version}\n"),
    )
    .with_context(|| format!("write {}", out_dir.join("protocol_version").display()))
}

fn write_checksums(out_dir: &Path) -> Result<()> {
    let mut artifacts = fs::read_dir(out_dir)
        .with_context(|| format!("read {}", out_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("read {}", out_dir.display()))?;
    artifacts.retain(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| matches!(name, "eosd-linux-amd64" | "eosd-linux-arm64"))
    });
    artifacts.sort();

    let mut body = String::new();
    for path in artifacts {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("artifact filename must be valid UTF-8")?;
        writeln!(&mut body, "{}  {name}", sha256_file(&path)?)
            .map_err(|_| anyhow::anyhow!("write checksum body"))?;
    }
    fs::write(out_dir.join("SHA256SUMS"), body)
        .with_context(|| format!("write {}", out_dir.join("SHA256SUMS").display()))
}

fn write_manifest(
    out_dir: &Path,
    target: &str,
    arch: &str,
    artifact_name: &str,
    sha256: &str,
    protocol_version: i64,
) -> Result<()> {
    let body = format!(
        concat!(
            "{{\n",
            "  \"artifact\": \"{}\",\n",
            "  \"arch\": \"{}\",\n",
            "  \"protocol_version\": {},\n",
            "  \"sha256\": \"{}\",\n",
            "  \"target\": \"{}\",\n",
            "  \"version\": \"{}\"\n",
            "}}\n"
        ),
        artifact_name,
        arch,
        protocol_version,
        sha256,
        target,
        env!("CARGO_PKG_VERSION"),
    );
    fs::write(out_dir.join(format!("{artifact_name}.json")), body).with_context(|| {
        format!(
            "write {}",
            out_dir.join(format!("{artifact_name}.json")).display()
        )
    })
}

fn sign_artifact(artifact: &Path, key: &Path) -> Result<()> {
    let signature = artifact.with_extension("minisig");
    let status = Command::new("minisign")
        .args(["-S", "-s"])
        .arg(key)
        .args(["-m"])
        .arg(artifact)
        .args(["-x"])
        .arg(&signature)
        .status()
        .with_context(|| "spawn minisign")?;
    if !status.success() {
        bail!("minisign failed for {} with {status}", artifact.display());
    }
    Ok(())
}

fn print_help() {
    println!(
        "\
xtask commands:
  package [--target <triple>] [--out-dir <dir>] [--builder rust-lld|cargo|cross] [--no-build]
          [--sign --minisign-key <path>]
  check-contract    verify crates/daemon/operation/ops.json matches `eosd dump-ops`, alias
                    integrity holds, docs/API.md is fresh, and the
                    conformance suites pass
  gen-docs          regenerate docs/API.md from crates/daemon/operation/ops.json

Targets:
  {AMD64_TARGET} -> eosd-linux-amd64
  {ARM64_TARGET} -> eosd-linux-arm64
"
    );
}
