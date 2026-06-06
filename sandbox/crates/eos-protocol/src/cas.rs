//! Content-addressed store byte-identity (AV-1c) — the crown-jewel correctness
//! surface.
//!
//! Invariant: [`manifest_root_hash`] and [`layer_digest`] must reproduce the
//! live Rust hashes BYTE-FOR-BYTE. A single wrong byte is a silent data
//! divergence that passes every ASCII test. The two hashes are deliberately
//! OPPOSITE on non-ASCII handling:
//!
//! - `manifest_root_hash` serializes with ASCII-only JSON string escaping: every
//!   non-ASCII scalar is `\uXXXX`-escaped (hand-built here, since `serde_json`
//!   emits raw UTF-8 and would diverge).
//! - `layer_digest` hashes RAW UTF-8 path/source bytes with NUL framing.
//!

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::version::MANIFEST_SCHEMA_VERSION;

const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

/// Errors raised while parsing CAS path / manifest values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CasError {
    /// A layer path was absolute, escaped the stack, was empty, or held a NUL.
    #[error("invalid layer path: {0}")]
    InvalidPath(String),
    /// A manifest carried an unsupported `schema_version`.
    #[error("unsupported manifest schema_version: {0}")]
    UnsupportedSchemaVersion(i64),
}

/// A normalized, relative, NUL-free layer path (`api-parse-dont-validate`).
///
/// Construct via [`LayerPath::parse`]; an invalid path is unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayerPath(String);

impl LayerPath {
    /// Normalize a raw path string exactly as Rust `normalize_layer_path`:
    /// `\` -> `/`, strip surrounding whitespace, drop empty / `.` segments,
    /// reject absolute / `..` / NUL / empty-result.
    ///
    /// # Errors
    ///
    /// Returns [`CasError::InvalidPath`] when the normalized path would be
    /// empty, absolute, escaping, or contain a NUL byte.
    pub fn parse(path: &str) -> Result<Self, CasError> {
        let raw = path.replace('\\', "/");
        let raw = raw.trim();
        if raw.contains('\0') {
            return Err(CasError::InvalidPath(path.to_owned()));
        }
        // PurePosixPath: a leading '/' makes the path absolute.
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

    /// The normalized path string.
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

/// One layer reference in a manifest: `{layer_id, path}` (both strings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerRef {
    pub layer_id: String,
    pub path: String,
}

/// The persisted manifest. `version`/`schema_version` are NOT hashed by
/// [`manifest_root_hash`]; only `layers` (in given order) is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: i64,
    pub layers: Vec<LayerRef>,
    pub schema_version: i64,
}

impl Manifest {
    /// Construct a manifest, rejecting any `schema_version` that does not equal
    /// [`MANIFEST_SCHEMA_VERSION`].
    ///
    /// # Errors
    ///
    /// Returns [`CasError::UnsupportedSchemaVersion`] when `schema_version`
    /// does not match [`MANIFEST_SCHEMA_VERSION`].
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

    /// Number of layers (the manifest-depth invariant surface).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.layers.len()
    }
}

/// Append the ASCII-only JSON string escaping of `s` (without surrounding
/// quotes) to `out`: control/quote/backslash use short escapes and every
/// non-ASCII scalar becomes `\uXXXX` (surrogate pairs for non-BMP).
///
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
                // other control chars: lowercase 4-digit \u00XX
                push_u_escape(out, u32::from(c));
            }
            c => {
                let cp = u32::from(c);
                if cp <= 0xFFFF {
                    push_u_escape(out, cp);
                } else {
                    // UTF-16 surrogate pair, both lowercase.
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

/// Build the exact `json.dumps({"layers":[...]}, sort_keys=True,
/// separators=(",",":"))` byte string the manifest root hash is computed over.
fn manifest_layers_json(layers: &[LayerRef]) -> String {
    let mut out = String::from("{\"layers\":[");
    for (i, layer) in layers.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        // sort_keys: "layer_id" < "path" (code-point order), so layer_id first.
        out.push_str("{\"layer_id\":\"");
        push_json_ascii_escaped(&mut out, &layer.layer_id);
        out.push_str("\",\"path\":\"");
        push_json_ascii_escaped(&mut out, &layer.path);
        out.push_str("\"}");
    }
    out.push_str("]}");
    out
}

/// Stable identity hash for a manifest's root view.
///
/// Hashes ONLY `{"layers":...}` in GIVEN order (order-sensitive);
/// `ensure_ascii=True` escaping applied.
#[must_use]
pub fn manifest_root_hash(manifest: &Manifest) -> String {
    let encoded = manifest_layers_json(&manifest.layers);
    let mut hasher = Sha256::new();
    hasher.update(encoded.as_bytes());
    hex_lower(&hasher.finalize())
}

/// A storage-level layer change.
///
/// Tagged union by kind. `path` is the post-normalization form; `Write` carries
/// raw bytes hashed verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerChange {
    /// File write; `content` is hashed RAW (may be empty / contain NUL / binary).
    Write { path: LayerPath, content: Vec<u8> },
    /// File/dir removal (whiteout). No payload hashed.
    Delete { path: LayerPath },
    /// Symlink; `source_path` (the link target) is hashed RAW UTF-8.
    Symlink {
        path: LayerPath,
        source_path: String,
    },
    /// Opaque-directory marker. No payload hashed.
    OpaqueDir { path: LayerPath },
}

