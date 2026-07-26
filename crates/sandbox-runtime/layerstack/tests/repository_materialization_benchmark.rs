#![forbid(unsafe_code)]
#![cfg(target_os = "linux")]

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sandbox_runtime_layerstack::service::{
    lookup_hidden_candidate_generation, materialize_hidden_candidate,
    CandidateMaterializationDisposition,
};
use sandbox_runtime_layerstack::{HiddenValidationPublication, LayerChange, LayerPath, LayerStack};
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CorpusProfile {
    directories: u64,
    files: u64,
    logical_bytes: u64,
}

struct StateRoot(PathBuf);

impl StateRoot {
    fn new() -> TestResult<Self> {
        let path = std::env::temp_dir().join(format!(
            "eos-experiment-materialization-{}",
            std::process::id()
        ));
        match fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StateRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn collect_corpus(source: &Path) -> TestResult<(Vec<LayerChange>, CorpusProfile)> {
    let mut changes = Vec::new();
    let mut profile = CorpusProfile {
        directories: 1,
        ..CorpusProfile::default()
    };
    visit_corpus(source, source, &mut changes, &mut profile)?;
    Ok((changes, profile))
}

fn visit_corpus(
    source: &Path,
    directory: &Path,
    changes: &mut Vec<LayerChange>,
    profile: &mut CorpusProfile,
) -> TestResult {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(source)?
            .to_str()
            .ok_or("corpus contains a non-UTF-8 path")?
            .replace('\\', "/");
        let layer_path = LayerPath::parse(&relative)?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            profile.directories = profile
                .directories
                .checked_add(1)
                .ok_or("directory count overflow")?;
            changes.push(LayerChange::Directory { path: layer_path });
            visit_corpus(source, &path, changes, profile)?;
        } else if file_type.is_file() {
            let size = entry.metadata()?.len();
            profile.files = profile.files.checked_add(1).ok_or("file count overflow")?;
            profile.logical_bytes = profile
                .logical_bytes
                .checked_add(size)
                .ok_or("logical byte count overflow")?;
            changes.push(LayerChange::WriteFile {
                path: layer_path,
                source_path: path,
                size,
            });
        } else if file_type.is_symlink() {
            let target = fs::read_link(&path)?;
            changes.push(LayerChange::Symlink {
                path: layer_path,
                source_path: target
                    .to_str()
                    .ok_or("corpus contains a non-UTF-8 symlink target")?
                    .to_owned(),
            });
        } else {
            return Err(format!("unsupported corpus entry: {}", path.display()).into());
        }
    }
    Ok(())
}

fn profile_tree(root: &Path) -> TestResult<CorpusProfile> {
    let mut profile = CorpusProfile {
        directories: 1,
        ..CorpusProfile::default()
    };
    visit_profile(root, &mut profile)?;
    Ok(profile)
}

