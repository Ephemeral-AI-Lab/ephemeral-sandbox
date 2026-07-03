use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::stack::publish::model::{
    ContentFingerprint, LayerProtectedDrop, LayerProtectedDropReason, PublishBase,
    PublishBaseRevision, PublishRejectReason, PublishValidatedChangesRequest,
};

use super::*;

struct PublishFixture {
    base: PathBuf,
    root: PathBuf,
    workspace: PathBuf,
}

impl PublishFixture {
    fn new(label: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let base = std::env::temp_dir().join(format!(
            "layerstack-publish-{label}-{}-{}",
            std::process::id(),
            NEXT_PUBLISH_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("layer-stack");
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace)?;
        Ok(Self {
            base,
            root,
            workspace,
        })
    }

    fn build_base(&self) -> Result<Manifest, Box<dyn std::error::Error + Send + Sync>> {
        build_workspace_base(&self.root, &self.workspace, false)?;
        let stack = LayerStack::open(self.root.clone())?;
        Ok(stack.read_active_manifest()?)
    }

    fn stack(&self) -> Result<LayerStack, LayerStackError> {
        LayerStack::open(self.root.clone())
    }
}

impl Drop for PublishFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

static NEXT_PUBLISH_TEST: AtomicU64 = AtomicU64::new(0);

fn lp(path: &str) -> LayerPath {
    LayerPath::parse(path).expect("test layer path is valid")
}

fn request(base: Manifest, changes: Vec<LayerChange>) -> PublishValidatedChangesRequest {
    PublishValidatedChangesRequest {
        base: PublishBase {
            revision: PublishBaseRevision {
                manifest_version: base.version,
                root_hash: manifest_root_hash(&base),
                layer_count: base.layers.len(),
            },
            manifest: base,
        },
        changes,
        protected_drops: Vec::new(),
    }
}

fn read_text(
    root: &std::path::Path,
    manifest: &Manifest,
    path: &str,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let view = MergedView::new(root.to_path_buf());
    let (bytes, exists) = view.read_bytes(path, manifest)?;
    if !exists {
        return Ok(None);
    }
    let bytes = bytes.expect("merged view returned bytes for existing path");
    Ok(Some(
        String::from_utf8(bytes).expect("test content is utf8"),
    ))
}

#[test]
fn source_occ_publish_succeeds_when_active_matches_base(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("source-success")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;

    let result = fixture.stack()?.publish_validated_changes(request(
        base,
        vec![LayerChange::Write {
            path: lp("README.md"),
            content: b"command\n".to_vec(),
        }],
    ))?;

    assert!(!result.no_op);
    assert_eq!(result.route_summary.source_count, 1);
    assert_eq!(
        read_text(&fixture.root, &result.manifest, "README.md")?,
        Some("command\n".to_owned())
    );
    Ok(())
}

#[test]
fn source_occ_conflict_rejects_without_publishing_ignored_changes(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("source-conflict")?;
    std::fs::write(fixture.workspace.join(".gitignore"), "ignored.log\n")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let mut stack = fixture.stack()?;
    let advanced = stack.publish_layer(&[LayerChange::Write {
        path: lp("README.md"),
        content: b"advanced\n".to_vec(),
    }])?;

    let error = stack
        .publish_validated_changes(request(
            base,
            vec![
                LayerChange::Write {
                    path: lp("README.md"),
                    content: b"command\n".to_vec(),
                },
                LayerChange::Write {
                    path: lp("ignored.log"),
                    content: b"ignored\n".to_vec(),
                },
            ],
        ))
        .expect_err("source conflict rejects publish");

    assert!(matches!(
        error,
        LayerStackError::PublishRejected(rejection)
            if rejection.reason == PublishRejectReason::SourceConflict
    ));
    let active = fixture.stack()?.read_active_manifest()?;
    assert_eq!(active, advanced);
    assert_eq!(read_text(&fixture.root, &active, "ignored.log")?, None);
    Ok(())
}

#[test]
fn ignored_only_publish_uses_command_base_gitignore(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("ignored-base")?;
    std::fs::write(fixture.workspace.join(".gitignore"), "out.log\n")?;
    let base = fixture.build_base()?;
    let mut stack = fixture.stack()?;
    stack.publish_layer(&[
        LayerChange::Write {
            path: lp(".gitignore"),
            content: Vec::new(),
        },
        LayerChange::Write {
            path: lp("out.log"),
            content: b"active\n".to_vec(),
        },
    ])?;

    let result = stack.publish_validated_changes(request(
        base,
        vec![LayerChange::Write {
            path: lp("out.log"),
            content: b"command\n".to_vec(),
        }],
    ))?;

    assert_eq!(result.route_summary.ignored_count, 1);
    assert_eq!(
        read_text(&fixture.root, &result.manifest, "out.log")?,
        Some("command\n".to_owned())
    );
    Ok(())
}

#[test]
fn nested_gitignore_anchored_patterns_do_not_double_strip(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("nested-gitignore")?;
    std::fs::create_dir_all(fixture.workspace.join("pkg/pkg"))?;
    std::fs::write(fixture.workspace.join("pkg/.gitignore"), "/pkg.log\n")?;
    let base = fixture.build_base()?;

    let result = fixture.stack()?.publish_validated_changes(request(
        base,
        vec![
            LayerChange::Write {
                path: lp("pkg/pkg.log"),
                content: b"ignored\n".to_vec(),
            },
            LayerChange::Write {
                path: lp("pkg/pkg/pkg.log"),
                content: b"source\n".to_vec(),
            },
        ],
    ))?;

    assert_eq!(result.route_summary.ignored_count, 1);
    assert_eq!(result.route_summary.source_count, 1);
    Ok(())
}

#[test]
fn protected_paths_reject() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("forbidden")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;