impl LayerChange {
    /// The `kind` discriminator string fed to the digest.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Write { .. } => "write",
            Self::Delete { .. } => "delete",
            Self::Symlink { .. } => "symlink",
            Self::OpaqueDir { .. } => "opaque_dir",
        }
    }

    /// The normalized path this change targets.
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

/// Last-write-wins per `path`, then emit in ascending `path` order.
/// Input-order-insensitive (the OPPOSITE of `manifest_root_hash`).
#[must_use]
pub fn aggregate_layer_changes(changes: &[LayerChange]) -> Vec<LayerChange> {
    // BTreeMap gives sorted-by-path emission; insertion overwrites (last-write-wins).
    let mut by_path: BTreeMap<LayerPath, LayerChange> = BTreeMap::new();
    for change in changes.iter().cloned() {
        by_path.insert(change.path().clone(), change);
    }
    by_path.into_values().collect()
}

/// Feed one change's framed bytes into the running digest:
/// `kind ‖ \0 ‖ path ‖ \0 ‖ <payload-or-nothing> ‖ \0`. Trailing `\0` always.
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

/// Per-layer change-set digest: sha256 over `aggregate_layer_changes(changes)`.
#[must_use]
pub fn layer_digest(changes: &[LayerChange]) -> String {
    let mut hasher = Sha256::new();
    for change in aggregate_layer_changes(changes) {
        update_digest(&mut hasher, &change);
    }
    hex_lower(&hasher.finalize())
}

