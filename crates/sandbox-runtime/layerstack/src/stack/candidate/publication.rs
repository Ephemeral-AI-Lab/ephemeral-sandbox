use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sandbox_runtime_layerstack_core::{
    ActorId, BranchId, Digest32, FileNodeId, PublicationId, RootId,
};
use sha2::{Digest, Sha256};

use crate::model::{aggregate_layer_changes, LayerChange};
use crate::{LayerStack, LayerStackError, Sha256Digest};

use super::object_store::LooseObjectStore;
use super::operation::{
    OpenDisposition, OperationJournal, OperationKind, OperationPhase, OperationRequest,
    TerminalOutcome,
};
use super::refs::{Head, NoGcBarrier, RefStore, RefTarget};
use super::seqcdc;
use super::spool::{ChangedPathSpool, MutationAction, MutationRecord};
use super::tree::{
    AttributionFact, FileNodeV3, FileSnapshotV3, MetadataV3, PersistentPages, SegmentDescriptor,
    SegmentKind,
};

const VALIDATION_BRANCH: &[u8] = b"hidden-validation";
const REQUEST_DIGEST_DOMAIN: &[u8] = b"EOS-LS3-HIDDEN-VALIDATION-REQUEST\0";
const SPOOL_MEMORY_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug)]
pub struct HiddenValidationPublication {
    pub publication_id: [u8; 16],
    pub changes: Vec<LayerChange>,
    pub source_layer_dir: PathBuf,
    pub public_root_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HiddenValidationOutcome {
    pub correlation_id: String,
    pub candidate_generation: u64,
    pub matched: bool,
}

enum CandidateMutation {
    Delete,
    Replace(FileNodeId),
    EnsureDirectory,
    OpaqueDirectory,
}

impl LayerStack {
    #[doc(hidden)]
    pub fn publish_hidden_validation(
        &self,
        publication: HiddenValidationPublication,
    ) -> Result<HiddenValidationOutcome, LayerStackError> {
        publish(self, &publication).map_err(|error| {
            LayerStackError::Storage(format!("hidden candidate publication failed: {error}"))
        })
    }

    #[doc(hidden)]
    pub fn hidden_validation_generation(&self) -> Result<Option<u64>, LayerStackError> {
        self.hidden_validation_head()
            .map(|head| head.map(|head| head.generation))
    }

    pub(crate) fn hidden_validation_root(&self) -> Result<Option<RootId>, LayerStackError> {
        self.hidden_validation_head()
            .map(|head| head.map(|head| RootId::new(head.target.root)))
    }

    fn hidden_validation_head(&self) -> Result<Option<Head>, LayerStackError> {
        let branch = BranchId::new(VALIDATION_BRANCH.to_vec()).map_err(|error| {
            LayerStackError::Storage(format!("hidden validation branch is invalid: {error}"))
        })?;
        let mut digest = Sha256Digest;
        let barrier = NoGcBarrier;
        let refs = RefStore::open(
            self.storage_root.clone(),
            &self.writer_lock,
            &barrier,
            &mut digest,
        )
        .map_err(|error| {
            LayerStackError::Storage(format!("open hidden validation refs failed: {error}"))
        })?;
        refs.read_head(&branch, &mut digest).map_err(|error| {
            LayerStackError::Storage(format!("read hidden validation head failed: {error}"))
        })
    }
}

fn publish(
    stack: &LayerStack,
    publication: &HiddenValidationPublication,
) -> Result<HiddenValidationOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let branch = BranchId::new(VALIDATION_BRANCH.to_vec())?;
    let publication_id = PublicationId::new(publication.publication_id)?;
    let mut digest = Sha256Digest;
    let barrier = NoGcBarrier;
    let mut refs = RefStore::open(
        stack.storage_root.clone(),
        &stack.writer_lock,
        &barrier,
        &mut digest,
    )?;
    let journal = OperationJournal::new(stack.storage_root.clone(), &stack.writer_lock);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    journal.recover_batch(&mut refs, now, &mut digest)?;
    let base = refs.read_head(&branch, &mut digest)?;
    let request_digest = validation_request_digest(publication)?;
    let request = OperationRequest {
        kind: OperationKind::HiddenValidation,
        branch,
        publication_id,
        request_digest,
        base,
    };
    let opened = journal.open(&request, now, &mut digest)?;
    if let OpenDisposition::Terminal(outcome) = opened.disposition {
        return outcome_from_terminal(publication, outcome, None, true);
    }
    if opened.state.phase == OperationPhase::Prepared {
        let outcome = journal.commit_success(opened.id, &mut refs, now, &mut digest, |_| Ok(()))?;
        return outcome_from_terminal(publication, outcome, None, true);
    }

