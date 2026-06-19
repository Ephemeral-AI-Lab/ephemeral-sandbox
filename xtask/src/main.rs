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
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

const AMD64_TARGET: &str = "x86_64-unknown-linux-musl";
const ARM64_TARGET: &str = "aarch64-unknown-linux-musl";
const DEFAULT_PACKAGE_PROFILE: &str = "package-fast";
const FAST_PACKAGE_PROFILE: &str = "package-fast";
const MAX_MOD_OR_LIB_LINES: usize = 300;

fn main() -> Result<()> {
    let mut args = env::args_os();
    let _argv0 = args.next();
    match args
        .next()
        .and_then(|arg| arg.into_string().ok())
        .as_deref()
    {
        Some("package") => package(&PackageArgs::parse(args)?),
        Some("check-mod-lib-size") => {
            check_mod_lib_size_policy(&ModLibSizePolicyArgs::parse(args)?)
        }
        Some("check-inline-cfg-test" | "check-inline-tests") => {
            check_inline_test_policy(&InlineTestPolicyArgs::parse(args)?)
        }
        Some("help" | "--help" | "-h") | None => {
            print_help();
            Ok(())
        }
        Some(other) => bail!("unknown xtask command {other:?}; run `cargo run -p xtask -- help`"),
    }
}

#[derive(Debug)]
struct InlineTestPolicyArgs {
    roots: Vec<PathBuf>,
}

#[derive(Debug)]
struct ModLibSizePolicyArgs {
    roots: Vec<PathBuf>,
    max_lines: usize,
}

#[derive(Debug)]
struct ModLibSizePolicyViolation {
    path: PathBuf,
    line_count: usize,
}

#[derive(Debug)]
struct InlineTestPolicyViolation {
    path: PathBuf,
    line_number: usize,
    line: String,
    kind: InlineTestPolicyViolationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InlineTestPolicyViolationKind {
    AbiLinkageAttribute,
    BenchAttribute,
    BroadAllowAttribute,
    CfgTest,
    MacroUseAttribute,
    PathAttribute,
    TestSupportAttribute,
    TestAttribute,
}

#[derive(Debug)]
struct InlineTestPolicyException {
    path: &'static str,
    kind: InlineTestPolicyViolationKind,
}

const INLINE_TEST_POLICY_EXCEPTIONS: &[InlineTestPolicyException] = &[
    InlineTestPolicyException {
        path: "crates/daemon/core/src/trace/mod.rs",
        kind: InlineTestPolicyViolationKind::BroadAllowAttribute,
    },
    InlineTestPolicyException {
        path: "crates/daemon/core/src/trace/sidecar/build.rs",
        kind: InlineTestPolicyViolationKind::BroadAllowAttribute,
    },
    InlineTestPolicyException {
        path: "crates/daemon/core/src/trace/spool.rs",
        kind: InlineTestPolicyViolationKind::BroadAllowAttribute,
    },
    InlineTestPolicyException {
        path: "crates/daemon/layerstack/src/service.rs",
        kind: InlineTestPolicyViolationKind::BroadAllowAttribute,
    },
    InlineTestPolicyException {
        path: "crates/daemon/layerstack/src/service.rs",
        kind: InlineTestPolicyViolationKind::PathAttribute,
    },
    InlineTestPolicyException {
        path: "crates/daemon/namespace-process/src/holder/namespace.rs",
        kind: InlineTestPolicyViolationKind::BroadAllowAttribute,
    },
    InlineTestPolicyException {
        path: "crates/daemon/namespace-process/src/runner/setns.rs",
        kind: InlineTestPolicyViolationKind::BroadAllowAttribute,
    },
    InlineTestPolicyException {
        path: "crates/daemon/operation_service/src/command/mod.rs",
        kind: InlineTestPolicyViolationKind::PathAttribute,
    },
    InlineTestPolicyException {
        path: "crates/daemon/operation_service/src/workspace_session/mod.rs",
        kind: InlineTestPolicyViolationKind::PathAttribute,
    },
    InlineTestPolicyException {
        path: "crates/host/src/container.rs",
        kind: InlineTestPolicyViolationKind::BroadAllowAttribute,
    },
    InlineTestPolicyException {
        path: "crates/host/src/daemon_wire.rs",
        kind: InlineTestPolicyViolationKind::BroadAllowAttribute,
    },
    InlineTestPolicyException {
        path: "crates/host/src/trace_store/mod.rs",
        kind: InlineTestPolicyViolationKind::BroadAllowAttribute,
    },
];

impl InlineTestPolicyArgs {
    fn parse<I>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut roots = Vec::new();
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            let arg = arg
                .into_string()
                .map_err(|_| anyhow::anyhow!("xtask arguments must be valid UTF-8"))?;
            match arg.as_str() {
                "--root" => roots.push(PathBuf::from(next_string(&mut iter, "--root")?)),
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown inline test policy option {other:?}"),
            }
        }
        if roots.is_empty() {
            roots.extend([PathBuf::from("crates"), PathBuf::from("xtask")]);
        }
        Ok(Self { roots })
    }
}