/// Lowercase hex of a digest, matching Rust `hexdigest()`.
fn hex_lower(bytes: &[u8]) -> String {
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
mod tests {
    use super::*;
    use proptest::prelude::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    fn lp(s: &str) -> Result<LayerPath, CasError> {
        LayerPath::parse(s)
    }

    #[test]
    fn ascii_escaper_reproduces_documented_literal() {
        // From RUST-GUIDANCE §2a directed test + fixture manifest_unicode_bmp.
        let layers = vec![LayerRef {
            layer_id: "Lunicodé".to_owned(),
            path: "layers/café".to_owned(),
        }];
        assert_eq!(
            manifest_layers_json(&layers),
            "{\"layers\":[{\"layer_id\":\"Lunicod\\u00e9\",\"path\":\"layers/caf\\u00e9\"}]}"
        );
    }

    #[test]
    fn ascii_escaper_surrogate_pair_for_nonbmp() {
        let layers = vec![LayerRef {
            layer_id: "Lrocket".to_owned(),
            path: "layers/🚀".to_owned(),
        }];
        assert_eq!(
            manifest_layers_json(&layers),
            "{\"layers\":[{\"layer_id\":\"Lrocket\",\"path\":\"layers/\\ud83d\\ude80\"}]}"
        );
    }

    #[test]
    fn escaper_short_escapes_and_control_chars() {
        let mut out = String::new();
        push_json_ascii_escaped(&mut out, "\u{0008}\t\n\u{000C}\r\u{0001}\u{007F}\"\\");
        assert_eq!(out, "\\b\\t\\n\\f\\r\\u0001\\u007f\\\"\\\\");
    }

    #[test]
    fn normalize_layer_path_rules() -> TestResult {
        assert_eq!(lp("a/b/c")?.as_str(), "a/b/c");
        assert_eq!(lp(" a//b/./c ")?.as_str(), "a/b/c");
        assert_eq!(lp("a\\b")?.as_str(), "a/b");
        assert!(LayerPath::parse("/abs").is_err());
        assert!(LayerPath::parse("a/../b").is_err());
        assert!(LayerPath::parse("").is_err());
        assert!(LayerPath::parse("./").is_err());
        assert!(LayerPath::parse("a\0b").is_err());
        Ok(())
    }

    #[test]
    fn manifest_new_rejects_bad_schema() {
        assert!(Manifest::new(0, vec![], 2).is_err());
        assert!(Manifest::new(0, vec![], 1).is_ok());
    }

    #[test]
    fn aggregate_is_idempotent_and_order_insensitive() -> TestResult {
        let changes = vec![
            LayerChange::Write {
                path: lp("z.txt")?,
                content: b"z".to_vec(),
            },
            LayerChange::Delete { path: lp("a.txt")? },
            LayerChange::Symlink {
                path: lp("m")?,
                source_path: "t".to_owned(),
            },
        ];
        let agg = aggregate_layer_changes(&changes);
        assert_eq!(agg, aggregate_layer_changes(&agg));
        let mut reversed = changes;
        reversed.reverse();
        assert_eq!(agg, aggregate_layer_changes(&reversed));
        // sorted by path: a.txt, m, z.txt
        assert_eq!(
            agg.iter().map(|c| c.path().as_str()).collect::<Vec<_>>(),
            vec!["a.txt", "m", "z.txt"]
        );
        Ok(())
    }

    #[test]
    fn aggregate_last_write_wins() -> TestResult {
        let changes = vec![
            LayerChange::Write {
                path: lp("x")?,
                content: b"first".to_vec(),
            },
            LayerChange::Delete { path: lp("x")? },
        ];
        let agg = aggregate_layer_changes(&changes);
        assert_eq!(agg.len(), 1);
        assert_eq!(agg[0].kind(), "delete");
        Ok(())
    }

    // A change over a UNIQUE relative path (no collisions, so order-insensitivity
    // holds — colliding paths would change the last-write-wins survivor).
    fn arb_change_unique() -> impl Strategy<Value = LayerChange> {
        // single-segment lowercase paths keep them unique-able and always valid.
        let path = "[a-z]{1,8}".prop_map(LayerPath);
        prop_oneof![
            (path.clone(), prop::collection::vec(any::<u8>(), 0..32))
                .prop_map(|(path, content)| LayerChange::Write { path, content }),
            path.clone().prop_map(|path| LayerChange::Delete { path }),
            (path.clone(), "[a-z/]{0,16}")
                .prop_map(|(path, source_path)| LayerChange::Symlink { path, source_path }),
            path.prop_map(|path| LayerChange::OpaqueDir { path }),
        ]
    }

    proptest! {
        #[test]
        fn aggregate_idempotent_and_order_insensitive(changes in prop::collection::vec(arb_change_unique(), 0..12)) {
            // Dedup by path so the property's order-insensitivity precondition holds.
            let mut seen = std::collections::HashSet::new();
            let unique: Vec<LayerChange> = changes
                .into_iter()
                .filter(|c| seen.insert(c.path().as_str().to_owned()))
                .collect();
            let agg = aggregate_layer_changes(&unique);
            // idempotent
            prop_assert_eq!(agg.clone(), aggregate_layer_changes(&agg));
            // input-order-insensitive
            let mut shuffled = unique;
            shuffled.reverse();
            prop_assert_eq!(&agg, &aggregate_layer_changes(&shuffled));
            // emitted sorted by path
            let paths: Vec<&str> = agg.iter().map(|c| c.path().as_str()).collect();
            let mut sorted = paths.clone();
            sorted.sort_unstable();
            prop_assert_eq!(paths, sorted);
        }

        #[test]
        fn escaper_output_is_pure_ascii(s in ".*") {
            let mut out = String::new();
            push_json_ascii_escaped(&mut out, &s);
            prop_assert!(out.is_ascii(), "escaper leaked a non-ASCII byte for input {:?}", s);
        }
    }
}
