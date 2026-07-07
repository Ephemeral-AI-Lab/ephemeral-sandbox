//! Winner-fold unit suite: newest-wins selection, whiteout/opaque masking,
//! `MergedView` equivalence over the delta manifest (spec inv 2), and the
//! fold↔flatten winner-selection cross-check (spec decision 14).

use std::path::{Path, PathBuf};

use crate::stack::squash::flatten::flatten_block_into_with_lower;
use crate::stack::{
    delta_layer_refs, describe_layer_delta, fold_delta_winners, DeltaFold, DeltaWinner,
    LayerDeltaDescription, LayerDeltaEntry, LayerDeltaEntryKind,
};
use crate::whiteout::OPAQUE_MARKER;
use crate::{LayerPath, LayerRef, Manifest, MergedView, MANIFEST_SCHEMA_VERSION};

use super::test_fixture::unique_suffix;

struct DeltaFixture {
    base: PathBuf,
}

impl DeltaFixture {
    fn new(label: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("layerstack-export-{label}-{}", unique_suffix()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("layers")).expect("create layers dir");
        Self { base }
    }

    fn root(&self) -> &Path {
        &self.base
    }

    fn layer(&self, name: &str) -> PathBuf {
        let dir = self.base.join("layers").join(name);
        std::fs::create_dir_all(&dir).expect("create layer dir");
        dir
    }

    fn manifest(&self, newest_first: &[&str]) -> Manifest {
        let layers = newest_first
            .iter()
            .map(|name| LayerRef {
                layer_id: (*name).to_owned(),
                path: format!("layers/{name}"),
            })
            .collect();
        Manifest::new(newest_first.len() as i64, layers, MANIFEST_SCHEMA_VERSION).expect("manifest")
    }
}

impl Drop for DeltaFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write fixture file");
}

fn write_logical_whiteout(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).expect("mkdir");
    std::fs::write(dir.join(format!(".wh.{name}")), "").expect("write whiteout");
}

fn write_opaque_marker(dir: &Path) {
    std::fs::create_dir_all(dir).expect("mkdir");
    std::fs::write(dir.join(OPAQUE_MARKER), "").expect("write opaque marker");
}

fn path(p: &str) -> LayerPath {
    LayerPath::parse(p).expect("layer path")
}

#[test]
fn describe_layer_delta_reports_normalized_entries() {
    let fixture = DeltaFixture::new("describe-layer");
    let layer = fixture.layer("L000002-edits");
    write(&layer.join("src/main.rs"), "fn main() {}\n");
    write(&layer.join("src/lib.rs"), "");
    std::os::unix::fs::symlink("main.rs", layer.join("src/link.rs")).expect("symlink");
    write_logical_whiteout(&layer.join("src"), "old.rs");
    write_opaque_marker(&layer.join("config"));
    write(&layer.join("config/new.toml"), "");

    let delta: LayerDeltaDescription = describe_layer_delta(&layer, 10).expect("describe delta");
    let _first_entry: Option<&LayerDeltaEntry> = delta.entries.first();
    let entries = delta
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.kind))
        .collect::<Vec<_>>();

    assert!(!delta.truncated);
    assert_eq!(
        entries,
        vec![
            ("config", LayerDeltaEntryKind::OpaqueDir),
            ("config/new.toml", LayerDeltaEntryKind::File),
            ("src", LayerDeltaEntryKind::Directory),
            ("src/lib.rs", LayerDeltaEntryKind::File),
            ("src/link.rs", LayerDeltaEntryKind::Symlink),
            ("src/main.rs", LayerDeltaEntryKind::File),
            ("src/old.rs", LayerDeltaEntryKind::Delete),
        ]
    );

    let truncated = describe_layer_delta(&layer, 2).expect("describe truncated delta");
    assert!(truncated.truncated);
    assert_eq!(truncated.entries.len(), 2);
}