    let changes = aggregate_layer_changes(&publication.changes);
    let changed_path_bytes = changed_path_bytes(&changes)?;
    let changed_path_digest =
        journal.stage_changed_path_run(opened.id, &changed_path_bytes, &mut digest, |_| Ok(()))?;
    let mut changed_paths = ChangedPathSpool::new(
        journal.work_path(opened.id).join("semantic-keys"),
        SPOOL_MEMORY_BYTES,
    )?;
    for change in &changes {
        changed_paths.push(MutationRecord {
            path: change.path().as_str().as_bytes().to_vec(),
            action: match change {
                LayerChange::Delete { .. } => MutationAction::Remove,
                LayerChange::OpaqueDir { .. } => MutationAction::OpaqueDirectory,
                LayerChange::Write { .. }
                | LayerChange::WriteFile { .. }
                | LayerChange::Symlink { .. }
                | LayerChange::Directory { .. } => MutationAction::Replace,
            },
            conflict_group: None,
            descriptor: Vec::new(),
        })?;
    }
    let changed_paths = changed_paths.finish()?;

    let store = LooseObjectStore::new(stack.storage_root.clone())?;
    let mut pages = PersistentPages::new(&store);
    let target = build_target(&mut pages, base, publication, &changes, request_digest)?;
    let matched = target_matches(&mut pages, target, publication, &changes)?;
    journal.prepare(opened.id, target, Some(changed_path_digest), 0, &mut digest)?;
    let outcome = journal.commit_success(opened.id, &mut refs, now, &mut digest, |_| Ok(()))?;
    drop(changed_paths);
    outcome_from_terminal(publication, outcome, Some(target), matched)
}

fn build_target(
    pages: &mut PersistentPages<'_>,
    base: Option<Head>,
    publication: &HiddenValidationPublication,
    changes: &[LayerChange],
    request_digest: Digest32,
) -> Result<RefTarget, Box<dyn std::error::Error + Send + Sync>> {
    let mut tree = match base {
        Some(head) => pages.root_directory(RootId::new(head.target.root))?,
        None => pages.build_tree(Vec::new())?,
    };
    let mut attribution = Vec::with_capacity(changes.len());
    let actor = ActorId::new(request_digest.into_bytes())?;
    for change in changes {
        let mutation = mutation_for_change(pages, publication, change)?;
        tree = mutate_path(
            pages,
            tree,
            &change
                .path()
                .as_str()
                .split('/')
                .map(str::as_bytes)
                .collect::<Vec<_>>(),
            mutation,
        )?;
        let length = change_length(change);
        attribution.push(AttributionFact {
            path: change.path().as_str().as_bytes().to_vec(),
            scope: u8::from(length != 0),
            offset: 0,
            length,
            actor,
            publication: publication.publication_id,
        });
    }
    let root_file =
        pages.install_file_node(&FileNodeV3::directory(MetadataV3::directory(0o755), tree))?;
    let root = pages.install_root(root_file)?;
    let attribution_page = pages.build_attribution(attribution)?;
    let attribution_root = pages.install_attribution_root(root, attribution_page)?;
    Ok(RefTarget {
        root: root.digest(),
        attribution_root: attribution_root.digest(),
    })
}

fn target_matches(
    pages: &mut PersistentPages<'_>,
    target: RefTarget,
    publication: &HiddenValidationPublication,
    changes: &[LayerChange],
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let root = RootId::new(target.root);
    for change in changes {
        let file = pages.lookup_path(root, change.path().as_str().as_bytes())?;
        let matched = match change {
            LayerChange::Delete { .. } => file.is_none(),
            LayerChange::Directory { .. } | LayerChange::OpaqueDir { .. } => file
                .map(|file| pages.file_snapshot(file))
                .transpose()?
                .is_some_and(|snapshot| matches!(snapshot, FileSnapshotV3::Directory)),
            LayerChange::Symlink { source_path, .. } => file
                .map(|file| pages.file_snapshot(file))
                .transpose()?
                .is_some_and(
                    |snapshot| matches!(snapshot, FileSnapshotV3::Symlink(target) if target == source_path.as_bytes()),
                ),
            LayerChange::Write { path, .. } | LayerChange::WriteFile { path, .. } => {
                let Some(file) = file else {
                    return Ok(false);
                };
                match pages.file_snapshot(file)? {
                    FileSnapshotV3::Regular {
                        logical_length,
                        segments,
                    } => regular_matches(
                        pages,
                        &publication.source_layer_dir.join(path.as_str()),
                        logical_length,
                        &segments,
                    )?,
                    FileSnapshotV3::Directory
                    | FileSnapshotV3::Symlink(_)
                    | FileSnapshotV3::Other => false,
                }
            }
        };
        if !matched {
            return Ok(false);
        }
    }
    Ok(true)
}