impl ModLibSizePolicyArgs {
    fn parse<I>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut roots = Vec::new();
        let mut max_lines = MAX_MOD_OR_LIB_LINES;
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            let arg = arg
                .into_string()
                .map_err(|_| anyhow::anyhow!("xtask arguments must be valid UTF-8"))?;
            match arg.as_str() {
                "--root" => roots.push(PathBuf::from(next_string(&mut iter, "--root")?)),
                "--max-lines" => {
                    max_lines = next_string(&mut iter, "--max-lines")?
                        .parse()
                        .context("--max-lines must be a positive integer")?;
                    if max_lines == 0 {
                        bail!("--max-lines must be positive");
                    }
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown check-mod-lib-size option {other:?}"),
            }
        }
        if roots.is_empty() {
            roots.extend([PathBuf::from("crates"), PathBuf::from("xtask")]);
        }
        Ok(Self { roots, max_lines })
    }
}

#[derive(Debug)]
struct PackageArgs {
    target: String,
    out_dir: PathBuf,
    no_build: bool,
    builder: String,
    profile: String,
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
        let mut profile =
            env::var("EOS_XTASK_PROFILE").unwrap_or_else(|_| DEFAULT_PACKAGE_PROFILE.to_owned());
        let mut profile_from_arg = false;
        let mut fast = false;
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
                "--profile" => {
                    if fast {
                        bail!("--fast cannot be combined with --profile");
                    }
                    profile = next_string(&mut iter, "--profile")?;
                    profile_from_arg = true;
                }
                "--fast" => {
                    if profile_from_arg {
                        bail!("--fast cannot be combined with --profile");
                    }
                    fast = true;
                }
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
        if fast {
            profile = FAST_PACKAGE_PROFILE.to_owned();
        }
        arch_for_target(&target)?;
        validate_profile(&profile)?;
        Ok(Self {
            target,
            out_dir,
            no_build,
            builder,
            profile,
            sign,
            minisign_key,
        })
    }
}

fn check_inline_test_policy(args: &InlineTestPolicyArgs) -> Result<()> {
    let root = workspace_root()?;
    let mut violations = Vec::new();
    for scan_root in &args.roots {
        let scan_root = absolutize(&root, scan_root);
        for entry in WalkBuilder::new(&scan_root).build() {
            let entry = entry.with_context(|| format!("walk {}", scan_root.display()))?;
            let path = entry.path();
            if !entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
                || !is_rust_source(path)
                || is_under_crate_dir(path, "tests")
                || is_under_crate_dir(path, "benches")
            {
                continue;
            }
            collect_inline_test_policy_violations(&root, path, &mut violations)?;
        }
    }

    if violations.is_empty() {
        println!("no forbidden inline attributes found in production Rust sources");
        return Ok(());
    }

    eprintln!(
        "test, bench, broad lint-suppression, module path, macro_use, and ABI/linkage \
attributes are forbidden in production Rust sources unless explicitly allowlisted."
    );
    for violation in &violations {
        eprintln!(
            "{}:{}: {} ({})",
            relative_to(&root, &violation.path).display(),
            violation.line_number,
            violation.line.trim(),
            violation.kind.label()
        );
    }
    bail!(
        "found {} forbidden inline production attributes",
        violations.len()
    )
}

fn check_mod_lib_size_policy(args: &ModLibSizePolicyArgs) -> Result<()> {
    let root = workspace_root()?;
    let mut violations = Vec::new();
    for scan_root in &args.roots {
        let scan_root = absolutize(&root, scan_root);
        for entry in WalkBuilder::new(&scan_root).build() {
            let entry = entry.with_context(|| format!("walk {}", scan_root.display()))?;
            let path = entry.path();
            if !entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
                || !is_mod_or_lib_source(path)
                || is_under_crate_dir(path, "tests")
                || is_under_crate_dir(path, "benches")
            {
                continue;
            }
            let line_count = count_lines(path)?;
            if line_count > args.max_lines {
                violations.push(ModLibSizePolicyViolation {
                    path: path.to_path_buf(),
                    line_count,
                });
            }
        }
    }

    if violations.is_empty() {
        println!(
            "all mod.rs and lib.rs files are within {max_lines} lines",
            max_lines = args.max_lines
        );
        return Ok(());
    }

    eprintln!(
        "mod.rs and lib.rs files must be at most {} lines; move implementation \
into focused sibling modules and keep facades small.",
        args.max_lines
    );
    for violation in &violations {
        eprintln!(
            "{}: {} lines",
            relative_to(&root, &violation.path).display(),
            violation.line_count
        );
    }
    bail!("found {} oversized mod.rs/lib.rs files", violations.len())
}