#[test]
fn fold_newest_wins_and_never_selects_masked_content() {
    let fixture = DeltaFixture::new("newest-wins");
    let base = fixture.layer("B000001-base");
    write(&base.join("keep.txt"), "base\n");
    let l1 = fixture.layer("L000002-old");
    write(&l1.join("src/a.rs"), "v1\n");
    write(&l1.join("src/b.rs"), "B\n");
    let l2 = fixture.layer("L000003-new");
    write(&l2.join("src/a.rs"), "v2\n");
    write_logical_whiteout(&l2.join("src"), "b.rs");

    let manifest = fixture.manifest(&["L000003-new", "L000002-old", "B000001-base"]);
    let fold: DeltaFold = fold_delta_winners(fixture.root(), &manifest).expect("fold");

    assert_eq!(
        fold.delta_layers
            .iter()
            .map(|layer| layer.layer_id.as_str())
            .collect::<Vec<_>>(),
        vec!["L000003-new", "L000002-old"],
        "delta manifest excludes the base"
    );
    match fold.winners.get(&path("src/a.rs")) {
        Some(DeltaWinner::File { source }) => {
            assert!(
                source.starts_with(&l2),
                "newest layer wins; the masked older copy is never selected"
            );
        }
        other => panic!("expected file winner for src/a.rs, got {other:?}"),
    }
    assert_eq!(
        fold.winners.get(&path("src/b.rs")),
        Some(&DeltaWinner::Delete)
    );
    assert!(matches!(
        fold.winners.get(&path("src")),
        Some(DeltaWinner::Directory { .. })
    ));
    assert!(
        !fold.winners.contains_key(&path("keep.txt")),
        "base content never enters the fold"
    );
    assert_eq!(fold.winners.len(), 3);
}

#[test]
fn fold_whiteout_masks_older_subtree() {
    let fixture = DeltaFixture::new("whiteout-subtree");
    fixture.layer("B000001-base");
    let l1 = fixture.layer("L000002-old");
    write(&l1.join("cfg/dev.yml"), "D\n");
    write(&l1.join("cfg/nested/deep.yml"), "N\n");
    let l2 = fixture.layer("L000003-new");
    write_logical_whiteout(&l2, "cfg");

    let manifest = fixture.manifest(&["L000003-new", "L000002-old", "B000001-base"]);
    let fold = fold_delta_winners(fixture.root(), &manifest).expect("fold");

    assert_eq!(fold.winners.get(&path("cfg")), Some(&DeltaWinner::Delete));
    assert!(
        !fold.winners.contains_key(&path("cfg/dev.yml")),
        "whiteout masks the older subtree"
    );
    assert!(!fold.winners.contains_key(&path("cfg/nested")));
    assert!(!fold.winners.contains_key(&path("cfg/nested/deep.yml")));
    assert_eq!(fold.winners.len(), 1);
}

#[test]
fn fold_opaque_cut_masks_older_layers_and_keeps_same_layer_children() {
    let fixture = DeltaFixture::new("opaque-cut");
    fixture.layer("B000001-base");
    let l1 = fixture.layer("L000002-old");
    write(&l1.join("cfg/dev.yml"), "D\n");
    let l2 = fixture.layer("L000003-new");
    write(&l2.join("cfg/prod.yml"), "P2\n");
    write(&l2.join("cfg/.env"), "E\n");
    write_opaque_marker(&l2.join("cfg"));

    let manifest = fixture.manifest(&["L000003-new", "L000002-old", "B000001-base"]);
    let fold = fold_delta_winners(fixture.root(), &manifest).expect("fold");

    assert!(matches!(
        fold.winners.get(&path("cfg")),
        Some(DeltaWinner::OpaqueDir { .. })
    ));
    assert!(matches!(
        fold.winners.get(&path("cfg/prod.yml")),
        Some(DeltaWinner::File { .. })
    ));
    assert!(
        matches!(
            fold.winners.get(&path("cfg/.env")),
            Some(DeltaWinner::File { .. })
        ),
        "same-layer children survive their own opaque cut"
    );
    assert!(
        !fold.winners.contains_key(&path("cfg/dev.yml")),
        "older delta content under the cut never enters"
    );
    assert_eq!(fold.winners.len(), 3);
}