fn regular_matches(
    pages: &mut PersistentPages<'_>,
    source: &Path,
    logical_length: u64,
    segments: &[SegmentDescriptor],
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let mut source_file = File::open(source)?;
    let mut source_hasher = Sha256::new();
    let mut source_length = 0_u64;
    let mut buffer = [0_u8; 32 * 1024];
    loop {
        let count = source_file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        source_hasher.update(&buffer[..count]);
        source_length = source_length
            .checked_add(u64::try_from(count)?)
            .ok_or("source length overflow")?;
    }

    let mut candidate_hasher = Sha256::new();
    let mut candidate_length = 0_u64;
    let zeroes = [0_u8; 32 * 1024];
    let zeroes_len = u64::try_from(zeroes.len())?;
    for segment in segments {
        if segment.offset != candidate_length {
            return Ok(false);
        }
        match segment.kind {
            SegmentKind::Chunk(id) => {
                let chunk = pages.load_chunk(id)?;
                if u64::try_from(chunk.len())? != segment.length {
                    return Ok(false);
                }
                candidate_hasher.update(&chunk);
            }
            SegmentKind::Zero | SegmentKind::Hole => {
                let mut remaining = segment.length;
                while remaining > 0 {
                    let count = remaining.min(zeroes_len);
                    candidate_hasher.update(&zeroes[..usize::try_from(count)?]);
                    remaining -= count;
                }
            }
        }
        candidate_length = candidate_length
            .checked_add(segment.length)
            .ok_or("candidate length overflow")?;
    }
    Ok(source_length == logical_length
        && candidate_length == logical_length
        && source_hasher.finalize() == candidate_hasher.finalize())
}

fn mutation_for_change(
    pages: &mut PersistentPages<'_>,
    publication: &HiddenValidationPublication,
    change: &LayerChange,
) -> Result<CandidateMutation, Box<dyn std::error::Error + Send + Sync>> {
    match change {
        LayerChange::Write { path, .. } | LayerChange::WriteFile { path, .. } => {
            let source = publication.source_layer_dir.join(path.as_str());
            Ok(CandidateMutation::Replace(regular_file(pages, &source)?))
        }
        LayerChange::Delete { .. } => Ok(CandidateMutation::Delete),
        LayerChange::Symlink { source_path, .. } => {
            let node = FileNodeV3::symlink(
                MetadataV3::directory(0o777),
                source_path.as_bytes().to_vec(),
            );
            Ok(CandidateMutation::Replace(pages.install_file_node(&node)?))
        }
        LayerChange::Directory { .. } => Ok(CandidateMutation::EnsureDirectory),
        LayerChange::OpaqueDir { .. } => Ok(CandidateMutation::OpaqueDirectory),
    }
}

fn regular_file(
    pages: &mut PersistentPages<'_>,
    source: &Path,
) -> Result<FileNodeId, Box<dyn std::error::Error + Send + Sync>> {
    let mut file = File::open(source)?;
    let mut offset = 0_u64;
    let mut segments = Vec::new();
    let stats = seqcdc::stream(&mut file, |chunk| {
        let length = u64::try_from(chunk.len()).map_err(|_| "chunk length overflow")?;
        let kind = if chunk.is_all_zero() {
            SegmentKind::Zero
        } else {
            SegmentKind::Chunk(
                pages
                    .install_chunk_slices(chunk.first(), chunk.second())
                    .map_err(|_| "chunk installation failed")?,
            )
        };
        segments.push(SegmentDescriptor {
            offset,
            length,
            kind,
        });
        offset = offset.checked_add(length).ok_or("file length overflow")?;
        Ok::<(), &'static str>(())
    })?;
    let segment_root = pages.build_segments(segments)?;
    Ok(pages.install_file_node(&FileNodeV3::regular(
        MetadataV3::directory(0o644),
        stats.input_bytes,
        segment_root,
        None,
    ))?)
}

