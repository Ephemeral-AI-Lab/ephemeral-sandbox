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
use xtask::operation_architecture;
use xtask::package::{daemon_build_arguments, isolated_package_target_dir};

const AMD64_TARGET: &str = "x86_64-unknown-linux-musl";
const ARM64_TARGET: &str = "aarch64-unknown-linux-musl";
const DAEMON_BINARY: &str = "sandbox-daemon";
const DAEMON_ARTIFACT_PREFIX: &str = "sandbox-daemon";
const DEFAULT_PACKAGE_PROFILE: &str = "package-fast";
const FAST_PACKAGE_PROFILE: &str = "package-fast";
const DEFAULT_BUILDER: &str = "auto";
const MAX_MOD_OR_LIB_LINES: usize = 300;
const MAX_CRATE_SRC_LINES: usize = 1_000;

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
        Some("check-crate-source-size") => {
            check_crate_source_size_policy(&CrateSourceSizePolicyArgs::parse(args)?)
        }
        Some("check-inline-tests") => check_inline_test_policy(&InlineTestPolicyArgs::parse(args)?),
        Some("check-cfg") => check_cfg_policy(&CfgPolicyArgs::parse(args)?),
        Some("check-test-support") => {
            check_test_support_policy(&TestSupportPolicyArgs::parse(args)?)
        }
        Some("operation-architecture-check") => operation_architecture::check(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .context("xtask must be inside the workspace root")?,
        ),
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
struct CfgPolicyArgs {
    roots: Vec<PathBuf>,
}

#[derive(Debug)]
struct TestSupportPolicyArgs {
    roots: Vec<PathBuf>,
}

#[derive(Debug)]
struct ModLibSizePolicyArgs {
    roots: Vec<PathBuf>,
    max_lines: usize,
}

#[derive(Debug)]
struct CrateSourceSizePolicyArgs {
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

#[derive(Debug)]
struct CfgPolicyViolation {
    path: PathBuf,
    line_number: usize,
    line: String,
}

#[derive(Debug)]
struct TestSupportPolicyViolation {
    path: PathBuf,
    line_number: usize,
    line: String,
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

impl CfgPolicyArgs {
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
                other => bail!("unknown check-cfg option {other:?}"),
            }
        }
        if roots.is_empty() {
            roots.push(PathBuf::from("crates/sandbox-daemon"));
        }
        Ok(Self { roots })
    }
}

impl TestSupportPolicyArgs {
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
                other => bail!("unknown check-test-support option {other:?}"),
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

impl CrateSourceSizePolicyArgs {
    fn parse<I>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut roots = Vec::new();
        let mut max_lines = MAX_CRATE_SRC_LINES;
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
                other => bail!("unknown check-crate-source-size option {other:?}"),
            }
        }
        if roots.is_empty() {
            roots.push(PathBuf::from("crates"));
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
        let mut builder =
            env::var("SANDBOX_XTASK_BUILDER").unwrap_or_else(|_| DEFAULT_BUILDER.to_owned());
        let mut profile = env::var("SANDBOX_XTASK_PROFILE")
            .unwrap_or_else(|_| DEFAULT_PACKAGE_PROFILE.to_owned());
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
            collect_inline_test_policy_violations(path, &mut violations)?;
        }
    }

    if violations.is_empty() {
        println!("no forbidden inline attributes found in production Rust sources");
        return Ok(());
    }

    eprintln!(
        "test, bench, broad lint-suppression, module path, macro_use, and ABI/linkage \
attributes are forbidden in production Rust sources."
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

fn check_cfg_policy(args: &CfgPolicyArgs) -> Result<()> {
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
            collect_cfg_policy_violations(path, &mut violations)?;
        }
    }

    if violations.is_empty() {
        println!("no #[cfg]/#[cfg_attr] attributes found in scanned production Rust sources");
        return Ok(());
    }

    eprintln!(
        "conditional-compilation attributes (#[cfg]/#[cfg_attr]) are forbidden in scanned \
production Rust sources; keep platform- and feature-specific code out of src."
    );
    for violation in &violations {
        eprintln!(
            "{}:{}: {}",
            relative_to(&root, &violation.path).display(),
            violation.line_number,
            violation.line.trim(),
        );
    }
    bail!("found {} forbidden #[cfg] attributes", violations.len())
}