fn collect_inline_test_policy_violations(
    root: &Path,
    path: &Path,
    violations: &mut Vec<InlineTestPolicyViolation>,
) -> Result<()> {
    let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    for (line_index, line) in body.lines().enumerate() {
        let Some(kind) = line_inline_test_policy_violation_kind(line) else {
            continue;
        };
        if !inline_test_policy_exception_allowed(root, path, &kind) {
            violations.push(InlineTestPolicyViolation {
                path: path.to_path_buf(),
                line_number: line_index + 1,
                line: line.to_owned(),
                kind,
            });
        }
    }
    Ok(())
}

fn line_inline_test_policy_violation_kind(line: &str) -> Option<InlineTestPolicyViolationKind> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return None;
    }
    let compact = trimmed
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    if compact.starts_with("#[cfg(test)]") || compact.starts_with("#![cfg(test)]") {
        Some(InlineTestPolicyViolationKind::CfgTest)
    } else {
        attribute_violation_kind(&compact)
    }
}

fn attribute_violation_kind(compact_line: &str) -> Option<InlineTestPolicyViolationKind> {
    let attribute = attribute_body(compact_line)?;
    let (path, args) = attribute_path_and_args(attribute);
    if is_test_attribute(path) {
        Some(InlineTestPolicyViolationKind::TestAttribute)
    } else if is_test_support_attribute(path) {
        Some(InlineTestPolicyViolationKind::TestSupportAttribute)
    } else if is_bench_attribute(path) {
        Some(InlineTestPolicyViolationKind::BenchAttribute)
    } else if is_broad_allow_attribute(path, args) {
        Some(InlineTestPolicyViolationKind::BroadAllowAttribute)
    } else if path == "path" {
        Some(InlineTestPolicyViolationKind::PathAttribute)
    } else if path == "macro_use" {
        Some(InlineTestPolicyViolationKind::MacroUseAttribute)
    } else if is_abi_linkage_attribute(path, args) {
        Some(InlineTestPolicyViolationKind::AbiLinkageAttribute)
    } else {
        None
    }
}

fn attribute_body(compact_line: &str) -> Option<&str> {
    let attribute = compact_line
        .strip_prefix("#![")
        .or_else(|| compact_line.strip_prefix("#["))?;
    attribute.strip_suffix(']').or_else(|| {
        attribute
            .split_once(']')
            .map(|(attribute, _rest)| attribute)
    })
}

fn attribute_path_and_args(attribute: &str) -> (&str, Option<&str>) {
    if let Some((path, args)) = attribute.split_once('(') {
        (path, args.strip_suffix(')'))
    } else if let Some((path, _value)) = attribute.split_once('=') {
        (path, None)
    } else {
        (attribute, None)
    }
}

fn is_test_attribute(path: &str) -> bool {
    path == "test" || path.ends_with("::test")
}

fn is_test_support_attribute(path: &str) -> bool {
    matches!(
        path,
        "should_panic" | "ignore" | "rstest" | "case" | "test_case" | "quickcheck" | "proptest"
    ) || path.ends_with("::rstest")
        || path.ends_with("::case")
        || path.ends_with("::test_case")
        || path.ends_with("::quickcheck")
        || path.ends_with("::proptest")
}

fn is_bench_attribute(path: &str) -> bool {
    path == "bench" || path.ends_with("::bench")
}

fn is_broad_allow_attribute(path: &str, args: Option<&str>) -> bool {
    if path != "allow" {
        return false;
    }
    args.is_some_and(|args| {
        args.split(',').any(|lint| {
            matches!(
                lint,
                "warnings" | "unused" | "dead_code" | "unused_imports" | "clippy::all"
            )
        })
    })
}

fn is_abi_linkage_attribute(path: &str, args: Option<&str>) -> bool {
    matches!(path, "no_mangle" | "export_name" | "link_section" | "naked")
        || (path == "repr" && args.is_some_and(|args| args.split(',').any(|arg| arg == "packed")))
}

fn inline_test_policy_exception_allowed(
    root: &Path,
    path: &Path,
    kind: &InlineTestPolicyViolationKind,
) -> bool {
    let relative_path = relative_to(root, path);
    INLINE_TEST_POLICY_EXCEPTIONS
        .iter()
        .any(|exception| relative_path == Path::new(exception.path) && exception.kind == *kind)
}

impl InlineTestPolicyViolationKind {
    fn label(&self) -> &'static str {
        match self {
            Self::AbiLinkageAttribute => "ABI/linkage attribute",
            Self::BenchAttribute => "bench attribute",
            Self::BroadAllowAttribute => "broad lint suppression",
            Self::CfgTest => "inline cfg(test)",
            Self::MacroUseAttribute => "macro_use attribute",
            Self::PathAttribute => "path attribute",
            Self::TestSupportAttribute => "test support attribute",
            Self::TestAttribute => "inline test attribute",
        }
    }
}