fn mutate_path(
    pages: &mut PersistentPages<'_>,
    tree: sandbox_runtime_layerstack_core::TreePageId,
    components: &[&[u8]],
    mutation: CandidateMutation,
) -> Result<sandbox_runtime_layerstack_core::TreePageId, Box<dyn std::error::Error + Send + Sync>> {
    let (name, remaining) = components
        .split_first()
        .ok_or("candidate mutation path is empty")?;
    if remaining.is_empty() {
        let replacement = match mutation {
            CandidateMutation::Delete => None,
            CandidateMutation::Replace(file) => Some(file),
            CandidateMutation::EnsureDirectory => match pages.lookup_tree_entry(tree, name)? {
                Some(file) if pages.file_directory(file)?.is_some() => Some(file),
                _ => Some(empty_directory(pages)?),
            },
            CandidateMutation::OpaqueDirectory => Some(empty_directory(pages)?),
        };
        return Ok(pages.mutate_tree_entry(tree, name, replacement)?);
    }

    let child_tree = match pages.lookup_tree_entry(tree, name)? {
        Some(file) => pages
            .file_directory(file)?
            .unwrap_or(pages.build_tree(Vec::new())?),
        None => pages.build_tree(Vec::new())?,
    };
    let child_tree = mutate_path(pages, child_tree, remaining, mutation)?;
    let child = pages.install_file_node(&FileNodeV3::directory(
        MetadataV3::directory(0o755),
        child_tree,
    ))?;
    Ok(pages.mutate_tree_entry(tree, name, Some(child))?)
}

fn empty_directory(
    pages: &mut PersistentPages<'_>,
) -> Result<FileNodeId, Box<dyn std::error::Error + Send + Sync>> {
    let tree = pages.build_tree(Vec::new())?;
    Ok(pages.install_file_node(&FileNodeV3::directory(MetadataV3::directory(0o755), tree))?)
}

fn validation_request_digest(
    publication: &HiddenValidationPublication,
) -> Result<Digest32, Box<dyn std::error::Error + Send + Sync>> {
    let changes = aggregate_layer_changes(&publication.changes);
    let mut hasher = Sha256::new();
    hasher.update(REQUEST_DIGEST_DOMAIN);
    hasher.update(publication.public_root_hash.as_bytes());
    for change in &changes {
        hasher.update([change_kind(change)]);
        hasher.update((change.path().as_str().len() as u64).to_be_bytes());
        hasher.update(change.path().as_str().as_bytes());
        match change {
            LayerChange::Write { path, .. } | LayerChange::WriteFile { path, .. } => {
                let mut file = File::open(publication.source_layer_dir.join(path.as_str()))?;
                let mut buffer = [0_u8; 32 * 1024];
                loop {
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    hasher.update(&buffer[..count]);
                }
            }
            LayerChange::Symlink { source_path, .. } => hasher.update(source_path.as_bytes()),
            LayerChange::Delete { .. }
            | LayerChange::Directory { .. }
            | LayerChange::OpaqueDir { .. } => {}
        }
    }
    Ok(Digest32::new(hasher.finalize().into()))
}

fn changed_path_bytes(
    changes: &[LayerChange],
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut bytes = Vec::new();
    for change in changes {
        let path = change.path().as_str().as_bytes();
        let length = u16::try_from(path.len())?;
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.push(change_kind(change));
        bytes.extend_from_slice(path);
    }
    Ok(bytes)
}

const fn change_kind(change: &LayerChange) -> u8 {
    match change {
        LayerChange::Write { .. } | LayerChange::WriteFile { .. } => 1,
        LayerChange::Delete { .. } => 2,
        LayerChange::Symlink { .. } => 3,
        LayerChange::Directory { .. } => 4,
        LayerChange::OpaqueDir { .. } => 5,
    }
}

fn change_length(change: &LayerChange) -> u64 {
    match change {
        LayerChange::Write { content, .. } => content.len() as u64,
        LayerChange::WriteFile { size, .. } => *size,
        LayerChange::Symlink { source_path, .. } => source_path.len() as u64,
        LayerChange::Delete { .. }
        | LayerChange::Directory { .. }
        | LayerChange::OpaqueDir { .. } => 0,
    }
}

fn outcome_from_terminal(
    publication: &HiddenValidationPublication,
    outcome: TerminalOutcome,
    expected_target: Option<RefTarget>,
    content_matched: bool,
) -> Result<HiddenValidationOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let head = match outcome {
        TerminalOutcome::Success(head) => head,
        _ => return Err("hidden validation did not produce a success outcome".into()),
    };
    Ok(HiddenValidationOutcome {
        correlation_id: hex(&publication.publication_id),
        candidate_generation: head.generation,
        matched: content_matched && expected_target.is_none_or(|target| head.target == target),
    })
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    output
}