fn visit_profile(directory: &Path, profile: &mut CorpusProfile) -> TestResult {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            profile.directories = profile
                .directories
                .checked_add(1)
                .ok_or("directory count overflow")?;
            visit_profile(&entry.path(), profile)?;
        } else if file_type.is_file() {
            profile.files = profile.files.checked_add(1).ok_or("file count overflow")?;
            profile.logical_bytes = profile
                .logical_bytes
                .checked_add(entry.metadata()?.len())
                .ok_or("logical byte count overflow")?;
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> TestResult {
    fs::create_dir(destination)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_tree(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path)?;
        } else if file_type.is_symlink() {
            return Err(format!(
                "preservation does not support symlink output: {}",
                source_path.display()
            )
            .into());
        } else {
            return Err(format!(
                "preservation does not support output entry: {}",
                source_path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn duration_ns(duration: Duration) -> TestResult<u64> {
    Ok(u64::try_from(duration.as_nanos())?)
}

fn mb_per_second(bytes: u64, elapsed: Duration) -> f64 {
    bytes as f64 / 1_000_000.0 / elapsed.as_secs_f64()
}

fn mib_per_second(bytes: u64, elapsed: Duration) -> f64 {
    bytes as f64 / 1_048_576.0 / elapsed.as_secs_f64()
}

#[test]
fn materialize_repository_corpus() -> TestResult {
    let source = PathBuf::from(std::env::var("S04_REPOSITORY_SOURCE")?);
    let results_root = PathBuf::from(std::env::var("S04_BENCHMARK_RESULTS")?);
    let preserved_carrier = results_root.join("materialized-carrier");
    if preserved_carrier.exists() {
        return Err(format!(
            "refusing to overwrite preserved carrier: {}",
            preserved_carrier.display()
        )
        .into());
    }
    fs::create_dir_all(&results_root)?;

    let state = StateRoot::new()?;
    let (changes, source_profile) = collect_corpus(&source)?;
    let stack = LayerStack::open(state.path().to_path_buf())?;
    let publication = HiddenValidationPublication {
        publication_id: *b"EOS-EXP-20260727",
        changes,
        source_layer_dir: source.clone(),
        public_root_hash: format!(
            "experiment-console-release:{}:{}",
            source_profile.files, source_profile.logical_bytes
        ),
    };

    let publication_started = Instant::now();
    let publication_outcome = stack.publish_hidden_validation(publication)?;
    let publication_elapsed = publication_started.elapsed();
    if !publication_outcome.matched {
        return Err("candidate publication did not match the source corpus".into());
    }

    let cold_started = Instant::now();
    let cold = materialize_hidden_candidate(state.path(), Duration::from_secs(900))?;
    let cold_elapsed = cold_started.elapsed();
    if cold.disposition != CandidateMaterializationDisposition::Built {
        return Err(format!(
            "expected cold Built disposition, got {:?}",
            cold.disposition
        )
        .into());
    }
    let carrier_profile = profile_tree(&cold.selection.carrier_path)?;
    if carrier_profile != source_profile {
        return Err(format!(
            "materialized carrier profile differs: source={source_profile:?} carrier={carrier_profile:?}"
        )
        .into());
    }

    let reuse_started = Instant::now();
    let reuse = materialize_hidden_candidate(state.path(), Duration::from_secs(900))?;
    let reuse_elapsed = reuse_started.elapsed();
    if reuse.disposition != CandidateMaterializationDisposition::Reused {
        return Err(format!(
            "expected warm Reused disposition, got {:?}",
            reuse.disposition
        )
        .into());
    }
    if reuse.selection != cold.selection {
        return Err("same-key reuse selected a different generation".into());
    }

    let mut warm_lookup_ns = Vec::new();
    for _ in 0..5 {
        let lookup_started = Instant::now();
        let selection = lookup_hidden_candidate_generation(state.path())?
            .ok_or("warm lookup did not return the selected generation")?;
        warm_lookup_ns.push(duration_ns(lookup_started.elapsed())?);
        if selection != cold.selection {
            return Err("warm lookup selected a different generation".into());
        }
    }

    let preservation_started = Instant::now();
    copy_tree(&cold.selection.carrier_path, &preserved_carrier)?;
    let preservation_elapsed = preservation_started.elapsed();
    let preserved_profile = profile_tree(&preserved_carrier)?;
    if preserved_profile != source_profile {
        return Err(format!(
            "preserved carrier profile differs: source={source_profile:?} preserved={preserved_profile:?}"
        )
        .into());
    }

    warm_lookup_ns.sort_unstable();
    let result = json!({
        "schema_version": 1,
        "corpus": {
            "source": source,
            "directories": source_profile.directories,
            "files": source_profile.files,
            "logical_bytes": source_profile.logical_bytes,
            "mib": source_profile.logical_bytes as f64 / 1_048_576.0,
            "gib": source_profile.logical_bytes as f64 / 1_073_741_824.0
        },
        "publication_cdc_cas": {
            "elapsed_ns": duration_ns(publication_elapsed)?,
            "mb_per_second": mb_per_second(source_profile.logical_bytes, publication_elapsed),
            "mib_per_second": mib_per_second(source_profile.logical_bytes, publication_elapsed),
            "matched": publication_outcome.matched,
            "candidate_generation": publication_outcome.candidate_generation
        },
        "cold_materialization": {
            "elapsed_ns": duration_ns(cold_elapsed)?,
            "mb_per_second": mb_per_second(source_profile.logical_bytes, cold_elapsed),
            "mib_per_second": mib_per_second(source_profile.logical_bytes, cold_elapsed),
            "disposition": "built",
            "maximum_buffer_bytes": cold.maximum_buffer_bytes,
            "generation": cold.selection.generation,
            "manifest_sha256": cold.selection.manifest_sha256,
            "native_tree_sha256": cold.selection.native_tree_sha256
        },
        "same_key_materializer_reuse": {
            "elapsed_ns": duration_ns(reuse_elapsed)?,
            "mb_per_second": mb_per_second(source_profile.logical_bytes, reuse_elapsed),
            "mib_per_second": mib_per_second(source_profile.logical_bytes, reuse_elapsed),
            "disposition": "reused"
        },
        "warm_generation_lookup": {
            "samples_ns": warm_lookup_ns,
            "median_ns": warm_lookup_ns[2]
        },
        "preservation": {
            "carrier": preserved_carrier,
            "elapsed_ns": duration_ns(preservation_elapsed)?,
            "directories": preserved_profile.directories,
            "files": preserved_profile.files,
            "logical_bytes": preserved_profile.logical_bytes
        }
    });
    fs::write(
        results_root.join("benchmark.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!(
        "S04_REPOSITORY_MATERIALIZATION_BENCHMARK={}",
        serde_json::to_string(&result)?
    );
    Ok(())
}