    for path in [
        "manifest.json",
        "workspace.json",
        "layers",
        "staging",
        ".layer-metadata",
        "pkg/.layer-metadata/file",
    ] {
        let error = fixture
            .stack()?
            .publish_validated_changes(request(
                base.clone(),
                vec![LayerChange::Write {
                    path: lp(path),
                    content: b"x".to_vec(),
                }],
            ))
            .expect_err("protected path rejects publish");
        assert!(
            matches!(error, LayerStackError::PublishRejected(ref rejection) if rejection.reason == PublishRejectReason::ProtectedPath),
            "unexpected error for {path}: {error:?}"
        );
    }
    Ok(())
}

#[test]
fn git_paths_route_as_source_and_publish(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("git-allowed")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;

    // `.git` is no longer special-cased: a git command's writes route as
    // ordinary source (first-writer-wins), not a forbidden mutation. Nested
    // `.git` (`pkg/.git/...`) is likewise ordinary source.
    let result = fixture.stack()?.publish_validated_changes(request(
        base,
        vec![
            LayerChange::Write {
                path: lp(".git/config"),
                content: b"[core]\n".to_vec(),
            },
            LayerChange::Write {
                path: lp("pkg/.git/HEAD"),
                content: b"ref: refs/heads/main\n".to_vec(),
            },
        ],
    ))?;

    assert!(!result.no_op);
    assert_eq!(result.route_summary.source_count, 2);
    assert_eq!(
        read_text(&fixture.root, &result.manifest, ".git/config")?,
        Some("[core]\n".to_owned())
    );
    Ok(())
}

#[test]
fn concurrent_git_binary_divergence_rejects_as_source_conflict(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("git-binary-conflict")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let mut stack = fixture.stack()?;
    // A concurrent publisher advances `.git/index` with binary content (NUL).
    stack.publish_layer(&[LayerChange::Write {
        path: lp(".git/index"),
        content: vec![b'D', b'I', b'R', b'C', 0, 1],
    }])?;

    // Our publish, from the now-stale base, diverges on the same binary path.
    // Line merge is ineligible for binary, so OCC rejects cleanly rather than
    // committing a corrupt merge.
    let error = stack
        .publish_validated_changes(request(
            base,
            vec![LayerChange::Write {
                path: lp(".git/index"),
                content: vec![b'D', b'I', b'R', b'C', 0, 2],
            }],
        ))
        .expect_err("binary git divergence rejects publish");

    assert!(matches!(
        error,
        LayerStackError::PublishRejected(rejection)
            if rejection.reason == PublishRejectReason::SourceConflict
    ));
    Ok(())
}

