use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{PocError, PocResult, SCHEMA_VERSION};

const STREAM_BYTES: usize = 32 * 1024;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureTier {
    Smoke,
    Heavy,
}

impl FixtureTier {
    pub fn parse(value: &str) -> PocResult<Self> {
        match value {
            "smoke" => Ok(Self::Smoke),
            "heavy" => Ok(Self::Heavy),
            _ => Err(PocError::Integrity(format!(
                "unknown fixture tier {value:?}; expected smoke or heavy"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FixtureId {
    #[serde(rename = "S0-empty")]
    S0Empty,
    #[serde(rename = "S1-code")]
    S1Code,
    #[serde(rename = "S2-large")]
    S2Large,
    #[serde(rename = "S3-small")]
    S3Small,
    #[serde(rename = "S4-chain")]
    S4Chain,
    #[serde(rename = "S5-semantics")]
    S5Semantics,
}

impl FixtureId {
    pub const ALL: [Self; 6] = [
        Self::S0Empty,
        Self::S1Code,
        Self::S2Large,
        Self::S3Small,
        Self::S4Chain,
        Self::S5Semantics,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::S0Empty => "S0-empty",
            Self::S1Code => "S1-code",
            Self::S2Large => "S2-large",
            Self::S3Small => "S3-small",
            Self::S4Chain => "S4-chain",
            Self::S5Semantics => "S5-semantics",
        }
    }

    pub fn parse(value: &str) -> PocResult<Self> {
        Self::ALL
            .into_iter()
            .find(|fixture| fixture.as_str() == value)
            .ok_or_else(|| {
                PocError::Integrity(format!(
                    "unknown fixture {value:?}; expected one of {}",
                    Self::ALL
                        .iter()
                        .map(|fixture| fixture.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FixturePlan {
    pub fixture_id: FixtureId,
    pub tier: FixtureTier,
    pub declared_paths: u64,
    pub declared_logical_bytes: u64,
    pub delta_count: u64,
    pub maximum_chain_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FixtureReceipt {
    pub schema_version: u32,
    pub fixture_id: FixtureId,
    pub tier: FixtureTier,
    pub root: PathBuf,
    pub declared_paths: u64,
    pub observed_paths: u64,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub unique_inodes: u64,
    pub regular_files: u64,
    pub directories: u64,
    pub symlinks: u64,
    pub sparse_files: u64,
    pub hardlink_aliases: u64,
    pub xattrs_written: u64,
    pub generated_sha256: String,
    pub build_duration_ms: u64,
    pub stream_buffer_bytes: u64,
}

pub fn fixture_plan(fixture_id: FixtureId, tier: FixtureTier) -> FixturePlan {
    match (fixture_id, tier) {
        (FixtureId::S0Empty, _) => FixturePlan {
            fixture_id,
            tier,
            declared_paths: 1,
            declared_logical_bytes: 0,
            delta_count: 0,
            maximum_chain_bytes: 0,
        },
        (FixtureId::S1Code, FixtureTier::Smoke) => FixturePlan {
            fixture_id,
            tier,
            declared_paths: 10_000,
            declared_logical_bytes: 128 * MIB,
            delta_count: 0,
            maximum_chain_bytes: 128 * MIB,
        },
        (FixtureId::S1Code, FixtureTier::Heavy) => FixturePlan {
            fixture_id,
            tier,
            declared_paths: 100_000,
            declared_logical_bytes: GIB,
            delta_count: 0,
            maximum_chain_bytes: GIB,
        },
        (FixtureId::S2Large, FixtureTier::Smoke) => FixturePlan {
            fixture_id,
            tier,
            declared_paths: 5,
            declared_logical_bytes: 256 * MIB,
            delta_count: 0,
            maximum_chain_bytes: 256 * MIB,
        },
        (FixtureId::S2Large, FixtureTier::Heavy) => FixturePlan {
            fixture_id,
            tier,
            declared_paths: 5,
            declared_logical_bytes: GIB,
            delta_count: 0,
            maximum_chain_bytes: GIB,
        },
        (FixtureId::S3Small, FixtureTier::Smoke) => FixturePlan {
            fixture_id,
            tier,
            declared_paths: 20_000,
            declared_logical_bytes: 0,
            delta_count: 0,
            maximum_chain_bytes: 64 * MIB,
        },
        (FixtureId::S3Small, FixtureTier::Heavy) => FixturePlan {
            fixture_id,
            tier,
            declared_paths: 250_000,
            declared_logical_bytes: 0,
            delta_count: 0,
            maximum_chain_bytes: GIB,
        },
        (FixtureId::S4Chain, FixtureTier::Smoke) => FixturePlan {
            fixture_id,
            tier,
            declared_paths: 36,
            declared_logical_bytes: GIB + 16 * MIB,
            delta_count: 16,
            maximum_chain_bytes: GIB + 16 * MIB,
        },
        (FixtureId::S4Chain, FixtureTier::Heavy) => FixturePlan {
            fixture_id,
            tier,
            declared_paths: 132,
            declared_logical_bytes: 8 * GIB + 256 * MIB,
            delta_count: 64,
            maximum_chain_bytes: 8 * GIB + 256 * MIB,
        },
        (FixtureId::S5Semantics, FixtureTier::Smoke) => FixturePlan {
            fixture_id,
            tier,
            declared_paths: 5_000,
            declared_logical_bytes: 64 * MIB,
            delta_count: 0,
            maximum_chain_bytes: 64 * MIB,
        },
        (FixtureId::S5Semantics, FixtureTier::Heavy) => FixturePlan {
            fixture_id,
            tier,
            declared_paths: 25_000,
            declared_logical_bytes: 256 * MIB,
            delta_count: 0,
            maximum_chain_bytes: 256 * MIB,
        },
    }
}

pub fn prepare_fixture(
    root: &Path,
    fixture_id: FixtureId,
    tier: FixtureTier,
) -> PocResult<FixtureReceipt> {
    if root.exists() {
        return Err(PocError::Integrity(format!(
            "fixture destination already exists: {}",
            root.display()
        )));
    }
    let started = Instant::now();
    fs::create_dir(root).map_err(|error| PocError::io("create fixture root", root, error))?;
    build_fixture(root, fixture_id, tier, started)
}

pub fn populate_empty_fixture_root(
    root: &Path,
    fixture_id: FixtureId,
    tier: FixtureTier,
) -> PocResult<FixtureReceipt> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| PocError::io("stat fixture root", root, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PocError::Integrity(format!(
            "fixture root is not a real directory: {}",
            root.display()
        )));
    }
    let mut entries =
        fs::read_dir(root).map_err(|error| PocError::io("read fixture root", root, error))?;
    if entries
        .next()
        .transpose()
        .map_err(|error| PocError::io("read fixture root entry", root, error))?
        .is_some()
    {
        return Err(PocError::Integrity(format!(
            "fixture destination is not empty: {}",
            root.display()
        )));
    }
    build_fixture(root, fixture_id, tier, Instant::now())
}

fn build_fixture(
    root: &Path,
    fixture_id: FixtureId,
    tier: FixtureTier,
    started: Instant,
) -> PocResult<FixtureReceipt> {
    let plan = fixture_plan(fixture_id, tier);
    if plan.maximum_chain_bytes >= 10 * GIB {
        return Err(PocError::Integrity(format!(
            "{} plan exceeds the 10-GiB chain envelope",
            fixture_id.as_str()
        )));
    }
    let mut builder = FixtureBuilder::new(root, &plan)?;
    let build = match fixture_id {
        FixtureId::S0Empty => Ok(()),
        FixtureId::S1Code => builder.build_code(),
        FixtureId::S2Large => builder.build_large(),
        FixtureId::S3Small => builder.build_small(),
        FixtureId::S4Chain => builder.build_chain(),
        FixtureId::S5Semantics => builder.build_semantics(),
    };
    build?;
    builder.finish(started)
}

struct FixtureBuilder<'a> {
    root: &'a Path,
    plan: &'a FixturePlan,
    digest: Sha256,
    directories: Vec<PathBuf>,
    observed_paths: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
    unique_inodes: u64,
    regular_files: u64,
    symlinks: u64,
    sparse_files: u64,
    hardlink_aliases: u64,
    xattrs_written: u64,
}

impl<'a> FixtureBuilder<'a> {
    fn new(root: &'a Path, plan: &'a FixturePlan) -> PocResult<Self> {
        let mut builder = Self {
            root,
            plan,
            digest: Sha256::new(),
            directories: vec![root.to_path_buf()],
            observed_paths: 1,
            logical_bytes: 0,
            allocated_bytes: 0,
            unique_inodes: 1,
            regular_files: 0,
            symlinks: 0,
            sparse_files: 0,
            hardlink_aliases: 0,
            xattrs_written: 0,
        };
        builder.record(b"d", Path::new("."), 0, &[]);
        Ok(builder)
    }

    fn build_code(&mut self) -> PocResult<()> {
        let directory_count = match self.plan.tier {
            FixtureTier::Smoke => 64_u64,
            FixtureTier::Heavy => 512,
        };
        let symlink_count = match self.plan.tier {
            FixtureTier::Smoke => 32_u64,
            FixtureTier::Heavy => 256,
        };
        self.create_directory(Path::new("src"))?;
        for index in 0..directory_count {
            self.create_directory(&PathBuf::from(format!("src/d{index:04}")))?;
        }
        let file_count = self
            .plan
            .declared_paths
            .checked_sub(2 + directory_count + symlink_count)
            .ok_or_else(|| PocError::Integrity("S1 path plan underflow".to_owned()))?;
        for index in 0..file_count {
            let directory = index % directory_count;
            let relative = PathBuf::from(format!("src/d{directory:04}/module-{index:08}.rs"));
            let size = distributed_size(self.plan.declared_logical_bytes, file_count, index);
            self.create_file(&relative, size, 0x51_0000_0000 ^ index, false)?;
            if index % 97 == 0 {
                make_executable(&self.root.join(&relative))?;
                self.record(b"m", &relative, 0o755, &[]);
            }
            if index == 0 {
                self.try_write_xattr(&relative, b"user.mpla.fixture", b"S1-code")?;
            }
        }
        for index in 0..symlink_count {
            let directory = index % directory_count;
            let target_index = index.min(file_count.saturating_sub(1));
            let relative = PathBuf::from(format!("src/d{directory:04}/link-{index:06}"));
            let target = PathBuf::from(format!(
                "../d{:04}/module-{target_index:08}.rs",
                target_index % directory_count
            ));
            self.create_symlink(&relative, &target)?;
        }
        Ok(())
    }

    fn build_large(&mut self) -> PocResult<()> {
        for index in 0..4_u64 {
            let size = distributed_size(self.plan.declared_logical_bytes, 4, index);
            self.create_file(
                &PathBuf::from(format!("large-{index}.bin")),
                size,
                0x52_0000_0000 ^ index,
                true,
            )?;
        }
        Ok(())
    }

    fn build_small(&mut self) -> PocResult<()> {
        let directory_count = match self.plan.tier {
            FixtureTier::Smoke => 256_u64,
            FixtureTier::Heavy => 2_048,
        };
        let alias_count = match self.plan.tier {
            FixtureTier::Smoke => 256_u64,
            FixtureTier::Heavy => 2_048,
        };
        self.create_directory(Path::new("fanout"))?;
        for index in 0..directory_count {
            self.create_directory(&PathBuf::from(format!("fanout/d{index:05}")))?;
        }
        let file_count = self
            .plan
            .declared_paths
            .checked_sub(2 + directory_count + alias_count)
            .ok_or_else(|| PocError::Integrity("S3 path plan underflow".to_owned()))?;
        let mut alias_sources = Vec::with_capacity(usize::try_from(alias_count).unwrap_or(0));
        for index in 0..file_count {
            let directory = index % directory_count;
            let relative = PathBuf::from(format!(
                "fanout/d{directory:05}/small-{index:08}-{:080}.dat",
                index % 10_000
            ));
            self.create_file(&relative, index % 4_097, 0x53_0000_0000 ^ index, false)?;
            if u64::try_from(alias_sources.len()).unwrap_or(u64::MAX) < alias_count
                && index % 31 == 0
            {
                alias_sources.push(relative);
            }
        }
        while u64::try_from(alias_sources.len()).unwrap_or(u64::MAX) < alias_count {
            alias_sources.push(PathBuf::from(format!(
                "fanout/d00000/small-{:08}-{:080}.dat",
                alias_sources.len(),
                alias_sources.len() % 10_000
            )));
        }
        for (index, source) in alias_sources.into_iter().enumerate() {
            let relative = PathBuf::from(format!(
                "fanout/d{:05}/hardlink-{:08}",
                u64::try_from(index).unwrap_or(0) % directory_count,
                index
            ));
            self.create_hardlink(&source, &relative)?;
        }
        Ok(())
    }

    fn build_chain(&mut self) -> PocResult<()> {
        self.create_directory(Path::new("base"))?;
        self.create_directory(Path::new("deltas"))?;
        self.create_file(
            Path::new("base/payload.bin"),
            self.plan
                .declared_logical_bytes
                .saturating_sub(self.plan.delta_count * delta_size(self.plan.tier)),
            0x54_0000_0000,
            true,
        )?;
        for index in 0..self.plan.delta_count {
            let directory = PathBuf::from(format!("deltas/d{index:03}"));
            self.create_directory(&directory)?;
            self.create_file(
                &directory.join("edit.bin"),
                delta_size(self.plan.tier),
                0x54_1000_0000 ^ index,
                false,
            )?;
        }
        Ok(())
    }

    fn build_semantics(&mut self) -> PocResult<()> {
        let directory_count = match self.plan.tier {
            FixtureTier::Smoke => 64_u64,
            FixtureTier::Heavy => 256,
        };
        let symlink_count = directory_count / 2;
        let hardlink_count = directory_count;
        self.create_directory(Path::new("tree"))?;
        for index in 0..directory_count {
            self.create_directory(&PathBuf::from(format!("tree/d{index:04}")))?;
        }
        self.create_directory(Path::new("mutations"))?;
        for special in [
            "whiteout-target",
            "opaque-lower-child",
            "type-file-to-directory",
            "rename-source",
        ] {
            self.create_file(
                &PathBuf::from(format!("mutations/{special}")),
                64,
                seed_for(special.as_bytes()),
                false,
            )?;
        }
        let reserved = 3 + directory_count + 4 + symlink_count + hardlink_count;
        let file_count = self
            .plan
            .declared_paths
            .checked_sub(reserved)
            .ok_or_else(|| PocError::Integrity("S5 path plan underflow".to_owned()))?;
        let special_bytes = 4 * 64;
        let payload_bytes = self
            .plan
            .declared_logical_bytes
            .saturating_sub(special_bytes);
        let mut hardlink_sources = Vec::with_capacity(usize::try_from(hardlink_count).unwrap_or(0));
        for index in 0..file_count {
            let directory = index % directory_count;
            let relative = PathBuf::from(format!("tree/d{directory:04}/node-{index:08}.bin"));
            let size = distributed_size(payload_bytes, file_count, index);
            let sparse = index % 503 == 0;
            self.create_file(&relative, size, 0x55_0000_0000 ^ index, sparse)?;
            if index == 0 {
                self.try_write_xattr(&relative, b"user.mpla.semantic", b"semantic-v1")?;
            }
            if u64::try_from(hardlink_sources.len()).unwrap_or(u64::MAX) < hardlink_count {
                hardlink_sources.push(relative);
            }
        }
        for index in 0..symlink_count {
            let relative = PathBuf::from(format!("tree/d{index:04}/symlink"));
            let target = PathBuf::from(format!("node-{index:08}.bin"));
            self.create_symlink(&relative, &target)?;
        }
        for (index, source) in hardlink_sources.into_iter().enumerate() {
            let relative = PathBuf::from(format!(
                "tree/d{:04}/hardlink-{:04}",
                u64::try_from(index).unwrap_or(0) % directory_count,
                index
            ));
            self.create_hardlink(&source, &relative)?;
        }
        Ok(())
    }

    fn create_directory(&mut self, relative: &Path) -> PocResult<()> {
        let path = self.root.join(relative);
        fs::create_dir(&path)
            .map_err(|error| PocError::io("create fixture directory", &path, error))?;
        self.directories.push(path);
        self.observed_paths = self.observed_paths.saturating_add(1);
        self.unique_inodes = self.unique_inodes.saturating_add(1);
        self.record(b"d", relative, 0, &[]);
        Ok(())
    }

    fn create_file(
        &mut self,
        relative: &Path,
        size: u64,
        seed: u64,
        sparse: bool,
    ) -> PocResult<()> {
        let path = self.root.join(relative);
        let parent = path.parent().ok_or_else(|| {
            PocError::Integrity(format!("fixture file has no parent: {}", path.display()))
        })?;
        if !parent.is_dir() {
            return Err(PocError::Integrity(format!(
                "fixture file parent is missing: {}",
                parent.display()
            )));
        }
        let content_digest = if sparse {
            write_sparse_file(&path, size, seed)?
        } else {
            write_dense_file(&path, size, seed)?
        };
        let metadata =
            fs::metadata(&path).map_err(|error| PocError::io("stat fixture file", &path, error))?;
        self.observed_paths = self.observed_paths.saturating_add(1);
        self.unique_inodes = self.unique_inodes.saturating_add(1);
        self.regular_files = self.regular_files.saturating_add(1);
        self.logical_bytes = self.logical_bytes.saturating_add(size);
        self.allocated_bytes = self
            .allocated_bytes
            .saturating_add(metadata_allocated_bytes(&metadata));
        if sparse {
            self.sparse_files = self.sparse_files.saturating_add(1);
        }
        self.record(b"f", relative, size, &content_digest);
        Ok(())
    }

    fn create_symlink(&mut self, relative: &Path, target: &Path) -> PocResult<()> {
        let path = self.root.join(relative);
        #[cfg(unix)]
        symlink(target, &path)
            .map_err(|error| PocError::io("create fixture symlink", &path, error))?;
        #[cfg(not(unix))]
        return Err(PocError::Unsupported(
            "fixture symlinks require Unix".to_owned(),
        ));
        self.observed_paths = self.observed_paths.saturating_add(1);
        self.unique_inodes = self.unique_inodes.saturating_add(1);
        self.symlinks = self.symlinks.saturating_add(1);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| PocError::io("stat fixture symlink", &path, error))?;
        self.allocated_bytes = self
            .allocated_bytes
            .saturating_add(metadata_allocated_bytes(&metadata));
        self.record(b"l", relative, 0, raw_os_bytes(target.as_os_str()));
        Ok(())
    }

    fn create_hardlink(&mut self, source: &Path, relative: &Path) -> PocResult<()> {
        let source_path = self.root.join(source);
        let path = self.root.join(relative);
        fs::hard_link(&source_path, &path)
            .map_err(|error| PocError::io("create fixture hardlink", &path, error))?;
        let size = fs::metadata(&path)
            .map_err(|error| PocError::io("stat fixture hardlink", &path, error))?
            .len();
        self.observed_paths = self.observed_paths.saturating_add(1);
        self.regular_files = self.regular_files.saturating_add(1);
        self.hardlink_aliases = self.hardlink_aliases.saturating_add(1);
        self.record(b"h", relative, size, raw_os_bytes(source.as_os_str()));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn try_write_xattr(&mut self, relative: &Path, name: &[u8], value: &[u8]) -> PocResult<()> {
        use rustix::fs::XattrFlags;

        let path = self.root.join(relative);
        let before = fs::metadata(&path)
            .map_err(|error| PocError::io("stat fixture before xattr", &path, error))?;
        match rustix::fs::setxattr(&path, name, value, XattrFlags::empty()) {
            Ok(()) => {
                let after = fs::metadata(&path)
                    .map_err(|error| PocError::io("stat fixture after xattr", &path, error))?;
                self.allocated_bytes = self.allocated_bytes.saturating_add(
                    metadata_allocated_bytes(&after)
                        .saturating_sub(metadata_allocated_bytes(&before)),
                );
                self.xattrs_written = self.xattrs_written.saturating_add(1);
                self.record(
                    b"x",
                    relative,
                    u64::try_from(value.len()).unwrap_or(u64::MAX),
                    value,
                );
                Ok(())
            }
            Err(error)
                if matches!(
                    error,
                    rustix::io::Errno::NOTSUP | rustix::io::Errno::PERM | rustix::io::Errno::ACCESS
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(PocError::io("write fixture xattr", &path, error.into())),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn try_write_xattr(&mut self, _relative: &Path, _name: &[u8], _value: &[u8]) -> PocResult<()> {
        Ok(())
    }

    fn record(&mut self, kind: &[u8], relative: &Path, size: u64, payload: &[u8]) {
        let path = raw_os_bytes(relative.as_os_str());
        self.digest.update(kind);
        self.digest
            .update(u64::try_from(path.len()).unwrap_or(u64::MAX).to_be_bytes());
        self.digest.update(path);
        self.digest.update(size.to_be_bytes());
        self.digest.update(
            u64::try_from(payload.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        self.digest.update(payload);
    }

    fn finish(mut self, started: Instant) -> PocResult<FixtureReceipt> {
        if self.observed_paths != self.plan.declared_paths {
            return Err(PocError::Integrity(format!(
                "{} generated {} paths, expected {}",
                self.plan.fixture_id.as_str(),
                self.observed_paths,
                self.plan.declared_paths
            )));
        }
        if self.plan.declared_logical_bytes != 0
            && self.logical_bytes != self.plan.declared_logical_bytes
        {
            return Err(PocError::Integrity(format!(
                "{} generated {} logical bytes, expected {}",
                self.plan.fixture_id.as_str(),
                self.logical_bytes,
                self.plan.declared_logical_bytes
            )));
        }
        self.directories.sort_by(|left, right| {
            right
                .components()
                .count()
                .cmp(&left.components().count())
                .then_with(|| left.cmp(right))
        });
        self.directories.dedup();
        for directory in &self.directories {
            let metadata = fs::metadata(directory)
                .map_err(|error| PocError::io("stat fixture directory", directory, error))?;
            self.allocated_bytes = self
                .allocated_bytes
                .saturating_add(metadata_allocated_bytes(&metadata));
            File::open(directory)
                .map_err(|error| PocError::io("open fixture directory", directory, error))?
                .sync_all()
                .map_err(|error| PocError::io("fsync fixture directory", directory, error))?;
        }
        let digest = hex_digest(self.digest.finalize());
        Ok(FixtureReceipt {
            schema_version: SCHEMA_VERSION,
            fixture_id: self.plan.fixture_id,
            tier: self.plan.tier,
            root: self.root.to_path_buf(),
            declared_paths: self.plan.declared_paths,
            observed_paths: self.observed_paths,
            logical_bytes: self.logical_bytes,
            allocated_bytes: self.allocated_bytes,
            unique_inodes: self.unique_inodes,
            regular_files: self.regular_files,
            directories: u64::try_from(self.directories.len()).unwrap_or(u64::MAX),
            symlinks: self.symlinks,
            sparse_files: self.sparse_files,
            hardlink_aliases: self.hardlink_aliases,
            xattrs_written: self.xattrs_written,
            generated_sha256: digest,
            build_duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            stream_buffer_bytes: STREAM_BYTES as u64,
        })
    }
}

fn write_dense_file(path: &Path, size: u64, seed: u64) -> PocResult<Vec<u8>> {
    let mut file = create_new(path)?;
    let mut hasher = Sha256::new();
    let mut state = seed.max(1);
    let mut remaining = size;
    let mut buffer = [0_u8; STREAM_BYTES];
    while remaining > 0 {
        let count = usize::try_from(remaining.min(STREAM_BYTES as u64)).unwrap_or(STREAM_BYTES);
        fill_deterministic(&mut buffer[..count], &mut state);
        file.write_all(&buffer[..count])
            .map_err(|error| PocError::io("write dense fixture file", path, error))?;
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    file.sync_all()
        .map_err(|error| PocError::io("fsync dense fixture file", path, error))?;
    Ok(hasher.finalize().to_vec())
}

fn write_sparse_file(path: &Path, size: u64, seed: u64) -> PocResult<Vec<u8>> {
    const REGION_BYTES: u64 = MIB;
    let mut file = create_new(path)?;
    file.set_len(size)
        .map_err(|error| PocError::io("size sparse fixture file", path, error))?;
    let mut offset = 0_u64;
    let mut region = 0_u64;
    let mut buffer = [0_u8; STREAM_BYTES];
    let zeroes = [0_u8; STREAM_BYTES];
    let mut hasher = Sha256::new();
    while offset < size {
        let region_end = (offset + REGION_BYTES).min(size);
        if region % 8 != 7 {
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| PocError::io("seek sparse fixture file", path, error))?;
            let mut state = (seed ^ region.wrapping_mul(0x9e37_79b9_7f4a_7c15)).max(1);
            let mut cursor = offset;
            while cursor < region_end {
                let count = usize::try_from((region_end - cursor).min(STREAM_BYTES as u64))
                    .unwrap_or(STREAM_BYTES);
                fill_deterministic(&mut buffer[..count], &mut state);
                file.write_all(&buffer[..count])
                    .map_err(|error| PocError::io("write sparse fixture region", path, error))?;
                hasher.update(&buffer[..count]);
                cursor += count as u64;
            }
        } else {
            let mut cursor = offset;
            while cursor < region_end {
                let count = usize::try_from((region_end - cursor).min(STREAM_BYTES as u64))
                    .unwrap_or(STREAM_BYTES);
                hasher.update(&zeroes[..count]);
                cursor += count as u64;
            }
        }
        offset = region_end;
        region += 1;
    }
    file.sync_all()
        .map_err(|error| PocError::io("fsync sparse fixture file", path, error))?;
    Ok(hasher.finalize().to_vec())
}

fn create_new(path: &Path) -> PocResult<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| PocError::io("create fixture file", path, error))
}

fn fill_deterministic(buffer: &mut [u8], state: &mut u64) {
    for chunk in buffer.chunks_mut(8) {
        *state ^= *state >> 12;
        *state ^= *state << 25;
        *state ^= *state >> 27;
        let bytes = state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
}

fn distributed_size(total: u64, count: u64, index: u64) -> u64 {
    let base = total / count;
    base + u64::from(index < total % count)
}

const fn delta_size(tier: FixtureTier) -> u64 {
    match tier {
        FixtureTier::Smoke => MIB,
        FixtureTier::Heavy => 4 * MIB,
    }
}

fn seed_for(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |state, byte| {
        (state ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

#[cfg(unix)]
fn make_executable(path: &Path) -> PocResult<()> {
    let mut permissions = fs::metadata(path)
        .map_err(|error| PocError::io("stat executable fixture", path, error))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|error| PocError::io("set executable fixture mode", path, error))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> PocResult<()> {
    Ok(())
}

#[cfg(unix)]
fn metadata_allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn metadata_allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

#[cfg(unix)]
fn raw_os_bytes(value: &OsStr) -> &[u8] {
    value.as_bytes()
}

#[cfg(not(unix))]
fn raw_os_bytes(value: &OsStr) -> &[u8] {
    value.to_str().unwrap_or_default().as_bytes()
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}
