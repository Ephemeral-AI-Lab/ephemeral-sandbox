use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::model::WorkspaceProfileId;

pub const FIXTURE_GENERATOR_VERSION: u32 = 1;
pub const WORKSPACE_PROFILE_SCHEMA_VERSION: u32 = 1;
pub const WORKSPACE_PROFILE_CATALOG_SCHEMA_VERSION: u32 = 1;
const MAX_PROFILE_FILES: u64 = 1_000_000;
const MAX_PROFILE_LOGICAL_BYTES: u64 = 1 << 40;
const MAX_PROFILE_DEPTH: u32 = 64;
const MAX_PROFILE_LABEL_BYTES: usize = 80;
const MAX_PROFILE_HELP_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceFixtureSpec {
    pub file_count: u64,
    pub logical_bytes: u64,
    pub maximum_depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceProfileEnvelope {
    pub schema_version: u32,
    pub id: WorkspaceProfileId,
    pub version: u32,
    pub label: String,
    pub help: String,
    pub generator_version: u32,
    pub standard: bool,
    pub fixture: WorkspaceFixtureSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceProfileCatalog {
    pub schema_version: u32,
    pub profiles: Vec<WorkspaceProfileEnvelope>,
}

impl WorkspaceProfileCatalog {
    #[must_use]
    pub fn get(&self, id: &WorkspaceProfileId) -> Option<&WorkspaceProfileEnvelope> {
        self.profiles
            .binary_search_by(|profile| profile.id.cmp(id))
            .ok()
            .map(|index| &self.profiles[index])
    }

    pub fn validate(&self) -> Result<(), FixtureError> {
        self.validate_at(Path::new("<workspace-profile-catalog>"))
    }

    fn validate_at(&self, source: &Path) -> Result<(), FixtureError> {
        if self.schema_version != WORKSPACE_PROFILE_CATALOG_SCHEMA_VERSION {
            return Err(invalid_profile(
                source,
                format!(
                    "catalog schema_version must be {WORKSPACE_PROFILE_CATALOG_SCHEMA_VERSION}"
                ),
            ));
        }
        if self.profiles.is_empty() {
            return Err(FixtureError::EmptyCatalog(source.to_path_buf()));
        }
        for profile in &self.profiles {
            validate_profile(profile, source)?;
        }
        for profiles in self.profiles.windows(2) {
            match profiles[0].id.cmp(&profiles[1].id) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => {
                    return Err(FixtureError::DuplicateProfile {
                        directory: source.to_path_buf(),
                        id: profiles[0].id.clone(),
                    });
                }
                std::cmp::Ordering::Greater => {
                    return Err(invalid_profile(
                        source,
                        "catalog profiles must be sorted by id",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureManifest {
    pub schema_version: u32,
    pub fixture_hash: String,
    pub tree_hash: String,
    pub generator_version: u32,
    pub seed: u64,
    pub profile_id: WorkspaceProfileId,
    pub profile_version: u32,
    pub requested_file_count: u64,
    pub actual_file_count: u64,
    pub requested_logical_bytes: u64,
    pub actual_logical_bytes: u64,
    pub allocated_bytes: AvailabilityU64,
    pub directory_count: u64,
    pub requested_maximum_depth: u32,
    pub actual_maximum_depth: u32,
    pub small_text_files: u64,
    pub medium_binary_files: u64,
    pub large_binary_files: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
pub enum AvailabilityU64 {
    Available { value: u64 },
    Unavailable { reason: String },
}

#[derive(Debug, Clone)]
pub struct MaterializedFixture {
    pub path: PathBuf,
    pub manifest: FixtureManifest,
    pub reused: bool,
}

#[derive(Debug, Error)]
pub enum FixtureError {
    #[error(transparent)]
    Config(#[from] sandbox_config::ConfigError),
    #[error("workspace profile at {path} is invalid: {message}")]
    InvalidProfile { path: PathBuf, message: String },
    #[error("workspace profile catalog is empty: {0}")]
    EmptyCatalog(PathBuf),
    #[error("workspace profile id {id} is duplicated in {directory}")]
    DuplicateProfile {
        directory: PathBuf,
        id: WorkspaceProfileId,
    },
    #[error("fixture cache at {0} failed identity or integrity validation")]
    InvalidCache(PathBuf),
    #[error("fixture JSON failed at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("fixture filesystem operation failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[must_use]
pub fn default_workspace_profile_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../defaults/workspace-profiles")
}

pub fn load_workspace_profiles(directory: &Path) -> Result<WorkspaceProfileCatalog, FixtureError> {
    let mut paths = fs::read_dir(directory)
        .map_err(|source| io_error(directory, source))?
        .map(|entry| {
            entry
                .map(|value| value.path())
                .map_err(|source| io_error(directory, source))
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| {
        matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yml" | "yaml")
        )
    });
    paths.sort();
    if paths.is_empty() {
        return Err(FixtureError::EmptyCatalog(directory.to_path_buf()));
    }

    let mut profiles = Vec::with_capacity(paths.len());
    for path in paths {
        let profile: WorkspaceProfileEnvelope = sandbox_config::load_path(&path)?.document()?;
        validate_profile(&profile, &path)?;
        if path.file_stem().and_then(|value| value.to_str()) != Some(profile.id.as_str()) {
            return Err(invalid_profile(
                &path,
                "file name must match the profile id",
            ));
        }
        profiles.push(profile);
    }
    profiles.sort_by(|left, right| left.id.cmp(&right.id));
    let catalog = WorkspaceProfileCatalog {
        schema_version: WORKSPACE_PROFILE_CATALOG_SCHEMA_VERSION,
        profiles,
    };
    catalog.validate_at(directory)?;
    Ok(catalog)
}

fn validate_profile(profile: &WorkspaceProfileEnvelope, path: &Path) -> Result<(), FixtureError> {
    if profile.schema_version != WORKSPACE_PROFILE_SCHEMA_VERSION {
        return Err(invalid_profile(
            path,
            format!("schema_version must be {WORKSPACE_PROFILE_SCHEMA_VERSION}"),
        ));
    }
    if profile.version == 0 {
        return Err(invalid_profile(path, "version must be positive"));
    }
    if profile.generator_version != FIXTURE_GENERATOR_VERSION {
        return Err(invalid_profile(
            path,
            format!("generator_version must be {FIXTURE_GENERATOR_VERSION}"),
        ));
    }
    if profile.label.trim().is_empty() || profile.label.len() > MAX_PROFILE_LABEL_BYTES {
        return Err(invalid_profile(
            path,
            format!("label must contain 1 to {MAX_PROFILE_LABEL_BYTES} bytes"),
        ));
    }
    if profile.help.trim().is_empty() || profile.help.len() > MAX_PROFILE_HELP_BYTES {
        return Err(invalid_profile(
            path,
            format!("help must contain 1 to {MAX_PROFILE_HELP_BYTES} bytes"),
        ));
    }
    if !(1..=MAX_PROFILE_FILES).contains(&profile.fixture.file_count) {
        return Err(invalid_profile(
            path,
            format!("fixture.file_count must be between 1 and {MAX_PROFILE_FILES}"),
        ));
    }
    if profile.fixture.logical_bytes < profile.fixture.file_count
        || profile.fixture.logical_bytes > MAX_PROFILE_LOGICAL_BYTES
    {
        return Err(invalid_profile(
            path,
            format!(
                "fixture.logical_bytes must be at least fixture.file_count and at most {MAX_PROFILE_LOGICAL_BYTES}"
            ),
        ));
    }
    if profile.fixture.maximum_depth > MAX_PROFILE_DEPTH {
        return Err(invalid_profile(
            path,
            format!("fixture.maximum_depth must be at most {MAX_PROFILE_DEPTH}"),
        ));
    }
    Ok(())
}

fn invalid_profile(path: &Path, message: impl Into<String>) -> FixtureError {
    FixtureError::InvalidProfile {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

pub fn materialize(
    fixtures_root: &Path,
    profile: &WorkspaceProfileEnvelope,
    seed: u64,
) -> Result<MaterializedFixture, FixtureError> {
    validate_profile(profile, Path::new("<in-memory-profile>"))?;
    let fixture = profile.fixture;
    let fixture_hash = fixture_identity(profile, seed)?;
    let profile_root = fixtures_root.join(profile.id.as_str());
    fs::create_dir_all(&profile_root).map_err(|source| io_error(&profile_root, source))?;
    let final_path = profile_root.join(&fixture_hash);
    if final_path.is_dir() {
        let manifest = read_manifest(&final_path)?;
        if manifest.fixture_hash != fixture_hash
            || manifest.generator_version != FIXTURE_GENERATOR_VERSION
            || manifest.seed != seed
            || manifest.profile_id != profile.id
            || manifest.profile_version != profile.version
            || manifest.requested_file_count != fixture.file_count
            || manifest.actual_file_count != fixture.file_count
            || manifest.requested_logical_bytes != fixture.logical_bytes
            || manifest.actual_logical_bytes != fixture.logical_bytes
            || manifest.requested_maximum_depth != fixture.maximum_depth
        {
            return Err(FixtureError::InvalidCache(final_path));
        }
        return Ok(MaterializedFixture {
            path: final_path,
            manifest,
            reused: true,
        });
    }

    let staging = profile_root.join(format!(
        ".fixture-{fixture_hash}.tmp-{}",
        std::process::id()
    ));
    fs::create_dir(&staging).map_err(|source| io_error(&staging, source))?;
    let result = build_fixture(&staging, profile, seed, fixture_hash.clone());
    let manifest = match result {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    fs::rename(&staging, &final_path).map_err(|source| io_error(&final_path, source))?;
    sync_directory(&profile_root)?;
    make_read_only(&final_path)?;
    Ok(MaterializedFixture {
        path: final_path,
        manifest,
        reused: false,
    })
}

fn build_fixture(
    staging: &Path,
    profile: &WorkspaceProfileEnvelope,
    seed: u64,
    fixture_hash: String,
) -> Result<FixtureManifest, FixtureError> {
    let fixture = profile.fixture;
    let small_count = fixture.file_count * 80 / 100;
    let medium_count = fixture.file_count * 15 / 100;
    let large_count = fixture.file_count - small_count - medium_count;
    let total_weight =
        u128::from(small_count) + u128::from(medium_count) * 16 + u128::from(large_count) * 64;
    let mut remaining_bytes = fixture.logical_bytes;
    let mut remaining_weight = total_weight;
    let mut tree_hasher = Sha256::new();
    let mut directories = BTreeSet::new();
    let mut actual_depth = 0_u32;
    let mut allocated = 0_u64;
    let mut allocated_available = true;
    let mut rng = SplitMix64::new(seed);

    for index in 0..fixture.file_count {
        let (weight, extension, text) = if index < small_count {
            (1_u128, "txt", true)
        } else if index < small_count + medium_count {
            (16_u128, "bin", false)
        } else {
            (64_u128, "bin", false)
        };
        let bytes = if index + 1 == fixture.file_count {
            remaining_bytes
        } else {
            ((u128::from(remaining_bytes) * weight) / remaining_weight)
                .max(1)
                .min(u128::from(
                    remaining_bytes - (fixture.file_count - index - 1),
                )) as u64
        };
        remaining_bytes -= bytes;
        remaining_weight -= weight;

        let depth = if fixture.maximum_depth == 0 {
            0
        } else {
            1 + (index as u32 % fixture.maximum_depth)
        };
        actual_depth = actual_depth.max(depth);
        let relative = relative_file_path(index, depth, extension);
        let parent = relative.parent().unwrap_or(Path::new(""));
        let mut accumulated = PathBuf::new();
        for component in parent.components() {
            accumulated.push(component);
            directories.insert(accumulated.clone());
        }
        let path = staging.join(&relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        }
        let content_hash = write_content(&path, bytes, text, &mut rng)?;
        tree_hasher.update(relative.to_string_lossy().as_bytes());
        tree_hasher.update([0]);
        tree_hasher.update(bytes.to_le_bytes());
        tree_hasher.update(content_hash);
        match allocated_bytes(&path) {
            Some(value) => allocated = allocated.saturating_add(value),
            None => allocated_available = false,
        }
    }

    let manifest = FixtureManifest {
        schema_version: 1,
        fixture_hash,
        tree_hash: format!("sha256:{}", hex(&tree_hasher.finalize())),
        generator_version: FIXTURE_GENERATOR_VERSION,
        seed,
        profile_id: profile.id.clone(),
        profile_version: profile.version,
        requested_file_count: fixture.file_count,
        actual_file_count: fixture.file_count,
        requested_logical_bytes: fixture.logical_bytes,
        actual_logical_bytes: fixture.logical_bytes,
        allocated_bytes: if allocated_available {
            AvailabilityU64::Available { value: allocated }
        } else {
            AvailabilityU64::Unavailable {
                reason: "allocated_byte_counter_unavailable".to_owned(),
            }
        },
        directory_count: directories.len() as u64,
        requested_maximum_depth: fixture.maximum_depth,
        actual_maximum_depth: actual_depth,
        small_text_files: small_count,
        medium_binary_files: medium_count,
        large_binary_files: large_count,
    };
    let manifest_path = staging.join("fixture-manifest.json");
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|source| FixtureError::Json {
        path: manifest_path.clone(),
        source,
    })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manifest_path)
        .map_err(|source| io_error(&manifest_path, source))?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error(&manifest_path, source))?;
    sync_tree_directories(staging)?;
    Ok(manifest)
}

fn fixture_identity(profile: &WorkspaceProfileEnvelope, seed: u64) -> Result<String, FixtureError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        profile_id: &'a WorkspaceProfileId,
        profile_version: u32,
        generator_version: u32,
        fixture: WorkspaceFixtureSpec,
        seed: u64,
    }
    let value = serde_json::to_vec(&Identity {
        profile_id: &profile.id,
        profile_version: profile.version,
        generator_version: profile.generator_version,
        fixture: profile.fixture,
        seed,
    })
    .map_err(|source| FixtureError::Json {
        path: PathBuf::from("<fixture-identity>"),
        source,
    })?;
    Ok(format!("sha256:{}", hex(&Sha256::digest(value))))
}

fn write_content(
    path: &Path,
    length: u64,
    text: bool,
    rng: &mut SplitMix64,
) -> Result<Vec<u8>, FixtureError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    let mut hasher = Sha256::new();
    let mut remaining = length;
    let mut buffer = vec![0_u8; 64 * 1024];
    while remaining > 0 {
        let count = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        for byte in &mut buffer[..count] {
            let random = rng.next();
            *byte = if text {
                const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789 \n";
                ALPHABET[(random % ALPHABET.len() as u64) as usize]
            } else {
                random as u8
            };
        }
        file.write_all(&buffer[..count])
            .map_err(|source| io_error(path, source))?;
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    file.sync_all().map_err(|source| io_error(path, source))?;
    Ok(hasher.finalize().to_vec())
}

fn relative_file_path(index: u64, depth: u32, extension: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for level in 0..depth {
        let bucket = index
            .wrapping_mul(131_u64.wrapping_add(u64::from(level)))
            .wrapping_add(u64::from(level) * 17)
            % 97;
        path.push(format!("d{level:02}-{bucket:03}"));
    }
    path.push(format!("file-{index:08}.{extension}"));
    path
}

fn read_manifest(path: &Path) -> Result<FixtureManifest, FixtureError> {
    let manifest_path = path.join("fixture-manifest.json");
    let mut bytes = Vec::new();
    File::open(&manifest_path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|source| io_error(&manifest_path, source))?;
    serde_json::from_slice(&bytes).map_err(|source| FixtureError::Json {
        path: manifest_path,
        source,
    })
}

fn sync_tree_directories(root: &Path) -> Result<(), FixtureError> {
    let mut directories = vec![root.to_path_buf()];
    let mut index = 0;
    while index < directories.len() {
        let directory = directories[index].clone();
        for entry in fs::read_dir(&directory).map_err(|source| io_error(&directory, source))? {
            let entry = entry.map_err(|source| io_error(&directory, source))?;
            if entry
                .file_type()
                .map_err(|source| io_error(&entry.path(), source))?
                .is_dir()
            {
                directories.push(entry.path());
            }
        }
        index += 1;
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        sync_directory(&directory)?;
    }
    Ok(())
}

fn make_read_only(root: &Path) -> Result<(), FixtureError> {
    let mut paths = vec![root.to_path_buf()];
    let mut index = 0;
    while index < paths.len() {
        let path = paths[index].clone();
        if path.is_dir() {
            for entry in fs::read_dir(&path).map_err(|source| io_error(&path, source))? {
                let entry = entry.map_err(|source| io_error(&path, source))?;
                paths.push(entry.path());
            }
        }
        index += 1;
    }
    paths.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in paths {
        let mut permissions = fs::metadata(&path)
            .map_err(|source| io_error(&path, source))?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).map_err(|source| io_error(&path, source))?;
    }
    Ok(())
}

#[cfg(unix)]
fn allocated_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;

    fs::metadata(path)
        .ok()
        .map(|metadata| metadata.blocks().saturating_mul(512))
}

#[cfg(not(unix))]
fn allocated_bytes(_path: &Path) -> Option<u64> {
    None
}

fn sync_directory(path: &Path) -> Result<(), FixtureError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

fn io_error(path: &Path, source: io::Error) -> FixtureError {
    FixtureError::Io {
        path: path.to_path_buf(),
        source,
    }
}

struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}