#[test]
fn fold_upgrades_a_newer_directory_winner_when_an_older_layer_is_opaque() {
    let fixture = DeltaFixture::new("opaque-upgrade");
    fixture.layer("B000001-base");
    let l0 = fixture.layer("L000002-oldest");
    write(&l0.join("cfg/stale.yml"), "S\n");
    let l1 = fixture.layer("L000003-mid");
    write(&l1.join("cfg/mid.yml"), "M\n");
    write_opaque_marker(&l1.join("cfg"));
    let l2 = fixture.layer("L000004-new");
    write(&l2.join("cfg/new.yml"), "N\n");

    let manifest = fixture.manifest(&[
        "L000004-new",
        "L000003-mid",
        "L000002-oldest",
        "B000001-base",
    ]);
    let fold = fold_delta_winners(fixture.root(), &manifest).expect("fold");

    match fold.winners.get(&path("cfg")) {
        Some(DeltaWinner::OpaqueDir { source }) => {
            assert!(
                source.starts_with(&l2),
                "the newest directory verdict keeps its source through the upgrade"
            );
        }
        other => panic!("expected opaque dir winner for cfg, got {other:?}"),
    }
    assert!(fold.winners.contains_key(&path("cfg/new.yml")));
    assert!(
        fold.winners.contains_key(&path("cfg/mid.yml")),
        "content above the opaque layer survives"
    );
    assert!(
        !fold.winners.contains_key(&path("cfg/stale.yml")),
        "content below the opaque layer is cut"
    );
}

#[test]
fn fold_nondirectory_ancestor_masks_older_subtree() {
    let fixture = DeltaFixture::new("nondir-ancestor");
    fixture.layer("B000001-base");
    let l0 = fixture.layer("L000002-oldest");
    write(&l0.join("a/old.txt"), "O\n");
    let l1 = fixture.layer("L000003-mid");
    write(&l1.join("a"), "now a file\n");
    let l2 = fixture.layer("L000004-new");
    write(&l2.join("a/new.txt"), "N\n");

    let manifest = fixture.manifest(&[
        "L000004-new",
        "L000003-mid",
        "L000002-oldest",
        "B000001-base",
    ]);
    let fold = fold_delta_winners(fixture.root(), &manifest).expect("fold");

    assert!(
        matches!(
            fold.winners.get(&path("a")),
            Some(DeltaWinner::Directory { .. })
        ),
        "the newest verdict for the path itself wins"
    );
    assert!(fold.winners.contains_key(&path("a/new.txt")));
    assert!(
        !fold.winners.contains_key(&path("a/old.txt")),
        "the mid layer's file at `a` cuts older content under it"
    );
}

#[test]
fn fold_zero_base_manifest_is_refused() {
    let fixture = DeltaFixture::new("zero-base");
    fixture.layer("L000002-only");
    let manifest = fixture.manifest(&["L000002-only"]);

    let error = fold_delta_winners(fixture.root(), &manifest).expect_err("zero-base must fail");
    assert!(
        error.to_string().contains("no base"),
        "unexpected error: {error}"
    );
    assert!(delta_layer_refs(&manifest).is_err());
}