fn check_test_support_policy(args: &TestSupportPolicyArgs) -> Result<()> {
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
                || !is_under_crate_src(path)
            {
                continue;
            }
            collect_test_support_policy_violations(path, &mut violations)?;
        }
    }

    if violations.is_empty() {
        println!("no test-support feature gates found in crate src Rust files");
        return Ok(());
    }

    eprintln!(
        "test-support feature gates are forbidden in crate src/ Rust files; move \
test-only code into crate-root tests/ suites."
    );
    for violation in &violations {
        eprintln!(
            "{}:{}: {}",
            relative_to(&root, &violation.path).display(),
            violation.line_number,
            violation.line.trim(),
        );
    }
    bail!(
        "found {} forbidden test-support feature gates",
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

fn check_crate_source_size_policy(args: &CrateSourceSizePolicyArgs) -> Result<()> {
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
                || !is_under_crate_src(path)
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
            "all crate src Rust files are within {max_lines} lines",
            max_lines = args.max_lines
        );
        return Ok(());
    }

    eprintln!(
        "Rust files under crate src/ directories must be at most {} lines; split large \
implementation files into focused sibling modules.",
        args.max_lines
    );
    for violation in &violations {
        eprintln!(
            "{}: {} lines",
            relative_to(&root, &violation.path).display(),
            violation.line_count
        );
    }
    bail!("found {} oversized crate source files", violations.len())
}

fn collect_inline_test_policy_violations(
    path: &Path,
    violations: &mut Vec<InlineTestPolicyViolation>,
) -> Result<()> {
    for_each_attribute(path, |line_number, raw_attribute, normalized_attribute| {
        if let Some(kind) = normalized_attribute_violation_kind(normalized_attribute) {
            violations.push(InlineTestPolicyViolation {
                path: path.to_path_buf(),
                line_number,
                line: raw_attribute.to_owned(),
                kind,
            });
        }
    })
}

fn collect_cfg_policy_violations(
    path: &Path,
    violations: &mut Vec<CfgPolicyViolation>,
) -> Result<()> {
    for_each_attribute(path, |line_number, raw_attribute, normalized_attribute| {
        if normalized_attribute_is_cfg(normalized_attribute) {
            violations.push(CfgPolicyViolation {
                path: path.to_path_buf(),
                line_number,
                line: raw_attribute.to_owned(),
            });
        }
    })
}

fn collect_test_support_policy_violations(
    path: &Path,
    violations: &mut Vec<TestSupportPolicyViolation>,
) -> Result<()> {
    for_each_attribute(path, |line_number, raw_attribute, normalized_attribute| {
        if normalized_attribute_is_test_support_gate(normalized_attribute) {
            violations.push(TestSupportPolicyViolation {
                path: path.to_path_buf(),
                line_number,
                line: raw_attribute.to_owned(),
            });
        }
    })
}

fn for_each_attribute<F>(path: &Path, mut visit: F) -> Result<()>
where
    F: FnMut(usize, &str, &str),
{
    let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut pending_attribute: Option<(usize, String, String)> = None;
    for (line_index, line) in body.lines().enumerate() {
        if let Some((start_line, raw_attribute, normalized_attribute)) = &mut pending_attribute {
            raw_attribute.push(' ');
            raw_attribute.push_str(line.trim());
            normalized_attribute.push_str(&normalized_attribute_text(line));
            if normalized_attribute.contains(']') {
                visit(*start_line, raw_attribute, normalized_attribute);
                pending_attribute = None;
            }
            continue;
        }

        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || !is_attribute_start(trimmed) {
            continue;
        }
        let raw_attribute = trimmed.to_owned();
        let normalized_attribute = normalized_attribute_text(trimmed);
        if normalized_attribute.contains(']') {
            visit(line_index + 1, &raw_attribute, &normalized_attribute);
        } else {
            pending_attribute = Some((line_index + 1, raw_attribute, normalized_attribute));
        }
    }
    Ok(())
}

