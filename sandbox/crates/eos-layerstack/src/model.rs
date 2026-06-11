use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MANIFEST_SCHEMA_VERSION: i64 = 1;

const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CasError {
    #[error("invalid layer path: {0}")]
    InvalidPath(String),
    #[error("unsupported manifest schema_version: {0}")]
    UnsupportedSchemaVersion(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayerPath(String);

impl LayerPath {
    pub fn parse(path: &str) -> Result<Self, CasError> {
        let raw = path.replace('\\', "/");
        let raw = raw.trim();
        if raw.contains('\0') {
            return Err(CasError::InvalidPath(path.to_owned()));
        }
        if raw.starts_with('/') {
            return Err(CasError::InvalidPath(path.to_owned()));
        }
        let mut parts: Vec<&str> = Vec::new();
        for part in raw.split('/') {
            if part.is_empty() || part == "." {
                continue;
            }
            if part == ".." {
                return Err(CasError::InvalidPath(path.to_owned()));
            }
            parts.push(part);
        }
        if parts.is_empty() {
            return Err(CasError::InvalidPath(path.to_owned()));
        }
        Ok(Self(parts.join("/")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LayerPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LayerRef {
    pub layer_id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: i64,
    pub layers: Vec<LayerRef>,
    pub schema_version: i64,
}

impl Manifest {
    pub fn new(version: i64, layers: Vec<LayerRef>, schema_version: i64) -> Result<Self, CasError> {
        if schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(CasError::UnsupportedSchemaVersion(schema_version));
        }
        Ok(Self {
            version,
            layers,
            schema_version,
        })
    }

    #[must_use]
    pub fn depth(&self) -> usize {
        self.layers.len()
    }
}

fn push_json_ascii_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{0009}' => out.push_str("\\t"),
            '\u{000A}' => out.push_str("\\n"),
            '\u{000C}' => out.push_str("\\f"),
            '\u{000D}' => out.push_str("\\r"),
            c if (0x20..=0x7E).contains(&u32::from(c)) => out.push(c),
            c if u32::from(c) < 0x20 => {
                push_u_escape(out, u32::from(c));
            }
            c => {
                let cp = u32::from(c);
                if cp <= 0xFFFF {
                    push_u_escape(out, cp);
                } else {
                    let v = cp - 0x10000;
                    let hi = 0xD800 + (v >> 10);
                    let lo = 0xDC00 + (v & 0x3FF);
                    push_u_escape(out, hi);
                    push_u_escape(out, lo);
                }
            }
        }
    }
}

fn push_u_escape(out: &mut String, value: u32) {
    out.push_str("\\u");
    out.push(hex_char((value >> 12) & 0x0f));
    out.push(hex_char((value >> 8) & 0x0f));
    out.push(hex_char((value >> 4) & 0x0f));
    out.push(hex_char(value & 0x0f));
}

fn manifest_layers_json(layers: &[LayerRef]) -> String {
    let mut out = String::from("{\"layers\":[");
    for (i, layer) in layers.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"layer_id\":\"");
        push_json_ascii_escaped(&mut out, &layer.layer_id);
        out.push_str("\",\"path\":\"");
        push_json_ascii_escaped(&mut out, &layer.path);
        out.push_str("\"}");
    }
    out.push_str("]}");
    out
}

#[must_use]
pub fn manifest_root_hash(manifest: &Manifest) -> String {
    let encoded = manifest_layers_json(&manifest.layers);
    let mut hasher = Sha256::new();
    hasher.update(encoded.as_bytes());
    hex_lower(hasher.finalize())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerChange {
    Write {
        path: LayerPath,
        content: Vec<u8>,
    },
    Delete {
        path: LayerPath,
    },
    Symlink {
        path: LayerPath,
        source_path: String,
    },
    OpaqueDir {
        path: LayerPath,
    },
}

impl LayerChange {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Write { .. } => "write",
            Self::Delete { .. } => "delete",
            Self::Symlink { .. } => "symlink",
            Self::OpaqueDir { .. } => "opaque_dir",
        }
    }

    #[must_use]
    pub const fn path(&self) -> &LayerPath {
        match self {
            Self::Write { path, .. }
            | Self::Delete { path }
            | Self::Symlink { path, .. }
            | Self::OpaqueDir { path } => path,
        }
    }
}

#[must_use]
pub fn aggregate_layer_changes(changes: &[LayerChange]) -> Vec<LayerChange> {
    let mut by_path: BTreeMap<LayerPath, LayerChange> = BTreeMap::new();
    for change in changes.iter().cloned() {
        by_path.insert(change.path().clone(), change);
    }
    by_path.into_values().collect()
}

fn update_digest(hasher: &mut Sha256, change: &LayerChange) {
    hasher.update(change.kind().as_bytes());
    hasher.update(b"\0");
    hasher.update(change.path().as_str().as_bytes());
    hasher.update(b"\0");
    match change {
        LayerChange::Write { content, .. } => hasher.update(content),
        LayerChange::Symlink { source_path, .. } => hasher.update(source_path.as_bytes()),
        LayerChange::Delete { .. } | LayerChange::OpaqueDir { .. } => {}
    }
    hasher.update(b"\0");
}

#[must_use]
pub fn layer_digest(changes: &[LayerChange]) -> String {
    let mut hasher = Sha256::new();
    for change in aggregate_layer_changes(changes) {
        update_digest(&mut hasher, &change);
    }
    hex_lower(hasher.finalize())
}

pub(crate) fn hex_lower(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from(LOWER_HEX[usize::from(b >> 4)]));
        s.push(char::from(LOWER_HEX[usize::from(b & 0x0f)]));
    }
    s
}

fn hex_char(nibble: u32) -> char {
    let index = usize::from((nibble & 0x0f) as u8);
    char::from(LOWER_HEX[index])
}

#[cfg(test)]
#[path = "../tests/unit/model.rs"]
mod tests;
