//! Immutable, server-owned MPLA fixtures used by the scorecard setup path.
//!
//! The profile is deliberately a closed set.  In particular, no caller can
//! supply a host path, a volume name, or an arbitrary cache key.  Consumers
//! attach only a small run-local ref/locator snapshot and receive a fresh
//! writable upper from the normal MPLA lifecycle.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::fs::File;

use crate::allocation::open_allocation;
use crate::durable::{read_json, write_immutable_json};
use crate::{
    AllocationId, CanonicalDurabilityReceipt, CanonicalRootPair, PocError, PocResult,
    ProjectionRecipe, SemanticBuildReceipt, SCHEMA_VERSION,
};

pub const PREPARED_FIXTURE_PROFILE: &str = "s4-chain-sparse-v1";
pub const PREPARED_FIXTURE_RUN_ID: &str = "fixture-s4-chain-sparse-v1";
pub const PREPARED_FIXTURE_ROOT: &str = "/eos/mpla-fixtures/s4-chain-sparse-v1";
pub const PREPARED_FIXTURE_MANIFEST: &str =
    "/eos/mpla-fixtures/s4-chain-sparse-v1/PREPARED-FIXTURE.json";
pub const PREPARED_FIXTURE_PAYLOAD_ROOT: &str =
    "/eos/mpla-fixtures/s4-chain-sparse-v1/layer-stack/mpla-poc/payload";
pub const PREPARED_FIXTURE_CONTROL_ROOT: &str =
    "/eos/mpla-fixtures/s4-chain-sparse-v1/workspace/mpla-poc/control";
pub const PREPARED_FIXTURE_CONTROL_SOURCE: &str =
    "/eos/mpla-fixtures/s4-chain-sparse-v1/control-source";

const PREPARED_FIXTURE_FORMAT: &str = "mpla-prepared-fixture-sparse-v1";
const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
pub const PREPARED_FIXTURE_CHAIN_DEPTH: u64 = 8;
pub const PREPARED_FIXTURE_SINGLE_FILE_LAYER_BYTES: u64 = GIB;
pub const PREPARED_FIXTURE_MARKER_LAYER_BYTES: u64 = GIB;
pub const PREPARED_FIXTURE_DEPTH_FIVE_BYTES: u64 = 5 * GIB;
pub const PREPARED_FIXTURE_DEPTH_EIGHT_BYTES: u64 = 8 * GIB;
pub const PREPARED_FIXTURE_CONTROL_SOURCE_BYTES: u64 = GIB + MIB;
pub const PREPARED_FIXTURE_BUILDER_HEADROOM_BYTES: u64 = 2 * GIB;
pub const PREPARED_FIXTURE_MINIMUM_AVAILABLE_INODES: u64 = 4 * 1024;
pub const PREPARED_FIXTURE_ALLOCATION_COUNT: u64 = PREPARED_FIXTURE_CHAIN_DEPTH;
pub const PREPARED_FIXTURE_BASE_SHA256: &str =
    "49bc20df15e412a64472421e13fe86ff1c5165e18b2afccf160d4dc19fe68a14";
pub const PREPARED_FIXTURE_LARGE_DELTA_SHA256: &str =
    "08bb1b6e5b7e3a4f832496b25276bcd39683aaf1e71946ccd4fb23118e0bc312";
pub const PREPARED_FIXTURE_SMALL_DELTA_SHA256: &str =
    "d22418d7c698f456bc6c0a9039ca9684b34f7958a2fda939718e1ba507dbb730";
pub const PREPARED_FIXTURE_CONTROL_SOURCE_MANIFEST_SHA256: &str =
    "6697bbde5ae3d748b54c087a4e06c08bd16ac64369afcec1c15233417b4a805e";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PreparedFixtureStorageRequirement {
    pub chain_bytes: u64,
    pub control_source_bytes: u64,
    pub working_headroom_bytes: u64,
    pub required_available_bytes: u64,
    pub minimum_available_inodes: u64,
}