fn is_rust_source(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "rs")
}

fn is_mod_or_lib_source(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "mod.rs" | "lib.rs"))
}

fn count_lines(path: &Path) -> Result<usize> {
    let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(body.lines().count())
}

fn is_under_crate_dir(path: &Path, dir_name: &str) -> bool {
    path.ancestors().any(|ancestor| {
        ancestor.file_name().is_some_and(|name| name == dir_name)
            && ancestor
                .parent()
                .is_some_and(|parent| parent.join("Cargo.toml").is_file())
    })
}

fn relative_to<'a>(root: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}

fn package(args: &PackageArgs) -> Result<()> {
    let root = workspace_root()?;
    let out_dir = absolutize(&root, &args.out_dir);
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("create artifact dir {}", out_dir.display()))?;

    if !args.no_build {
        run_build(&root, &args.builder, &args.target, &args.profile)?;
    }

    let arch = arch_for_target(&args.target)?;
    let built = cargo_target_dir(&root)
        .join(&args.target)
        .join(cargo_profile_dir(&args.profile))
        .join("eosd");
    let artifact_name = format!("eosd-linux-{arch}");
    let artifact = out_dir.join(&artifact_name);
    fs::copy(&built, &artifact)
        .with_context(|| format!("copy {} to {}", built.display(), artifact.display()))?;

    #[cfg(unix)]
    set_executable(&artifact)?;

    let sha = sha256_file(&artifact)?;
    write_checksums(&out_dir)?;
    write_manifest(&out_dir, &args.target, arch, &artifact_name, &sha)?;

    if args.sign {
        let key = args
            .minisign_key
            .as_deref()
            .context("--sign requires --minisign-key <path>")?;
        sign_artifact(&artifact, key)?;
    }

    println!(
        "packaged {artifact_name} target={} profile={} sha256={}",
        args.target, args.profile, sha
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

fn validate_profile(profile: &str) -> Result<()> {
    if profile.is_empty() || profile.contains(['/', '\\']) {
        bail!("invalid package profile {profile:?}");
    }
    Ok(())
}

fn cargo_profile_dir(profile: &str) -> &str {
    match profile {
        "dev" => "debug",
        "release" => "release",
        other => other,
    }
}

fn run_build(root: &Path, builder: &str, target: &str, profile: &str) -> Result<()> {
    let mut command = match builder {
        "rust-lld" => {
            let mut command = cargo_build_command(target, profile);
            command.env("RUSTFLAGS", rustflags_with_rust_lld());
            command
        }
        "cargo" => cargo_build_command(target, profile),
        "cross" => {
            let mut command = Command::new("cross");
            command.args([
                "build",
                "-p",
                "eosd",
                "--target",
                target,
                "--profile",
                profile,
            ]);
            command
        }
        other => bail!("unsupported builder {other:?}; expected rust-lld, cargo, or cross"),
    };
    let status = command
        .current_dir(root)
        .status()
        .with_context(|| format!("spawn {builder} build"))?;
    if !status.success() {
        bail!("{builder} build failed for {target} profile {profile} with {status}");
    }
    Ok(())
}

fn cargo_build_command(target: &str, profile: &str) -> Command {
    let mut command = Command::new("cargo");
    command.args([
        "build",
        "-p",
        "eosd",
        "--target",
        target,
        "--profile",
        profile,
    ]);
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
) -> Result<()> {
    let body = format!(
        concat!(
            "{{\n",
            "  \"artifact\": \"{}\",\n",
            "  \"arch\": \"{}\",\n",
            "  \"sha256\": \"{}\",\n",
            "  \"target\": \"{}\",\n",
            "  \"version\": \"{}\"\n",
            "}}\n"
        ),
        artifact_name,
        arch,
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
  check-mod-lib-size [--root <path> ...] [--max-lines <n>]
          fail if any mod.rs or lib.rs file exceeds 300 lines by default
  check-inline-tests [--root <path> ...]
          fail if production Rust sources contain forbidden inline attributes
          alias: check-inline-cfg-test
  package [--target <triple>] [--out-dir <dir>] [--builder rust-lld|cargo|cross]
          [--profile <name> | --fast] [--no-build] [--sign --minisign-key <path>]

Targets:
  {AMD64_TARGET} -> eosd-linux-amd64
  {ARM64_TARGET} -> eosd-linux-arm64

Profiles:
  package-fast  default local Docker/E2E package, no LTO + incremental rebuilds
  release       final perf artifact, fat LTO, slowest local rebuilds
"
    );
}