#[test]
fn invalid_gitignore_does_not_panic_and_contributes_no_rules(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("invalid-gitignore")?;
    std::fs::write(fixture.workspace.join(".gitignore"), "[\n")?;
    let base = fixture.build_base()?;

    let result = fixture.stack()?.publish_validated_changes(request(
        base,
        vec![LayerChange::Write {
            path: lp("file.log"),
            content: b"source\n".to_vec(),
        }],
    ))?;

    assert_eq!(result.route_summary.source_count, 1);
    Ok(())
}

#[test]
fn protected_drop_rejects_before_publish() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("protected-drop")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let mut request = request(
        base.clone(),
        vec![LayerChange::Write {
            path: lp("README.md"),
            content: b"command\n".to_vec(),
        }],
    );
    request.protected_drops.push(LayerProtectedDrop {
        path: ".command-scratch/cmd-1".to_owned(),
        reason: LayerProtectedDropReason::CommandScratchPath,
    });

    let error = fixture
        .stack()?
        .publish_validated_changes(request)
        .expect_err("protected drop rejects publish");

    assert!(matches!(
        error,
        LayerStackError::PublishRejected(rejection)
            if rejection.reason == PublishRejectReason::ProtectedPath
                && rejection.protected_drop.as_ref().map(|drop| drop.reason)
                    == Some(LayerProtectedDropReason::CommandScratchPath)
    ));
    assert_eq!(fixture.stack()?.read_active_manifest()?, base);
    Ok(())
}

#[test]
fn unsupported_special_file_drop_does_not_block_regular_publish(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("unsupported-special-drop")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let mut request = request(
        base,
        vec![LayerChange::Write {
            path: lp("note.txt"),
            content: b"regular\n".to_vec(),
        }],
    );
    request.protected_drops.push(LayerProtectedDrop {
        path: "run.fifo".to_owned(),
        reason: LayerProtectedDropReason::UnsupportedSpecialFile,
    });

    let result = fixture.stack()?.publish_validated_changes(request)?;

    assert_eq!(
        read_text(&fixture.root, &result.manifest, "note.txt")?,
        Some("regular\n".to_owned())
    );
    Ok(())
}

