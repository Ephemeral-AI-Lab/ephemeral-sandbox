//! In-memory per-root substitution map and rewritten-lease acquisition.
//!
//! Squash commits record `{S layer → replaced raw run}` here; the map lives
//! beside the lease registry (the `shared_registry_for_root` precedent), is
//! keyed by canonical storage root, and dies with the daemon — no sidecar,
//! no schema version. Rewrite is an oldest-generation-first contraction:
//! entries are applied in recording order, so raw runs containing earlier
//! `S` ids compose across generations in one bounded pass. A missing entry
//! degrades that substitution to identity, never a wrong chain.
//!
//! The map mutex is a leaf lock: it is never held while acquiring the
//! storage writer lock or the lease-registry mutex.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::LayerStackError;
use crate::fs::{canonical_key, resolve_layer_path};
use crate::model::{LayerRef, Manifest};

use super::registry::lock_shared_registry;
use crate::stack::{LayerStack, Lease};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubstitutionRecord {
    pub(crate) s_layer: LayerRef,
    pub(crate) raw_run: Vec<LayerRef>,
}

/// Outcome of [`LayerStack::acquire_rewritten_lease`]: either the lease's
/// chain contracts to a shorter equivalent (a replacement lease is acquired;
/// the old lease is never released here), or nothing applies.
#[derive(Debug)]
pub enum RewrittenLease {
    Identity,
    Replaced(Lease),
}

pub(crate) fn reset_shared_substitutions_for_tests() {
    shared_substitutions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

impl LayerStack {
    /// Record one committed block's `{S layer → replaced raw run}` in this
    /// root's in-memory substitution map.
    pub(crate) fn record_substitution(&self, s_layer: LayerRef, raw_run: Vec<LayerRef>) {
        self.substitutions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(SubstitutionRecord { s_layer, raw_run });
    }

    /// Contract `lease`'s manifest through the substitution map, validate the
    /// rewritten layers alive, and acquire the replacement lease under one
    /// shared writer-lock guard — or return [`RewrittenLease::Identity`].
    ///
    /// The old lease is never released by this call (pin-overlap: no instant
    /// exists where either chain is unpinned).
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the writer lock or lease registry is
    /// unavailable; contraction and validation failures degrade to
    /// [`RewrittenLease::Identity`] instead of erroring.
    pub fn acquire_rewritten_lease(
        &self,
        lease: &Lease,
        owner_request_id: &str,
    ) -> Result<RewrittenLease, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let records = self
            .substitutions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let rewritten = contract_layers(&lease.manifest.layers, &records);
        if rewritten == lease.manifest.layers {
            return Ok(RewrittenLease::Identity);
        }
        for layer in &rewritten {
            if !resolve_layer_path(&self.storage_root, &layer.path).is_dir() {
                return Ok(RewrittenLease::Identity);
            }
        }
        let manifest = Manifest::new(
            lease.manifest.version,
            rewritten,
            lease.manifest.schema_version,
        )?;
        let record = {
            let mut leases = lock_shared_registry(&self.leases)?;
            leases.acquire(manifest.clone(), owner_request_id)?
        };
        let layer_paths = manifest
            .layers
            .iter()
            .map(|layer| resolve_layer_path(&self.storage_root, &layer.path))
            .collect();
        Ok(RewrittenLease::Replaced(Lease {
            lease_id: record.lease_id,
            manifest,
            layer_paths,
        }))
    }
}

/// Oldest-generation-first contraction: apply every recorded substitution in
/// recording order, replacing a still-contiguous raw run with its `S` layer.
/// Layer ids are unique within a manifest, so each run matches at most once;
/// the single bounded pass terminates by construction.
fn contract_layers(layers: &[LayerRef], records: &[SubstitutionRecord]) -> Vec<LayerRef> {
    let mut current = layers.to_vec();
    for record in records {
        if record.raw_run.is_empty() {
            continue;
        }
        if let Some(start) = find_run(&current, &record.raw_run) {
            current.splice(
                start..start + record.raw_run.len(),
                std::iter::once(record.s_layer.clone()),
            );
        }
    }
    current
}

fn find_run(haystack: &[LayerRef], needle: &[LayerRef]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub(in crate::stack) type SubstitutionMap = Arc<Mutex<Vec<SubstitutionRecord>>>;

pub(in crate::stack) fn shared_substitutions_for_root(storage_root: &Path) -> SubstitutionMap {
    let key = canonical_key(storage_root);
    shared_substitutions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(key)
        .or_default()
        .clone()
}

type SharedSubstitutionMaps = Mutex<HashMap<String, Arc<Mutex<Vec<SubstitutionRecord>>>>>;

fn shared_substitutions() -> &'static SharedSubstitutionMaps {
    static MAPS: OnceLock<SharedSubstitutionMaps> = OnceLock::new();
    MAPS.get_or_init(|| Mutex::new(HashMap::new()))
}