fn normalized_attribute_is_cfg(normalized: &str) -> bool {
    let Some(attribute) = attribute_body(normalized) else {
        return false;
    };
    let (path, _args) = attribute_path_and_args(attribute);
    path == "cfg" || path == "cfg_attr"
}

fn normalized_attribute_is_test_support_gate(normalized: &str) -> bool {
    if !normalized.contains("feature=\"test-support\"") {
        return false;
    }
    let Some(attribute) = attribute_body(normalized) else {
        return false;
    };
    let (path, _args) = attribute_path_and_args(attribute);
    path == "cfg" || path == "cfg_attr"
}

fn is_attribute_start(trimmed: &str) -> bool {
    trimmed.starts_with("#[") || trimmed.starts_with("#![")
}

fn normalized_attribute_text(line: &str) -> String {
    line.chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
}

fn normalized_attribute_violation_kind(normalized: &str) -> Option<InlineTestPolicyViolationKind> {
    if is_forbidden_cfg_test_attribute(normalized) {
        Some(InlineTestPolicyViolationKind::CfgTest)
    } else {
        attribute_violation_kind(normalized)
    }
}

fn is_forbidden_cfg_test_attribute(normalized: &str) -> bool {
    let Some(attribute) = attribute_body(normalized) else {
        return false;
    };
    let (path, args) = attribute_path_and_args(attribute);
    path == "cfg" && args.is_some_and(cfg_predicate_is_forbidden_test_gate)
}

fn attribute_violation_kind(normalized_line: &str) -> Option<InlineTestPolicyViolationKind> {
    let attribute = attribute_body(normalized_line)?;
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

fn attribute_body(normalized_line: &str) -> Option<&str> {
    let attribute = normalized_line
        .strip_prefix("#![")
        .or_else(|| normalized_line.strip_prefix("#["))?;
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
    match path {
        "allow" => args.is_some_and(allow_args_contain_forbidden_lint),
        "cfg_attr" => args.is_some_and(cfg_attr_args_contain_forbidden_allow),
        _ => false,
    }
}

fn cfg_attr_args_contain_forbidden_allow(args: &str) -> bool {
    if cfg_attr_test_gate_contains_allow(args) {
        return true;
    }

    let mut rest = args;
    while let Some(index) = rest.find("allow(") {
        let allow_args = &rest[index + "allow(".len()..];
        let Some(end) = matching_close_paren_index(allow_args) else {
            return false;
        };
        if allow_args_contain_forbidden_lint(&allow_args[..end]) {
            return true;
        }
        rest = &allow_args[end..];
    }
    false
}

fn cfg_attr_test_gate_contains_allow(args: &str) -> bool {
    let Some(condition_end) = top_level_comma_index(args) else {
        return false;
    };
    let condition = &args[..condition_end];
    let attributes = &args[condition_end + 1..];
    cfg_predicate_is_forbidden_test_gate(condition) && cfg_attr_attributes_contain_allow(attributes)
}

fn cfg_attr_attributes_contain_allow(attributes: &str) -> bool {
    split_top_level_commas(attributes)
        .map(attribute_path_and_args)
        .any(|(path, _args)| path == "allow")
}

fn cfg_predicate_is_forbidden_test_gate(predicate: &str) -> bool {
    predicate == "test" || cfg_predicate_is_test_or_feature_gate(predicate)
}

fn cfg_predicate_is_test_or_feature_gate(predicate: &str) -> bool {
    let Some(args) = predicate
        .strip_prefix("any(")
        .and_then(|args| args.strip_suffix(')'))
    else {
        return false;
    };

    let mut has_test = false;
    let mut has_feature = false;
    for arg in split_top_level_commas(args) {
        has_test |= arg == "test";
        has_feature |= arg.starts_with("feature=");
    }
    has_test && has_feature
}

fn split_top_level_commas(mut args: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if args.is_empty() {
            return None;
        }
        if let Some(index) = top_level_comma_index(args) {
            let arg = &args[..index];
            args = &args[index + 1..];
            Some(arg)
        } else {
            let arg = args;
            args = "";
            Some(arg)
        }
    })
}