#[test]
fn symlink_fingerprints_report_source_conflicts(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("symlink-conflict")?;
    std::os::unix::fs::symlink("base-target", fixture.workspace.join("link"))?;
    let base = fixture.build_base()?;
    let mut stack = fixture.stack()?;
    stack.publish_layer(&[LayerChange::Symlink {
        path: lp("link"),
        source_path: "active-target".to_owned(),
    }])?;

    let error = stack
        .publish_validated_changes(request(
            base,
            vec![LayerChange::Symlink {
                path: lp("link"),
                source_path: "command-target".to_owned(),
            }],
        ))
        .expect_err("symlink target mismatch conflicts");

    match error {
        LayerStackError::PublishRejected(rejection)
            if rejection.reason == PublishRejectReason::SourceConflict =>
        {
            let conflict = rejection
                .source_conflict
                .expect("source conflict is included");
            assert!(matches!(
                conflict.expected,
                ContentFingerprint::Symlink { ref target } if target == "base-target"
            ));
            assert!(matches!(
                conflict.actual,
                ContentFingerprint::Symlink { ref target } if target == "active-target"
            ));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    Ok(())
}

#[test]
fn opaque_dir_over_mixed_source_and_ignored_descendants_rejects(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("opaque-mixed")?;
    std::fs::write(fixture.workspace.join(".gitignore"), "target/ignored.log\n")?;
    std::fs::create_dir_all(fixture.workspace.join("target"))?;
    std::fs::write(fixture.workspace.join("target/source.txt"), "source\n")?;
    std::fs::write(fixture.workspace.join("target/ignored.log"), "ignored\n")?;
    let base = fixture.build_base()?;

    let error = fixture
        .stack()?
        .publish_validated_changes(request(
            base,
            vec![LayerChange::OpaqueDir { path: lp("target") }],
        ))
        .expect_err("opaque dir mixed routes reject");

    assert!(matches!(
        error,
        LayerStackError::PublishRejected(rejection)
            if rejection.reason == PublishRejectReason::OpaqueDirMixedRoutes
    ));
    Ok(())
}

#[test]
fn source_delete_conflicts_when_active_changed(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("source-delete-conflict")?;
    std::fs::write(fixture.workspace.join("stale.txt"), "base\n")?;
    let base = fixture.build_base()?;
    let mut stack = fixture.stack()?;
    let advanced = stack.publish_layer(&[LayerChange::Write {
        path: lp("stale.txt"),
        content: b"active\n".to_vec(),
    }])?;

    let error = stack
        .publish_validated_changes(request(
            base,
            vec![LayerChange::Delete {
                path: lp("stale.txt"),
            }],
        ))
        .expect_err("delete of changed source path rejects");

    match error {
        LayerStackError::PublishRejected(rejection)
            if rejection.reason == PublishRejectReason::SourceConflict =>
        {
            let conflict = rejection
                .source_conflict
                .expect("source conflict is included");
            assert!(matches!(
                conflict.expected,
                ContentFingerprint::File { ref digest, .. } if !digest.is_empty()
            ));
            assert!(matches!(
                conflict.actual,
                ContentFingerprint::File { ref digest, .. } if !digest.is_empty()
            ));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(fixture.stack()?.read_active_manifest()?, advanced);
    Ok(())
}

#[test]
fn directory_delete_uses_directory_gitignore_rules(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("directory-delete-gitignore")?;
    std::fs::write(fixture.workspace.join(".gitignore"), "target/\n")?;
    std::fs::create_dir_all(fixture.workspace.join("target"))?;
    std::fs::write(fixture.workspace.join("target/file.txt"), "base\n")?;
    let base = fixture.build_base()?;

    let result = fixture.stack()?.publish_validated_changes(request(
        base,
        vec![LayerChange::Delete { path: lp("target") }],
    ))?;

    assert_eq!(result.route_summary.ignored_count, 1);
    assert_eq!(result.route_summary.source_count, 0);
    assert_eq!(
        read_text(&fixture.root, &result.manifest, "target/file.txt")?,
        None
    );
    Ok(())
}

#[test]
fn gitignore_patterns_keep_directory_and_negation_semantics(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("gitignore-semantics")?;
    std::fs::write(
        fixture.workspace.join(".gitignore"),
        "node_modules/\nlogs/*.log\n**/build/\n*.tmp\n!important.tmp\nsealed/\n!sealed/keep.tmp\n",
    )?;
    let base = fixture.build_base()?;

    let result = fixture.stack()?.publish_validated_changes(request(
        base,
        vec![
            LayerChange::Write {
                path: lp("pkg/node_modules/a.js"),
                content: b"ignored\n".to_vec(),
            },
            LayerChange::Write {
                path: lp("logs/root.log"),
                content: b"ignored\n".to_vec(),
            },
            LayerChange::Write {
                path: lp("logs/sub/x.log"),
                content: b"source\n".to_vec(),
            },
            LayerChange::Write {
                path: lp("src/build/out.js"),
                content: b"ignored\n".to_vec(),
            },
            LayerChange::Write {
                path: lp("important.tmp"),
                content: b"source\n".to_vec(),
            },
            LayerChange::Write {
                path: lp("sealed/keep.tmp"),
                content: b"ignored\n".to_vec(),
            },
        ],
    ))?;

    assert_eq!(result.route_summary.ignored_count, 4);
    assert_eq!(result.route_summary.source_count, 2);
    Ok(())
}

#[test]
fn opaque_dir_all_source_validates_hidden_descendant_conflicts(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("opaque-source-conflict")?;
    std::fs::create_dir_all(fixture.workspace.join("target"))?;
    std::fs::write(fixture.workspace.join("target/source.txt"), "base\n")?;
    let base = fixture.build_base()?;
    let mut stack = fixture.stack()?;
    stack.publish_layer(&[LayerChange::Write {
        path: lp("target/source.txt"),
        content: b"active\n".to_vec(),
    }])?;

    let error = stack
        .publish_validated_changes(request(
            base,
            vec![LayerChange::OpaqueDir { path: lp("target") }],
        ))
        .expect_err("opaque source descendant conflict rejects");

    assert!(matches!(
        error,
        LayerStackError::PublishRejected(rejection)
            if rejection.reason == PublishRejectReason::SourceConflict
    ));
    Ok(())
}

#[test]
fn opaque_dir_all_ignored_descendants_routes_direct(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("opaque-ignored")?;
    std::fs::write(fixture.workspace.join(".gitignore"), "target/\n")?;
    std::fs::create_dir_all(fixture.workspace.join("target"))?;
    std::fs::write(fixture.workspace.join("target/ignored.log"), "base\n")?;
    let base = fixture.build_base()?;

    let result = fixture.stack()?.publish_validated_changes(request(
        base,
        vec![LayerChange::OpaqueDir { path: lp("target") }],
    ))?;

    assert_eq!(result.route_summary.ignored_count, 1);
    assert_eq!(result.route_summary.source_count, 0);
    assert_eq!(
        read_text(&fixture.root, &result.manifest, "target/ignored.log")?,
        None
    );
    Ok(())
}

#[test]
fn opaque_dir_over_protected_descendant_rejects(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("opaque-protected")?;
    std::fs::create_dir_all(fixture.workspace.join("target/.layer-metadata"))?;
    std::fs::write(
        fixture.workspace.join("target/.layer-metadata/file"),
        "protected\n",
    )?;
    let base = fixture.build_base()?;

    let error = fixture
        .stack()?
        .publish_validated_changes(request(
            base,
            vec![LayerChange::OpaqueDir { path: lp("target") }],
        ))
        .expect_err("opaque over protected descendant rejects");

    assert!(matches!(
        error,
        LayerStackError::PublishRejected(rejection)
            if rejection.reason == PublishRejectReason::OpaqueDirProtectedDescendant
    ));
    Ok(())
}

#[test]
fn opaque_dir_expansion_limit_bounds_traversal(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("opaque-limit")?;
    let target = fixture.workspace.join("target");
    std::fs::create_dir_all(&target)?;
    for index in 0..=4096 {
        std::fs::write(target.join(format!("{index:04}.txt")), "x\n")?;
    }
    let base = fixture.build_base()?;

    let error = fixture
        .stack()?
        .publish_validated_changes(request(
            base,
            vec![LayerChange::OpaqueDir { path: lp("target") }],
        ))
        .expect_err("opaque expansion limit rejects");

    assert!(matches!(
        error,
        LayerStackError::PublishRejected(rejection)
            if rejection.reason == PublishRejectReason::OpaqueDirExpansionLimit
    ));
    Ok(())
}

#[test]
fn mixed_source_and_ignored_changes_publish_in_one_layer(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("mixed-routes-success")?;
    std::fs::write(fixture.workspace.join(".gitignore"), "ignored.log\n")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let base_layer_count = base.layers.len();

    let result = fixture.stack()?.publish_validated_changes(request(
        base,
        vec![
            LayerChange::Write {
                path: lp("README.md"),
                content: b"command\n".to_vec(),
            },
            LayerChange::Write {
                path: lp("ignored.log"),
                content: b"ignored\n".to_vec(),
            },
        ],
    ))?;

    assert!(!result.no_op);
    assert_eq!(result.route_summary.source_count, 1);
    assert_eq!(result.route_summary.ignored_count, 1);
    assert_eq!(result.manifest.layers.len(), base_layer_count + 1);
    assert_eq!(
        read_text(&fixture.root, &result.manifest, "README.md")?,
        Some("command\n".to_owned())
    );
    assert_eq!(
        read_text(&fixture.root, &result.manifest, "ignored.log")?,
        Some("ignored\n".to_owned())
    );
    Ok(())
}

#[test]
fn digest_deduped_publish_reports_no_op() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = PublishFixture::new("dedupe-no-op")?;
    std::fs::write(fixture.workspace.join("README.md"), "base\n")?;
    let base = fixture.build_base()?;
    let mut stack = fixture.stack()?;
    let published = stack.publish_validated_changes(request(
        base,
        vec![LayerChange::Write {
            path: lp("README.md"),
            content: b"same\n".to_vec(),
        }],
    ))?;

    let deduped = stack.publish_validated_changes(request(
        published.manifest.clone(),
        vec![LayerChange::Write {
            path: lp("README.md"),
            content: b"same\n".to_vec(),
        }],
    ))?;

    assert!(deduped.no_op);
    assert_eq!(deduped.manifest, published.manifest);
    Ok(())
}