pub fn prepared_fixture_storage_requirement() -> PocResult<PreparedFixtureStorageRequirement> {
    // All logical payload and control files in this closed profile are
    // hole-only sparse files. The logical byte contract is retained below,
    // while physical capacity admission reserves bounded metadata/workspace
    // headroom instead of demanding a dense second copy of the fixture.
    let required_available_bytes = PREPARED_FIXTURE_BUILDER_HEADROOM_BYTES;
    Ok(PreparedFixtureStorageRequirement {
        chain_bytes: PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
        control_source_bytes: PREPARED_FIXTURE_CONTROL_SOURCE_BYTES,
        working_headroom_bytes: PREPARED_FIXTURE_BUILDER_HEADROOM_BYTES,
        required_available_bytes,
        minimum_available_inodes: PREPARED_FIXTURE_MINIMUM_AVAILABLE_INODES,
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparedFixtureControlSource {
    pub base_sha256: String,
    pub delta_sha256: Vec<String>,
    pub source_manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparedFixtureBranch {
    pub branch: String,
    pub chain_depth: u64,
    pub accumulated_bytes: u64,
    pub roots: CanonicalRootPair,
    pub projection: ProjectionRecipe,
    pub canonical: CanonicalDurabilityReceipt,
    /// Semantic receipt chained from the holder-namespace-attested initial
    /// publication. This prevents a cache from being sealed using an unbound
    /// service-namespace scan.
    pub semantic: SemanticBuildReceipt,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparedFixtureManifest {
    pub schema_version: u32,
    pub format: String,
    pub profile: String,
    pub run_id: String,
    pub build_commit: String,
    pub control_source: PreparedFixtureControlSource,
    pub branches: Vec<PreparedFixtureBranch>,
}

impl PreparedFixtureManifest {
    pub fn new(
        build_commit: String,
        control_source: PreparedFixtureControlSource,
        branches: Vec<PreparedFixtureBranch>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            format: PREPARED_FIXTURE_FORMAT.to_owned(),
            profile: PREPARED_FIXTURE_PROFILE.to_owned(),
            run_id: PREPARED_FIXTURE_RUN_ID.to_owned(),
            build_commit,
            control_source,
            branches,
        }
    }

    pub fn validate(&self) -> PocResult<()> {
        self.validate_for_control_root(Path::new(PREPARED_FIXTURE_CONTROL_ROOT))
    }

    fn validate_for_control_root(&self, control_root: &Path) -> PocResult<()> {
        if self.schema_version != SCHEMA_VERSION
            || self.format != PREPARED_FIXTURE_FORMAT
            || self.profile != PREPARED_FIXTURE_PROFILE
            || self.run_id != PREPARED_FIXTURE_RUN_ID
        {
            return Err(PocError::Integrity(
                "prepared fixture has an unsupported identity".to_owned(),
            ));
        }
        if self.build_commit.len() != 40
            || !self
                .build_commit
                .as_bytes()
                .iter()
                .all(u8::is_ascii_hexdigit)
        {
            return Err(PocError::Integrity(
                "prepared fixture build commit is not a full Git SHA".to_owned(),
            ));
        }
        if self.control_source.base_sha256 != PREPARED_FIXTURE_BASE_SHA256 {
            return Err(PocError::Integrity(
                "prepared fixture base digest differs from the closed zero-content profile"
                    .to_owned(),
            ));
        }
        let expected_delta_sha256 = std::iter::repeat_n(PREPARED_FIXTURE_LARGE_DELTA_SHA256, 6)
            .chain(std::iter::repeat_n(PREPARED_FIXTURE_SMALL_DELTA_SHA256, 4))
            .collect::<Vec<_>>();
        if self.control_source.delta_sha256.len() != 10
            || self
                .control_source
                .delta_sha256
                .iter()
                .map(String::as_str)
                .ne(expected_delta_sha256)
        {
            return Err(PocError::Integrity(
                "prepared fixture delta digests differ from the closed zero-content profile"
                    .to_owned(),
            ));
        }
        if self.control_source.source_manifest_sha256
            != PREPARED_FIXTURE_CONTROL_SOURCE_MANIFEST_SHA256
        {
            return Err(PocError::Integrity(
                "prepared fixture control manifest digest differs from the closed profile"
                    .to_owned(),
            ));
        }

        let expected = [
            ("fixture-depth-1", 1_u64, GIB),
            ("fixture-depth-5", 5_u64, PREPARED_FIXTURE_DEPTH_FIVE_BYTES),
            (
                "fixture-depth-8",
                PREPARED_FIXTURE_CHAIN_DEPTH,
                PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
            ),
        ];
        if self.branches.len() != expected.len() {
            return Err(PocError::Integrity(
                "prepared fixture has an invalid branch count".to_owned(),
            ));
        }
        for ((name, depth, bytes), branch) in expected.iter().zip(&self.branches) {
            if branch.branch != *name
                || branch.chain_depth != *depth
                || branch.accumulated_bytes != *bytes
                || branch.projection.roots != branch.roots
                || branch.semantic.roots != branch.roots
                || branch.semantic.durability != branch.canonical
            {
                return Err(PocError::Integrity(
                    "prepared fixture branch shape does not match s4-chain-sparse-v1".to_owned(),
                ));
            }
            branch.projection.validate()?;
            if u64::try_from(branch.projection.kernel_lower_count()).unwrap_or(u64::MAX)
                != branch.chain_depth
            {
                return Err(PocError::Integrity(
                    "prepared fixture projection depth does not match its sealed chain depth"
                        .to_owned(),
                ));
            }
            if branch.semantic.schema_version != SCHEMA_VERSION
                || branch.semantic.semantic_format != crate::m1_contract::SEMANTIC_FORMAT_VERSION
                || branch.semantic.operation_id.as_str().is_empty()
            {
                return Err(PocError::Integrity(
                    "prepared fixture semantic attestation is invalid".to_owned(),
                ));
            }
            if branch.canonical.root_manifest.is_relative()
                || !branch.canonical.root_manifest.starts_with(control_root)
            {
                return Err(PocError::Integrity(
                    "prepared fixture canonical manifest is outside the server-owned cache"
                        .to_owned(),
                ));
            }
        }
        let maximum = self
            .branches
            .last()
            .ok_or_else(|| {
                PocError::Integrity("prepared fixture has no maximum branch".to_owned())
            })?
            .projection
            .lower_allocation_ids_newest_first();
        if maximum.len() != usize::try_from(PREPARED_FIXTURE_ALLOCATION_COUNT).unwrap_or(usize::MAX)
        {
            return Err(PocError::Integrity(
                "prepared fixture does not contain exactly eight cache allocations".to_owned(),
            ));
        }
        let unique = maximum.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != maximum.len() {
            return Err(PocError::Integrity(
                "prepared fixture maximum projection repeats an allocation".to_owned(),
            ));
        }
        for branch in &self.branches {
            let observed = branch.projection.lower_allocation_ids_newest_first();
            let expected_start = maximum.len().checked_sub(observed.len()).ok_or_else(|| {
                PocError::Integrity(
                    "prepared fixture branch is deeper than its maximum projection".to_owned(),
                )
            })?;
            if observed.as_slice() != &maximum[expected_start..] {
                return Err(PocError::Integrity(
                    "prepared fixture branches are not exact nested projection suffixes".to_owned(),
                ));
            }
        }
        Ok(())
    }

    pub fn branch(&self, branch: &str) -> PocResult<&PreparedFixtureBranch> {
        self.branches
            .iter()
            .find(|entry| entry.branch == branch)
            .ok_or_else(|| {
                PocError::Integrity(format!("prepared fixture is missing branch {branch}"))
            })
    }
}

pub fn prepared_fixture_manifest_path() -> PathBuf {
    PathBuf::from(PREPARED_FIXTURE_MANIFEST)
}

pub fn read_prepared_fixture_manifest() -> PocResult<PreparedFixtureManifest> {
    let manifest: PreparedFixtureManifest = read_json(&prepared_fixture_manifest_path())?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn write_prepared_fixture_manifest(manifest: &PreparedFixtureManifest) -> PocResult<()> {
    manifest.validate()?;
    write_immutable_json(&prepared_fixture_manifest_path(), manifest)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PreparedFixtureLayoutReceipt {
    pub allocation_count: u64,
    pub payload_logical_bytes: u64,
    pub control_source_logical_bytes: u64,
    pub allocated_bytes: u64,
    pub payload_bytes_read: u64,
}

/// Validate the closed cache without copying or hashing logical payload
/// bytes. Linux consumers require every fixture file to remain a hole-only
/// sparse regular file, so a write that allocates even one data extent fails
/// before refs are attached. Exact directory inventories reject omissions,
/// additions, symlinks, and path substitution.
pub fn validate_prepared_fixture_cache_layout(
    manifest: &PreparedFixtureManifest,
) -> PocResult<PreparedFixtureLayoutReceipt> {
    validate_prepared_fixture_cache_layout_at(
        manifest,
        Path::new(PREPARED_FIXTURE_PAYLOAD_ROOT),
        Path::new(PREPARED_FIXTURE_CONTROL_ROOT),
        Path::new(PREPARED_FIXTURE_CONTROL_SOURCE),
    )
}

fn validate_prepared_fixture_cache_layout_at(
    manifest: &PreparedFixtureManifest,
    payload_root: &Path,
    control_root: &Path,
    control_source: &Path,
) -> PocResult<PreparedFixtureLayoutReceipt> {
    manifest.validate_for_control_root(control_root)?;
    let maximum = manifest.branch("fixture-depth-8")?;
    let newest_first = maximum.projection.lower_allocation_ids_newest_first();
    let allocation_ids = newest_first.into_iter().rev().collect::<Vec<_>>();
    require_real_directory(payload_root, "prepared fixture payload root")?;
    let canonical_payload_root = std::fs::canonicalize(payload_root).map_err(|source| {
        PocError::io(
            "canonicalize prepared fixture payload root",
            payload_root,
            source,
        )
    })?;
    let allocation_arena = payload_root.join("allocations");
    validate_allocation_arena_inventory(&allocation_arena, &allocation_ids)?;
    let mut allocated_bytes = 0_u64;
    for (layer, allocation_id) in allocation_ids.iter().enumerate() {
        let allocation = open_allocation(&allocation_arena, allocation_id)?;
        let canonical_upper = std::fs::canonicalize(&allocation.upper_dir).map_err(|source| {
            PocError::io(
                "canonicalize prepared fixture allocation upper",
                &allocation.upper_dir,
                source,
            )
        })?;
        if !canonical_upper.starts_with(&canonical_payload_root) {
            return Err(PocError::Integrity(format!(
                "prepared fixture allocation escaped its closed payload root: {}",
                canonical_upper.display()
            )));
        }
        let expected = expected_layer_files(u8::try_from(layer).map_err(|_| {
            PocError::Integrity("prepared fixture layer index overflowed u8".to_owned())
        })?);
        allocated_bytes = allocated_bytes
            .checked_add(validate_sparse_directory(&allocation.upper_dir, &expected)?)
            .ok_or_else(|| {
                PocError::Integrity("prepared fixture allocated-byte total overflowed".to_owned())
            })?;
    }

    let mut expected_control = vec![("layer-000.bin".to_owned(), GIB)];
    expected_control
        .extend((0..10).map(|index| (format!("delta-{index:02}.bin"), delta_bytes(index))));
    allocated_bytes = allocated_bytes
        .checked_add(validate_sparse_directory(
            control_source,
            &expected_control,
        )?)
        .ok_or_else(|| {
            PocError::Integrity("prepared fixture allocated-byte total overflowed".to_owned())
        })?;

    require_real_directory(control_root, "prepared fixture control root")?;
    let canonical_control_root = std::fs::canonicalize(control_root).map_err(|source| {
        PocError::io(
            "canonicalize prepared fixture control root",
            control_root,
            source,
        )
    })?;
    for branch in &manifest.branches {
        let canonical_manifest =
            std::fs::canonicalize(&branch.canonical.root_manifest).map_err(|source| {
                PocError::io(
                    "canonicalize prepared fixture root manifest",
                    &branch.canonical.root_manifest,
                    source,
                )
            })?;
        let manifest_metadata = std::fs::symlink_metadata(&branch.canonical.root_manifest)
            .map_err(|source| {
                PocError::io(
                    "stat prepared fixture root manifest",
                    &branch.canonical.root_manifest,
                    source,
                )
            })?;
        if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
            return Err(PocError::Integrity(format!(
                "prepared fixture canonical manifest is not a real file: {}",
                branch.canonical.root_manifest.display()
            )));
        }
        if !canonical_manifest.starts_with(&canonical_control_root) {
            return Err(PocError::Integrity(format!(
                "prepared fixture canonical manifest escaped its closed control root: {}",
                canonical_manifest.display()
            )));
        }
    }

    Ok(PreparedFixtureLayoutReceipt {
        allocation_count: u64::try_from(allocation_ids.len()).map_err(|_| {
            PocError::Integrity("prepared fixture allocation count overflowed u64".to_owned())
        })?,
        payload_logical_bytes: PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
        control_source_logical_bytes: PREPARED_FIXTURE_CONTROL_SOURCE_BYTES,
        allocated_bytes,
        payload_bytes_read: 0,
    })
}

fn expected_layer_files(layer: u8) -> Vec<(String, u64)> {
    match layer {
        0 => vec![("layer-000.bin".to_owned(), GIB)],
        1 | 5 => vec![(
            format!("layer-{layer:03}.bin"),
            PREPARED_FIXTURE_SINGLE_FILE_LAYER_BYTES,
        )],
        2..=4 | 6..=7 => (0..10)
            .map(|index| {
                (
                    format!("marker-{layer:03}-{index:02}.bin"),
                    partition_bytes(PREPARED_FIXTURE_MARKER_LAYER_BYTES, index),
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn delta_bytes(index: usize) -> u64 {
    partition_bytes(MIB, index)
}

fn partition_bytes(total: u64, index: usize) -> u64 {
    total / 10 + u64::from(index < usize::try_from(total % 10).unwrap_or(0))
}

fn validate_allocation_arena_inventory(arena: &Path, expected: &[&AllocationId]) -> PocResult<()> {
    require_real_directory(arena, "prepared fixture allocation arena")?;
    let expected = expected
        .iter()
        .map(|allocation_id| allocation_id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    for prefix in std::fs::read_dir(arena)
        .map_err(|source| PocError::io("read prepared fixture allocation arena", arena, source))?
    {
        let prefix = prefix.map_err(|source| {
            PocError::io("read prepared fixture allocation prefix", arena, source)
        })?;
        let prefix_path = prefix.path();
        let prefix_name = prefix.file_name().into_string().map_err(|_| {
            PocError::Integrity(format!(
                "prepared fixture allocation prefix is not UTF-8: {}",
                prefix_path.display()
            ))
        })?;
        require_real_directory(&prefix_path, "prepared fixture allocation prefix")?;
        if prefix_name.len() != 2 || !prefix_name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(PocError::Integrity(format!(
                "prepared fixture allocation prefix is invalid: {}",
                prefix_path.display()
            )));
        }
        let mut prefix_count = 0_u64;
        for allocation in std::fs::read_dir(&prefix_path).map_err(|source| {
            PocError::io(
                "read prepared fixture allocation prefix",
                &prefix_path,
                source,
            )
        })? {
            let allocation = allocation.map_err(|source| {
                PocError::io(
                    "read prepared fixture allocation entry",
                    &prefix_path,
                    source,
                )
            })?;
            let allocation_path = allocation.path();
            let allocation_id = allocation.file_name().into_string().map_err(|_| {
                PocError::Integrity(format!(
                    "prepared fixture allocation ID is not UTF-8: {}",
                    allocation_path.display()
                ))
            })?;
            require_real_directory(&allocation_path, "prepared fixture allocation root")?;
            if !allocation_id.starts_with(&prefix_name) || !observed.insert(allocation_id) {
                return Err(PocError::Integrity(format!(
                    "prepared fixture allocation arena has an invalid entry: {}",
                    allocation_path.display()
                )));
            }
            prefix_count += 1;
        }
        if prefix_count == 0 {
            return Err(PocError::Integrity(format!(
                "prepared fixture allocation arena has an empty prefix: {}",
                prefix_path.display()
            )));
        }
    }
    if observed != expected {
        return Err(PocError::Integrity(
            "prepared fixture allocation arena differs from the exact sealed projection".to_owned(),
        ));
    }
    Ok(())
}

fn validate_sparse_directory(path: &Path, expected: &[(String, u64)]) -> PocResult<u64> {
    require_real_directory(path, "prepared fixture sparse directory")?;
    let observed = std::fs::read_dir(path)
        .map_err(|source| PocError::io("read prepared fixture sparse directory", path, source))?
        .map(|entry| {
            let entry = entry
                .map_err(|source| PocError::io("read prepared fixture entry", path, source))?;
            let name = entry.file_name().into_string().map_err(|_| {
                PocError::Integrity(format!(
                    "prepared fixture entry name is not UTF-8: {}",
                    entry.path().display()
                ))
            })?;
            Ok(name)
        })
        .collect::<PocResult<BTreeSet<_>>>()?;
    let expected_names = expected
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    if observed != expected_names {
        return Err(PocError::Integrity(format!(
            "prepared fixture sparse directory inventory differs at {}",
            path.display()
        )));
    }
    let mut allocated_bytes = 0_u64;
    for (name, logical_bytes) in expected {
        let file_path = path.join(name);
        let metadata = std::fs::symlink_metadata(&file_path).map_err(|source| {
            PocError::io("stat prepared fixture sparse file", &file_path, source)
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() != *logical_bytes
        {
            return Err(PocError::Integrity(format!(
                "prepared fixture sparse file shape differs: {}",
                file_path.display()
            )));
        }
        allocated_bytes = allocated_bytes
            .checked_add(allocated_file_bytes(&file_path, &metadata)?)
            .ok_or_else(|| {
                PocError::Integrity("prepared fixture allocated-byte total overflowed".to_owned())
            })?;
    }
    Ok(allocated_bytes)
}

#[cfg(target_os = "linux")]
fn allocated_file_bytes(path: &Path, metadata: &std::fs::Metadata) -> PocResult<u64> {
    use std::os::unix::fs::MetadataExt;

    let allocated = metadata.blocks().checked_mul(512).ok_or_else(|| {
        PocError::Integrity("prepared fixture allocated-byte receipt overflowed".to_owned())
    })?;
    if allocated != 0 {
        return Err(PocError::Integrity(format!(
            "prepared fixture sparse file acquired data blocks: {}",
            path.display()
        )));
    }
    let file = File::open(path)
        .map_err(|source| PocError::io("open prepared fixture sparse file", path, source))?;
    match rustix::fs::seek(&file, rustix::fs::SeekFrom::Data(0)) {
        Err(error) if error == rustix::io::Errno::NXIO => Ok(allocated),
        Ok(offset) => Err(PocError::Integrity(format!(
            "prepared fixture sparse file acquired a data extent at {offset}: {}",
            path.display()
        ))),
        Err(error) => Err(PocError::io(
            "seek prepared fixture sparse data extent",
            path,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        )),
    }
}

#[cfg(not(target_os = "linux"))]
fn allocated_file_bytes(_path: &Path, _metadata: &std::fs::Metadata) -> PocResult<u64> {
    // The cache is built and consumed only by the Linux/ARM64 runtime. Native
    // host unit tests still validate exact inventories and logical sizes.
    Ok(0)
}

fn require_real_directory(path: &Path, label: &str) -> PocResult<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| PocError::io("stat prepared fixture directory", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PocError::Integrity(format!(
            "{label} is not a real directory: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m1_contract::SEMANTIC_FORMAT_VERSION;
    use crate::{AttributionInput, AttributionRootId, OperationId, RootId, SemanticPhaseSpan};

    struct HermeticPreparedFixture {
        root: PathBuf,
        payload_root: PathBuf,
        control_root: PathBuf,
        control_source: PathBuf,
        allocation_uppers: Vec<PathBuf>,
        manifest: PreparedFixtureManifest,
    }

    impl HermeticPreparedFixture {
        fn create() -> Self {
            let root = std::env::temp_dir().join(format!(
                "mpla-prepared-fixture-layout-{}",
                uuid::Uuid::new_v4()
            ));
            let payload_root = root.join("payload");
            let control_root = root.join("control");
            let control_source = root.join("control-source");
            let allocation_arena = payload_root.join("allocations");
            std::fs::create_dir_all(&control_root).expect("create hermetic control root");
            std::fs::create_dir_all(&control_source).expect("create hermetic control source");

            let mut allocation_ids = Vec::new();
            let mut allocation_uppers = Vec::new();
            for layer in 0..PREPARED_FIXTURE_CHAIN_DEPTH {
                let operation_id = OperationId::from_string(format!("fixture-layer-{layer}"));
                let allocation =
                    crate::allocation::create_allocation(&allocation_arena, &operation_id)
                        .expect("create hermetic fixture allocation");
                for (name, logical_bytes) in
                    expected_layer_files(u8::try_from(layer).expect("layer index"))
                {
                    create_sparse_file(&allocation.upper_dir.join(name), logical_bytes);
                }
                allocation_ids.push(allocation.descriptor.allocation_id);
                allocation_uppers.push(allocation.upper_dir);
            }

            create_sparse_file(&control_source.join("layer-000.bin"), GIB);
            for index in 0..10 {
                create_sparse_file(
                    &control_source.join(format!("delta-{index:02}.bin")),
                    delta_bytes(index),
                );
            }

            let branches = [
                ("fixture-depth-1", 1_usize, GIB),
                (
                    "fixture-depth-5",
                    5_usize,
                    PREPARED_FIXTURE_DEPTH_FIVE_BYTES,
                ),
                (
                    "fixture-depth-8",
                    usize::try_from(PREPARED_FIXTURE_CHAIN_DEPTH).expect("chain depth"),
                    PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
                ),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (branch, depth, accumulated_bytes))| {
                let roots = CanonicalRootPair {
                    root_id: RootId::parse(format!("{:02x}", index + 1).repeat(32))
                        .expect("root ID"),
                    attribution_root_id: AttributionRootId::parse(
                        format!("{:02x}", index + 65).repeat(32),
                    )
                    .expect("attribution root ID"),
                };
                let projection = ProjectionRecipe {
                    schema_version: SCHEMA_VERSION,
                    roots: roots.clone(),
                    base_allocation_id: allocation_ids[0].clone(),
                    net_delta_carrier_id: None,
                    recent_delta_ids: allocation_ids[1..depth].iter().rev().cloned().collect(),
                };
                let canonical_manifest = control_root.join(format!("{branch}.json"));
                std::fs::write(&canonical_manifest, b"{}")
                    .expect("write hermetic canonical manifest");
                let attribution = AttributionInput {
                    actor_id: "prepared-fixture-test".to_owned(),
                    semantic_operation_id: branch.to_owned(),
                };
                let canonical = CanonicalDurabilityReceipt {
                    root_manifest: canonical_manifest,
                    semantic_attribution: attribution.clone(),
                    immutable_object_count: 1,
                    immutable_object_bytes: 2,
                    object_set_sha256: "ab".repeat(32),
                    files_fsynced: true,
                    object_directory_fsynced: true,
                    manifest_fsynced: true,
                    manifest_directory_fsynced: true,
                };
                PreparedFixtureBranch {
                    branch: branch.to_owned(),
                    chain_depth: u64::try_from(depth).expect("branch depth"),
                    accumulated_bytes,
                    roots: roots.clone(),
                    projection,
                    canonical: canonical.clone(),
                    semantic: SemanticBuildReceipt {
                        schema_version: SCHEMA_VERSION,
                        semantic_format: SEMANTIC_FORMAT_VERSION.to_owned(),
                        operation_id: OperationId::from_string(format!(
                            "prepared-fixture-{branch}"
                        )),
                        roots,
                        record_stream_sha256: "cd".repeat(32),
                        entry_count: 1,
                        bytes_read: 0,
                        spool_runs: 0,
                        spool_bytes: 0,
                        peak_open_data_fds: 1,
                        peak_data_workers: 1,
                        phase_spans: vec![SemanticPhaseSpan {
                            phase: "test".to_owned(),
                            elapsed_ns: 1,
                        }],
                        durability: canonical,
                    },
                }
            })
            .collect();
            let manifest = PreparedFixtureManifest::new(
                "01".repeat(20),
                PreparedFixtureControlSource {
                    base_sha256: PREPARED_FIXTURE_BASE_SHA256.to_owned(),
                    delta_sha256: std::iter::repeat_n(
                        PREPARED_FIXTURE_LARGE_DELTA_SHA256.to_owned(),
                        6,
                    )
                    .chain(std::iter::repeat_n(
                        PREPARED_FIXTURE_SMALL_DELTA_SHA256.to_owned(),
                        4,
                    ))
                    .collect(),
                    source_manifest_sha256: PREPARED_FIXTURE_CONTROL_SOURCE_MANIFEST_SHA256
                        .to_owned(),
                },
                branches,
            );

            Self {
                root,
                payload_root,
                control_root,
                control_source,
                allocation_uppers,
                manifest,
            }
        }

        fn validate(&self) -> PocResult<PreparedFixtureLayoutReceipt> {
            validate_prepared_fixture_cache_layout_at(
                &self.manifest,
                &self.payload_root,
                &self.control_root,
                &self.control_source,
            )
        }
    }

    impl Drop for HermeticPreparedFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn create_sparse_file(path: &Path, logical_bytes: u64) {
        let file = std::fs::File::create(path).expect("create hermetic sparse file");
        file.set_len(logical_bytes)
            .expect("size hermetic sparse file");
    }

    fn sparse_inventory(uppers: &[PathBuf]) -> Vec<Vec<(PathBuf, u64, bool)>> {
        uppers
            .iter()
            .map(|upper| {
                let mut entries = std::fs::read_dir(upper)
                    .expect("read allocation upper")
                    .map(|entry| {
                        let path = entry.expect("read allocation entry").path();
                        let metadata =
                            std::fs::symlink_metadata(&path).expect("stat allocation entry");
                        (path, metadata.len(), metadata.file_type().is_symlink())
                    })
                    .collect::<Vec<_>>();
                entries.sort();
                entries
            })
            .collect()
    }

    #[test]
    fn closed_profile_constants_are_not_paths_selected_by_callers() {
        assert_eq!(PREPARED_FIXTURE_PROFILE, "s4-chain-sparse-v1");
        assert!(Path::new(PREPARED_FIXTURE_ROOT).is_absolute());
        assert!(Path::new(PREPARED_FIXTURE_MANIFEST).starts_with(PREPARED_FIXTURE_ROOT));
    }

    #[test]
    fn sparse_fixture_capacity_is_physical_not_logical() {
        let requirement = prepared_fixture_storage_requirement().expect("storage requirement");
        assert_eq!(
            requirement.required_available_bytes,
            PREPARED_FIXTURE_BUILDER_HEADROOM_BYTES
        );
        assert!(requirement.required_available_bytes < PREPARED_FIXTURE_DEPTH_EIGHT_BYTES);
    }

    #[test]
    fn closed_sparse_layers_preserve_an_exact_eight_gib_logical_chain() {
        let logical_bytes = (0..PREPARED_FIXTURE_CHAIN_DEPTH)
            .map(|layer| {
                expected_layer_files(u8::try_from(layer).expect("layer index"))
                    .into_iter()
                    .map(|(_, bytes)| bytes)
                    .sum::<u64>()
            })
            .sum::<u64>();
        assert_eq!(logical_bytes, PREPARED_FIXTURE_DEPTH_EIGHT_BYTES);

        let marker = expected_layer_files(2);
        assert_eq!(marker.len(), 10);
        assert_eq!(
            marker.iter().map(|(_, bytes)| *bytes).sum::<u64>(),
            PREPARED_FIXTURE_MARKER_LAYER_BYTES
        );
        assert_eq!(
            marker
                .iter()
                .filter(|(_, bytes)| *bytes == PREPARED_FIXTURE_MARKER_LAYER_BYTES / 10 + 1)
                .count(),
            usize::try_from(PREPARED_FIXTURE_MARKER_LAYER_BYTES % 10).expect("remainder")
        );
    }

    #[test]
    fn hermetic_sealed_fixture_validation_is_repeatable_and_read_only() {
        let fixture = HermeticPreparedFixture::create();
        let before = sparse_inventory(&fixture.allocation_uppers);

        let first = fixture.validate().expect("valid sealed fixture");
        let second = fixture
            .validate()
            .expect("repeat sealed fixture validation");

        assert_eq!(first, second);
        assert_eq!(first.allocation_count, PREPARED_FIXTURE_ALLOCATION_COUNT);
        assert_eq!(
            first.payload_logical_bytes,
            PREPARED_FIXTURE_DEPTH_EIGHT_BYTES
        );
        assert_eq!(first.payload_bytes_read, 0);
        let after = sparse_inventory(&fixture.allocation_uppers);
        assert_eq!(after, before, "validation must not mutate the sealed cache");
    }

    #[test]
    fn hermetic_fixture_validation_rejects_corrupt_or_partial_cache_without_repair() {
        let missing = HermeticPreparedFixture::create();
        let missing_path = missing.allocation_uppers[0].join("layer-000.bin");
        std::fs::remove_file(&missing_path).expect("remove required fixture file");
        assert!(missing.validate().is_err());
        assert!(
            !missing_path.exists(),
            "validation must not repair a missing file"
        );

        let extra = HermeticPreparedFixture::create();
        let extra_path = extra.allocation_uppers[1].join("unexpected.bin");
        std::fs::write(&extra_path, b"unexpected").expect("write unexpected fixture file");
        assert!(extra.validate().is_err());
        assert!(
            extra_path.exists(),
            "validation must not remove an unexpected file"
        );

        let wrong_size = HermeticPreparedFixture::create();
        let wrong_size_path = wrong_size.allocation_uppers[5].join("layer-005.bin");
        std::fs::File::options()
            .write(true)
            .open(&wrong_size_path)
            .expect("open fixture file")
            .set_len(MIB)
            .expect("truncate fixture file");
        assert!(wrong_size.validate().is_err());
        assert_eq!(
            std::fs::metadata(&wrong_size_path)
                .expect("stat corrupt fixture file")
                .len(),
            MIB,
            "validation must not resize a corrupt file"
        );

        let wrong_manifest = HermeticPreparedFixture::create();
        let mut tampered_manifest = wrong_manifest.manifest.clone();
        tampered_manifest.profile = "untrusted-profile".to_owned();
        assert!(validate_prepared_fixture_cache_layout_at(
            &tampered_manifest,
            &wrong_manifest.payload_root,
            &wrong_manifest.control_root,
            &wrong_manifest.control_source,
        )
        .is_err());
        assert_eq!(
            wrong_manifest.manifest.profile, PREPARED_FIXTURE_PROFILE,
            "validation must not rewrite a corrupt seal"
        );
    }

    #[cfg(unix)]
    #[test]
    fn hermetic_fixture_validation_rejects_symlink_substitution_without_following_it() {
        use std::os::unix::fs::symlink;

        let fixture = HermeticPreparedFixture::create();
        let target = fixture.root.join("outside.bin");
        std::fs::write(&target, b"outside").expect("write outside sentinel");
        let fixture_path = fixture.allocation_uppers[0].join("layer-000.bin");
        std::fs::remove_file(&fixture_path).expect("remove fixture file before symlink");
        symlink(&target, &fixture_path).expect("substitute fixture symlink");

        assert!(fixture.validate().is_err());
        assert_eq!(
            std::fs::read_to_string(&target).expect("read outside sentinel"),
            "outside",
            "validation must not follow or mutate a substituted symlink"
        );
        assert!(
            std::fs::symlink_metadata(&fixture_path)
                .expect("stat substituted symlink")
                .file_type()
                .is_symlink(),
            "validation must not replace a substituted symlink"
        );
    }
}