fn top_level_comma_index(args: &str) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, ch) in args.char_indices() {
        match ch {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

fn matching_close_paren_index(args: &str) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, ch) in args.char_indices() {
        match ch {
            '(' => depth = depth.saturating_add(1),
            ')' if depth == 0 => return Some(index),
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

fn allow_args_contain_forbidden_lint(args: &str) -> bool {
    args.split(',').any(is_forbidden_allow_lint)
}

fn is_forbidden_allow_lint(lint: &str) -> bool {
    matches!(
        lint,
        "warnings"
            | "unused"
            | "unused_imports"
            | "unused_variables"
            | "unused_mut"
            | "unused_assignments"
            | "unused_must_use"
            | "dead_code"
            | "unreachable_code"
            | "unsafe_code"
            | "clippy::all"
            | "clippy::pedantic"
            | "clippy::nursery"
            | "clippy::restriction"
            | "clippy::unwrap_used"
            | "clippy::expect_used"
            | "clippy::panic"
            | "clippy::todo"
            | "clippy::unimplemented"
            | "clippy::dbg_macro"
    )
}

fn is_abi_linkage_attribute(path: &str, args: Option<&str>) -> bool {
    matches!(path, "no_mangle" | "export_name" | "link_section" | "naked")
        || (path == "repr" && args.is_some_and(|args| args.split(',').any(|arg| arg == "packed")))
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

fn is_under_crate_src(path: &Path) -> bool {
    path.ancestors().any(|ancestor| {
        ancestor.file_name().is_some_and(|name| name == "src")
            && ancestor
                .parent()
                .is_some_and(|parent| parent.join("Cargo.toml").is_file())
    })
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
    let package_target_dir = isolated_package_target_dir(&cargo_target_dir(&root));
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("create artifact dir {}", out_dir.display()))?;

    if !args.no_build {
        run_build(
            &root,
            &args.builder,
            &args.target,
            &args.profile,
            &package_target_dir,
        )?;
    }

    let arch = arch_for_target(&args.target)?;
    let built = package_target_dir
        .join(&args.target)
        .join(cargo_profile_dir(&args.profile))
        .join(DAEMON_BINARY);
    let artifact_name = format!("{DAEMON_ARTIFACT_PREFIX}-linux-{arch}");
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

fn run_build(
    root: &Path,
    builder: &str,
    target: &str,
    profile: &str,
    package_target_dir: &Path,
) -> Result<()> {
    let builder = Builder::resolve(builder)?;
    println!("using builder: {}", builder.name());
    let status = builder
        .build_command(target, profile)
        .current_dir(root)
        .env("CARGO_TARGET_DIR", package_target_dir)
        .status()
        .with_context(|| format!("spawn {} build", builder.name()))?;
    if !status.success() {
        bail!(
            "{} build failed for {target} profile {profile} with {status}",
            builder.name()
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Builder {
    Zigbuild,
    Cross,
    RustLld,
    Cargo,
}

impl Builder {
    fn resolve(requested: &str) -> Result<Self> {
        match requested {
            "auto" => Self::detect(),
            "zigbuild" => {
                if !Self::zigbuild_available() {
                    bail!(
                        "builder zigbuild needs cargo-zigbuild and zig on PATH; \
run bin/setup-musl-cross, or `cargo install --locked cargo-zigbuild` plus a zig install"
                    );
                }
                Ok(Self::Zigbuild)
            }
            "cross" => {
                if !command_exists("cross") {
                    bail!(
                        "builder cross needs the cross binary and a running Docker daemon; \
install it with `cargo install cross`"
                    );
                }
                Ok(Self::Cross)
            }
            "rust-lld" => Ok(Self::RustLld),
            "cargo" => Ok(Self::Cargo),
            other => bail!(
                "unsupported builder {other:?}; expected auto, zigbuild, cross, rust-lld, or cargo"
            ),
        }
    }

    fn detect() -> Result<Self> {
        if Self::zigbuild_available() {
            return Ok(Self::Zigbuild);
        }
        if command_exists("cross") {
            return Ok(Self::Cross);
        }
        bail!(
            "no musl cross builder found; run bin/setup-musl-cross to install \
zig + cargo-zigbuild (preferred), or `cargo install cross` to build via Docker, \
or force a host toolchain with --builder rust-lld"
        )
    }

    fn zigbuild_available() -> bool {
        command_exists("cargo-zigbuild") && command_exists("zig")
    }

    fn name(self) -> &'static str {
        match self {
            Self::Zigbuild => "zigbuild",
            Self::Cross => "cross",
            Self::RustLld => "rust-lld",
            Self::Cargo => "cargo",
        }
    }

    fn build_command(self, target: &str, profile: &str) -> Command {
        match self {
            Self::Zigbuild => {
                let mut command = Command::new("cargo");
                command.arg("zigbuild");
                command.args(daemon_build_arguments(target, profile));
                command
            }
            Self::Cross => {
                let mut command = Command::new("cross");
                command.arg("build");
                command.args(daemon_build_arguments(target, profile));
                command
            }
            Self::RustLld => {
                let mut command = cargo_build_command(target, profile);
                command.env("RUSTFLAGS", rustflags_with_rust_lld());
                configure_arm64_musl_cc(&mut command, target);
                command
            }
            Self::Cargo => {
                let mut command = cargo_build_command(target, profile);
                configure_arm64_musl_cc(&mut command, target);
                command
            }
        }
    }
}

fn configure_arm64_musl_cc(command: &mut Command, target: &str) {
    if target != ARM64_TARGET
        || env::var_os("CC_aarch64-unknown-linux-musl").is_some()
        || env::var_os("CC_aarch64_unknown_linux_musl").is_some()
        || command_exists("aarch64-linux-musl-gcc")
        || !command_exists("clang")
    {
        return;
    }
    command.env("CC_aarch64_unknown_linux_musl", "clang");
}

fn command_exists(name: &str) -> bool {
    Command::new(name).arg("--version").output().is_ok()
}

fn cargo_build_command(target: &str, profile: &str) -> Command {
    let mut command = Command::new("cargo");
    command.arg("build");
    command.args(daemon_build_arguments(target, profile));
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
            .is_some_and(is_primary_daemon_artifact)
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

fn is_primary_daemon_artifact(name: &str) -> bool {
    matches!(
        name,
        "sandbox-daemon-linux-amd64" | "sandbox-daemon-linux-arm64"
    )
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
  check-crate-source-size [--root <path> ...] [--max-lines <n>]
          fail if any Rust file under a crate src/ directory exceeds 1000 lines by default
  check-mod-lib-size [--root <path> ...] [--max-lines <n>]
          fail if any mod.rs or lib.rs file exceeds 300 lines by default
  check-inline-tests [--root <path> ...]
          fail if production Rust sources contain forbidden inline attributes
  check-cfg [--root <path> ...]
          fail if production Rust sources contain #[cfg]/#[cfg_attr] attributes
          (defaults to crates/sandbox-daemon)
  check-test-support [--root <path> ...]
          fail if crate src/ Rust files contain test-support feature gates
  operation-architecture-check
          enforce sandbox operation ownership, dependency, route, and stale-reference laws
  package [--target <triple>] [--out-dir <dir>]
          [--builder auto|zigbuild|cross|rust-lld|cargo]
          [--profile <name> | --fast] [--no-build] [--sign --minisign-key <path>]
Targets:
  {AMD64_TARGET} -> sandbox-daemon-linux-amd64
  {ARM64_TARGET} -> sandbox-daemon-linux-arm64

Builders (default auto; also settable via SANDBOX_XTASK_BUILDER):
  auto      zigbuild when cargo-zigbuild + zig are on PATH, else cross, else an
            error pointing at bin/setup-musl-cross
  zigbuild  cargo zigbuild; zig cc compiles and links C/asm deps against musl
  cross     Docker-based cross-rs toolchain image
  rust-lld  host cargo + rust-lld linker; needs a host C compiler with musl
            headers once C-backed deps are in the graph
  cargo     plain cargo build with the default linker

Profiles:
  package-fast  default local Docker/E2E package, no LTO + incremental rebuilds
  package-local fastest local Docker/E2E package, unoptimized + stripped
  release       final perf artifact, fat LTO, slowest local rebuilds
"
    );
}