#[test]
fn fold_matches_merged_view_over_the_delta_manifest() {
    let fixture = DeltaFixture::new("merged-view-equivalence");
    fixture.layer("B000001-base");
    let l1 = fixture.layer("L000002-old");
    write(&l1.join("src/a.rs"), "v1\n");
    write(&l1.join("src/b.rs"), "B\n");
    write(&l1.join("cfg/dev.yml"), "D\n");
    write(&l1.join("docs/guide.md"), "G\n");
    std::os::unix::fs::symlink("guide.md", l1.join("docs/link.md")).expect("symlink");
    let l2 = fixture.layer("L000003-new");
    write(&l2.join("src/a.rs"), "v2\n");
    write_logical_whiteout(&l2.join("src"), "b.rs");
    write(&l2.join("cfg/prod.yml"), "P\n");
    write_opaque_marker(&l2.join("cfg"));

    let manifest = fixture.manifest(&["L000003-new", "L000002-old", "B000001-base"]);
    let fold = fold_delta_winners(fixture.root(), &manifest).expect("fold");
    let delta_manifest = Manifest::new(
        manifest.version,
        delta_layer_refs(&manifest).expect("delta layers"),
        MANIFEST_SCHEMA_VERSION,
    )
    .expect("delta manifest");
    let view = MergedView::new(fixture.root().to_path_buf());

    let candidates = [
        "src",
        "src/a.rs",
        "src/b.rs",
        "cfg",
        "cfg/dev.yml",
        "cfg/prod.yml",
        "docs",
        "docs/guide.md",
        "docs/link.md",
        "absent.txt",
    ];
    for candidate in candidates {
        let merged = view
            .read_entry(candidate, &delta_manifest)
            .expect("merged read");
        match fold.winners.get(&path(candidate)) {
            Some(DeltaWinner::File { source }) => {
                let bytes = std::fs::read(source).expect("winner bytes");
                assert_eq!(
                    merged,
                    crate::stack::projection::MergedEntry::File { bytes },
                    "file winner diverges from MergedView at {candidate}"
                );
            }
            Some(DeltaWinner::Symlink { source }) => {
                let target = std::fs::read_link(source).expect("winner target");
                assert_eq!(
                    merged,
                    crate::stack::projection::MergedEntry::Symlink {
                        target: target.to_string_lossy().into_owned()
                    },
                    "symlink winner diverges from MergedView at {candidate}"
                );
            }
            Some(DeltaWinner::Directory { .. } | DeltaWinner::OpaqueDir { .. }) => {
                assert_eq!(
                    merged,
                    crate::stack::projection::MergedEntry::Directory,
                    "directory winner diverges from MergedView at {candidate}"
                );
            }
            Some(DeltaWinner::Delete) | None => {
                assert_eq!(
                    merged,
                    crate::stack::projection::MergedEntry::Absent,
                    "absent path diverges from MergedView at {candidate}"
                );
            }
        }
    }
}

#[test]
fn fold_agrees_with_flatten_on_winner_selection() {
    let fixture = DeltaFixture::new("flatten-crosscheck");
    let base = fixture.layer("B000001-base");
    write(&base.join("src/b.rs"), "B\n");
    write(&base.join("src/keep.rs"), "K\n");
    let l1 = fixture.layer("L000002-old");
    write(&l1.join("src/a.rs"), "v1\n");
    write(&l1.join("src/c.rs"), "C\n");
    let l2 = fixture.layer("L000003-new");
    write(&l2.join("src/a.rs"), "v2\n");
    write_logical_whiteout(&l2.join("src"), "b.rs");

    let manifest = fixture.manifest(&["L000003-new", "L000002-old", "B000001-base"]);
    let fold = fold_delta_winners(fixture.root(), &manifest).expect("fold");

    let staging = fixture.base.join("flattened");
    flatten_block_into_with_lower(
        &staging,
        &[l2.clone(), l1.clone()],
        std::slice::from_ref(&base),
    )
    .expect("flatten");

    for (winner_path, winner) in &fold.winners {
        let flattened = staging.join(winner_path.as_str());
        match winner {
            DeltaWinner::File { source } => {
                assert_eq!(
                    std::fs::read(&flattened).expect("flattened bytes"),
                    std::fs::read(source).expect("winner bytes"),
                    "flatten selected different content at {winner_path}"
                );
            }
            DeltaWinner::Directory { .. } | DeltaWinner::OpaqueDir { .. } => {
                assert!(flattened.is_dir(), "flatten lost directory {winner_path}");
            }
            DeltaWinner::Delete => {
                let whited = crate::whiteout::is_kernel_whiteout(&flattened)
                    || crate::whiteout::logical_whiteout_path_for_target(&flattened).exists();
                assert!(
                    whited,
                    "flatten dropped base-visible whiteout {winner_path}"
                );
            }
            DeltaWinner::Symlink { .. } => {
                assert!(
                    std::fs::symlink_metadata(&flattened)
                        .expect("flattened symlink")
                        .file_type()
                        .is_symlink(),
                    "flatten lost symlink {winner_path}"
                );
            }
        }
    }
}
