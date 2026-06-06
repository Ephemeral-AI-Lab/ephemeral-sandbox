use std::future;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use eos_protocol::audit::Lane;
use serde_json::json;

use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn upperdir_tree_resource_timings_capture_bounded_payload() -> TestResult {
    let fixture = Fixture::new("upperdir_tree_stats")?;
    let upperdir = fixture.base.join("upperdir");
    std::fs::create_dir_all(upperdir.join("nested"))?;
    std::fs::write(upperdir.join("nested/payload.bin"), vec![7_u8; 4096])?;

    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let mut timings = resource_timings(&manifest, 1);
    let upperdir_stats = eos_ephemeral_workspace::TreeResourceStats::collect(&upperdir);
    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.upperdir",
        &TreeResourceStats::from_ephemeral(&upperdir_stats),
    );

    assert_eq!(
        timing_f64_value(&timings, "resource.command_exec.workspace_tree_bytes"),
        0.0
    );
    assert_eq!(
        timing_f64_value(&timings, "resource.command_exec.upperdir_tree_exists"),
        1.0
    );
    assert!(timing_f64_value(&timings, "resource.command_exec.upperdir_tree_bytes") >= 4096.0);
    assert_eq!(
        timing_f64_value(&timings, "resource.command_exec.upperdir_tree_truncated"),
        0.0
    );
    Ok(())
}

#[test]
fn op_table_rejects_different_handler_collision() {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "test handlers must match the dispatcher handler ABI"
    )]
    fn first_handler(_args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
        Ok(json!({"handler": "first"}))
    }
    #[expect(
        clippy::unnecessary_wraps,
        reason = "test handlers must match the dispatcher handler ABI"
    )]
    fn second_handler(_args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
        Ok(json!({"handler": "second"}))
    }

    let mut table = OpTable::default();
    assert!(table.register("api.test.collision", first_handler));
    assert!(table.register("api.test.collision", first_handler));
    assert!(!table.register("api.test.collision", second_handler));

    let response = table.dispatch(&Request {
        op: "api.test.collision".to_owned(),
        invocation_id: "collision-test".to_owned(),
        args: json!({}),
    });
    assert_eq!(response["handler"], "first");
}

#[test]
fn builtin_table_routes_commit_to_workspace() {
    let response = OpTable::with_builtins().dispatch(&Request {
        op: "api.commit_to_workspace".to_owned(),
        invocation_id: "commit-to-workspace-route-test".to_owned(),
        args: json!({}),
    });

    assert_ne!(response["error"]["kind"], json!("unknown_op"));
    assert_eq!(response["error"]["kind"], json!("invalid_envelope"));
    assert!(response["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("layer_stack_root is required"));
}

#[test]
fn builtin_table_routes_commit_to_git() {
    let response = OpTable::with_builtins().dispatch(&Request {
        op: "api.commit_to_git".to_owned(),
        invocation_id: "commit-to-git-route-test".to_owned(),
        args: json!({}),
    });

    assert_ne!(response["error"]["kind"], json!("unknown_op"));
    assert_eq!(response["error"]["kind"], json!("invalid_envelope"));
    assert!(response["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("layer_stack_root is required"));
}

#[test]
fn dispatch_attaches_real_runtime_timings() {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "test handlers must match the dispatcher handler ABI"
    )]
    fn slow_handler(_args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
        std::thread::sleep(std::time::Duration::from_millis(2));
        Ok(json!({"success": true}))
    }

    let mut table = OpTable::default();
    assert!(table.register("api.test.slow", slow_handler));

    let response = table.dispatch_with_context(
        &Request {
            op: "api.test.slow".to_owned(),
            invocation_id: "timings-test".to_owned(),
            args: json!({}),
        },
        DispatchContext {
            invocation_registry: None,
            audit_config: None,
            read_request_s: Some(0.125),
        },
    );

    assert_eq!(response["success"], json!(true));
    assert!(
        response["timings"]["runtime.boot_to_dispatch_s"]
            .as_f64()
            .unwrap_or_default()
            >= 0.0
    );
    assert!(
        response["timings"]["runtime.dispatch_s"]
            .as_f64()
            .unwrap_or_default()
            > 0.0
    );
    assert_eq!(response["timings"]["runtime.read_request_s"], json!(0.125));
}

#[tokio::test]
async fn cancel_waits_for_bounded_cleanup() -> TestResult {
    let registry = Arc::new(InFlightRegistry::new(300.0, 30.0));
    let task = tokio::spawn(future::pending::<()>());
    registry.register(
        "cancel-target",
        task.abort_handle(),
        "caller-a",
        "api.v1.exec_command",
        true,
    );
    let cleanup_registry = Arc::clone(&registry);
    let cleanup_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        cleanup_registry.deregister("cancel-target");
    });

    let response = OpTable::with_builtins().dispatch_with_context(
        &Request {
            op: "api.v1.cancel".to_owned(),
            invocation_id: "cancel-request".to_owned(),
            args: json!({"invocation_id": "cancel-target"}),
        },
        DispatchContext::with_invocation_registry(&registry),
    );

    cleanup_thread
        .join()
        .map_err(|_| "cleanup helper panicked")?;
    assert_eq!(response["cancelled"], json!(true));
    assert_eq!(response["already_done"], json!(false));
    assert_eq!(response["cleanup_done"], json!(true));
    match task.await {
        Ok(()) => Err("expected cancelled task".into()),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(format!("expected cancellation, got {error}").into()),
    }
}

#[test]
fn internal_error_envelope_adds_error_id() {
    let response = error_envelope(
        ErrorKind::InternalError,
        "daemon invocation failed",
        json!({"op": "api.test.failure"}),
    );

    assert_eq!(response["error"]["kind"], json!("internal_error"));
    assert_eq!(
        response["error"]["details"]["op"],
        json!("api.test.failure")
    );
    let Some(error_id) = response["error"]["details"]["error_id"].as_str() else {
        panic!("internal errors carry details.error_id");
    };
    assert_eq!(error_id.len(), 32);
    assert!(error_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(error_id.as_bytes()[12], b'4');
    assert!(matches!(error_id.as_bytes()[16], b'8' | b'9' | b'a' | b'b'));
}

#[test]
fn base_hashes_accept_opaque_dir_over_existing_directory() -> TestResult {
    let fixture = Fixture::new("opaque_base_hash")?;
    std::fs::create_dir_all(fixture.root.join("layers/B000001-base/opaque_dir"))?;
    std::fs::write(
        fixture.root.join("layers/B000001-base/opaque_dir/old.txt"),
        "old\n",
    )?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;

    let hashes = base_hashes_for_snapshot(
        &fixture.root,
        &manifest,
        &[LayerChange::OpaqueDir {
            path: lp("opaque_dir")?,
        }],
    )?;

    assert_eq!(hashes, vec![(lp("opaque_dir")?, None)]);
    Ok(())
}

#[test]
fn command_collect_completed_is_background_only_not_overlay_lifecycle() {
    let request = Request {
        op: "api.v1.command.collect_completed".to_owned(),
        invocation_id: "collect-completed".to_owned(),
        args: json!({"command_session_id": "cmd-1", "caller_id": "caller-1"}),
    };

    assert_eq!(
        background_event_kind(&request, &json!({"success": true})),
        Some(("background_tool.completed", "command_session"))
    );
    assert!(!uses_overlay_or_lease(
        &request.op,
        &json!({"success": true})
    ));
}

#[test]
fn gated_stale_base_aborts_without_publish() -> TestResult {
    let fixture = Fixture::new("gated_stale")?;
    let old_hash = hash_bytes(b"# README\n");
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp("README.md")?,
        content: b"# theirs\n".to_vec(),
    }])?;

    let result = transaction(&fixture)
        .revalidate_and_publish(&PreparedChangeset {
            snapshot_version: Some(1),
            path_groups: vec![publish_decision("README.md", Route::Gated, Some(old_hash))?],
            changes: vec![LayerChange::Write {
                path: lp("README.md")?,
                content: b"# mine\n".to_vec(),
            }],
            atomic: true,
        })
        .map_err(|conflict| format!("unexpected publish conflict: {conflict:?}"))?;

    assert_eq!(result.published_manifest_version, None);
    assert_eq!(result.files[0].status, OccStatus::AbortedVersion);
    assert_eq!(read_text(&fixture, "README.md")?, "# theirs\n");
    Ok(())
}

#[test]
fn direct_route_ignores_stale_base_and_publishes() -> TestResult {
    let fixture = Fixture::new("direct_stale")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp("target/out.txt")?,
        content: b"theirs\n".to_vec(),
    }])?;

    let result = transaction(&fixture)
        .revalidate_and_publish(&PreparedChangeset {
            snapshot_version: Some(1),
            path_groups: vec![publish_decision(
                "target/out.txt",
                Route::Direct,
                Some("stale".to_owned()),
            )?],
            changes: vec![LayerChange::Write {
                path: lp("target/out.txt")?,
                content: b"mine\n".to_vec(),
            }],
            atomic: true,
        })
        .map_err(|conflict| format!("unexpected publish conflict: {conflict:?}"))?;

    assert!(result.success());
    assert_eq!(result.files[0].status, OccStatus::Committed);
    assert_eq!(read_text(&fixture, "target/out.txt")?, "mine\n");
    Ok(())
}

#[test]
fn gated_symlink_change_validates_and_publishes() -> TestResult {
    let fixture = Fixture::new("gated_symlink")?;
    let result = transaction(&fixture)
        .revalidate_and_publish(&PreparedChangeset {
            snapshot_version: Some(1),
            path_groups: vec![publish_decision("link.txt", Route::Gated, None)?],
            changes: vec![LayerChange::Symlink {
                path: lp("link.txt")?,
                source_path: "target.txt".to_owned(),
            }],
            atomic: true,
        })
        .map_err(|conflict| format!("unexpected publish conflict: {conflict:?}"))?;

    assert!(result.success());
    assert_eq!(result.files[0].status, OccStatus::Committed);
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let projected = fixture.base.join("projected");
    eos_layerstack::MergedView::new(fixture.root.clone()).project(&projected, &manifest)?;
    assert_eq!(
        std::fs::read_link(projected.join("link.txt"))?,
        PathBuf::from("target.txt")
    );
    Ok(())
}

#[test]
fn atomic_mixed_validation_failure_drops_accepted_paths() -> TestResult {
    let fixture = Fixture::new("atomic_mixed")?;
    let old_hash = hash_bytes(b"# README\n");
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: lp("README.md")?,
        content: b"# theirs\n".to_vec(),
    }])?;

    let result = transaction(&fixture)
        .revalidate_and_publish(&PreparedChangeset {
            snapshot_version: Some(1),
            path_groups: vec![
                publish_decision("README.md", Route::Gated, Some(old_hash))?,
                publish_decision("target/out.txt", Route::Direct, None)?,
            ],
            changes: vec![
                LayerChange::Write {
                    path: lp("README.md")?,
                    content: b"# mine\n".to_vec(),
                },
                LayerChange::Write {
                    path: lp("target/out.txt")?,
                    content: b"ok\n".to_vec(),
                },
            ],
            atomic: true,
        })
        .map_err(|conflict| format!("unexpected publish conflict: {conflict:?}"))?;

    assert_eq!(result.published_manifest_version, None);
    assert_eq!(result.files[0].status, OccStatus::AbortedVersion);
    assert_eq!(result.files[1].status, OccStatus::Dropped);
    assert_eq!(read_text(&fixture, "README.md")?, "# theirs\n");
    assert!(
        !LayerStack::open(fixture.root.clone())?
            .read_bytes("target/out.txt")?
            .1
    );
    Ok(())
}

#[test]
fn root_gitignore_routes_target_as_direct() -> TestResult {
    let fixture = Fixture::new_with_gitignore("gitignore_direct", "target/\n*.pyc\n")?;
    let provider = LayerStackRouteProvider {
        root: fixture.root.clone(),
    };

    assert!(provider.is_ignored(&lp("target/out.txt")?)?);
    assert!(provider.is_ignored(&lp("pkg/cache.pyc")?)?);
    assert!(!provider.is_ignored(&lp("src/main.rs")?)?);
    Ok(())
}

#[test]
fn occ_route_metrics_count_gated_and_direct_paths() -> TestResult {
    let fixture = Fixture::new_with_gitignore("route_metrics", "target/\n*.pyc\n")?;
    let metrics = occ_route_metrics(
        &fixture.root,
        &[
            LayerChange::Write {
                path: lp("src/main.rs")?,
                content: b"tracked".to_vec(),
            },
            LayerChange::Write {
                path: lp("target/out.txt")?,
                content: b"direct".to_vec(),
            },
            LayerChange::Write {
                path: lp("pkg/cache.pyc")?,
                content: b"direct".to_vec(),
            },
            LayerChange::Write {
                path: lp(".git/config")?,
                content: b"drop".to_vec(),
            },
        ],
    )?;

    assert_eq!(metrics.gated_path_count, 1);
    assert_eq!(metrics.direct_path_count, 2);
    Ok(())
}

fn route_provider(fixture: &Fixture) -> LayerStackRouteProvider {
    LayerStackRouteProvider {
        root: fixture.root.clone(),
    }
}

// N2 (HIGH): a no-slash dir-only pattern is anchored at *any* depth, so a
// file under `frontend/node_modules/` routes DIRECT — the most common
// misroute the old root-anchored prefix check produced.
#[test]
fn dir_only_pattern_matches_at_any_depth() -> TestResult {
    let fixture = Fixture::new_with_gitignore("n2_dir_only", "node_modules/\n")?;
    let provider = route_provider(&fixture);
    assert!(provider.is_ignored(&lp("frontend/node_modules/index.js")?)?);
    assert!(provider.is_ignored(&lp("node_modules/index.js")?)?);
    assert!(!provider.is_ignored(&lp("frontend/src/index.js")?)?);
    Ok(())
}

// N3 (HIGH, data-loss): `*` must not cross `/`. `logs/*.log` does NOT match
// `logs/sub/x.log`, so it routes GATED (base-hash validated) — not
// DIRECT-then-silently-clobber as the old `wildcard_match` allowed.
#[test]
fn star_does_not_cross_slash() -> TestResult {
    let fixture = Fixture::new_with_gitignore("n3_star_slash", "logs/*.log\n")?;
    let provider = route_provider(&fixture);
    assert!(provider.is_ignored(&lp("logs/app.log")?)?);
    assert!(!provider.is_ignored(&lp("logs/sub/x.log")?)?);
    Ok(())
}

// Nested `.gitignore` is scoped to its own subtree.
#[test]
fn nested_gitignore_is_scoped_to_its_subtree() -> TestResult {
    let fixture = Fixture::new_with_gitignores("nested", &[("frontend", "dist/\n")])?;
    let provider = route_provider(&fixture);
    assert!(provider.is_ignored(&lp("frontend/dist/bundle.js")?)?);
    assert!(!provider.is_ignored(&lp("dist/bundle.js")?)?);
    Ok(())
}

// `**` matches across path segments.
#[test]
fn double_star_matches_across_segments() -> TestResult {
    let fixture = Fixture::new_with_gitignore("double_star", "**/build/\n")?;
    let provider = route_provider(&fixture);
    assert!(provider.is_ignored(&lp("a/b/build/out.o")?)?);
    assert!(provider.is_ignored(&lp("build/out.o")?)?);
    assert!(!provider.is_ignored(&lp("a/b/builder.rs")?)?);
    Ok(())
}

// `!` re-includes within a non-sealed directory.
#[test]
fn bang_re_includes_in_unsealed_dir() -> TestResult {
    let fixture = Fixture::new_with_gitignore("bang", "*.log\n!keep.log\n")?;
    let provider = route_provider(&fixture);
    assert!(provider.is_ignored(&lp("other.log")?)?);
    assert!(!provider.is_ignored(&lp("keep.log")?)?);
    Ok(())
}

// Directory seal: an excluded ancestor dir seals its subtree — a deeper `!`
// cannot rescue contents under it (Git semantics).
#[test]
fn excluded_dir_seals_against_deeper_reinclude() -> TestResult {
    let fixture =
        Fixture::new_with_gitignores("seal", &[("", "build/\n"), ("build", "!keep.txt\n")])?;
    let provider = route_provider(&fixture);
    assert!(provider.is_ignored(&lp("build/keep.txt")?)?);
    Ok(())
}

// Telemetry shares the one routine, so counts equal the route decision for
// the same inputs (including the N2/N3/nested/seal cases above).
#[test]
fn occ_route_metrics_match_route_decision() -> TestResult {
    let fixture = Fixture::new_with_gitignores(
        "metrics_parity",
        &[
            ("", "node_modules/\nlogs/*.log\nbuild/\n"),
            ("build", "!keep.txt\n"),
        ],
    )?;
    let provider = route_provider(&fixture);
    let paths = [
        "frontend/node_modules/index.js", // DIRECT (N2 dir-only any depth)
        "logs/sub/x.log",                 // GATED  (N3 star not crossing /)
        "logs/app.log",                   // DIRECT
        "build/keep.txt",                 // DIRECT (seal beats deeper !)
        "src/main.rs",                    // GATED
        ".git/config",                    // skipped by metrics
    ];
    let mut expected_direct = 0;
    let mut expected_gated = 0;
    for path in paths {
        if path == ".git/config" {
            continue;
        }
        if provider.is_ignored(&lp(path)?)? {
            expected_direct += 1;
        } else {
            expected_gated += 1;
        }
    }
    let changes: Vec<LayerChange> = paths
        .iter()
        .map(|path| {
            Ok(LayerChange::Write {
                path: lp(path)?,
                content: b"x".to_vec(),
            })
        })
        .collect::<TestResult<_>>()?;
    let metrics = occ_route_metrics(&fixture.root, &changes)?;
    assert_eq!(metrics.direct_path_count, expected_direct);
    assert_eq!(metrics.gated_path_count, expected_gated);
    assert_eq!(expected_direct, 3);
    assert_eq!(expected_gated, 2);
    Ok(())
}

// Overlay/layerstack composition: a `.gitignore` published into an *upper*
// layer (the base layer carries none) is resolved through the active merged
// manifest — the same newest-layer-wins, whiteout-aware view the overlay
// mount projects. Proves the oracle reads `.gitignore` via `read_bytes`/
// `MergedView` across layers, not just from a single seeded layer.
#[test]
fn gitignore_resolves_through_published_upper_layer() -> TestResult {
    let fixture = Fixture::new("cross_layer")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[
        LayerChange::Write {
            path: lp(".gitignore")?,
            content: b"node_modules/\n".to_vec(),
        },
        LayerChange::Write {
            path: lp("frontend/.gitignore")?,
            content: b"dist/\n".to_vec(),
        },
    ])?;
    let provider = route_provider(&fixture);
    // Root rule from the upper layer, matched at depth via the seal.
    assert!(provider.is_ignored(&lp("frontend/node_modules/index.js")?)?);
    // Nested rule, also published into the upper layer.
    assert!(provider.is_ignored(&lp("frontend/dist/bundle.js")?)?);
    assert!(!provider.is_ignored(&lp("src/main.rs")?)?);
    Ok(())
}

// Regression (double-strip on prefix replay, data-loss-class): a per-level
// matcher for dir `D` must not strip `D` from a path whose next component
// repeats `D`'s name. The caller already makes the path relative to `D`, so
// the matcher must be rooted at `.` — `GitignoreBuilder::new(D)` would strip
// `D` a SECOND time (raw byte prefix), turning `a/x` into `x` and matching an
// anchored `/x`. Ground truth below is `git check-ignore --no-index`.
#[test]
fn nested_anchored_pattern_not_double_stripped_on_prefix_replay() -> TestResult {
    let fixture = Fixture::new_with_gitignores(
        "prefix_replay",
        &[("a", "/x\n/b\n"), ("build", "/build/x\n")],
    )?;
    let provider = route_provider(&fixture);
    // `/x` anchored at `a/` matches `a/x` (DIRECT) but NOT `a/a/x` — routing
    // the tracked `a/a/x` DIRECT would bypass the gate and silently clobber.
    assert!(provider.is_ignored(&lp("a/x")?)?);
    assert!(!provider.is_ignored(&lp("a/a/x")?)?);
    // Seal variant: `/b` seals `a/b`'s subtree, but `a/a/b` is not the
    // anchored `a/b`, so its whole subtree must stay GATED.
    assert!(provider.is_ignored(&lp("a/b/file.txt")?)?);
    assert!(!provider.is_ignored(&lp("a/a/b/file.txt")?)?);
    // Opposite (false-GATED) direction: `/build/x` anchored at `build/` DOES
    // match `build/build/x`; the old double-strip dropped it to `x` and missed.
    assert!(provider.is_ignored(&lp("build/build/x")?)?);
    assert!(!provider.is_ignored(&lp("build/x")?)?);
    Ok(())
}

#[test]
fn audit_pull_reads_shared_daemon_ring() -> TestResult {
    let marker = format!("phase3t-audit-test-{}", unique_suffix());
    let after_seq = audit_after_seq()?;
    crate::audit::buffer::safe_emit(
        json!({"type": marker, "payload": {"source": "unit-test"}}),
        Lane::Normal,
    );

    let pulled = op_audit_pull(
        &json!({"after_seq": after_seq, "limit": 128}),
        DispatchContext::empty(),
    )?;

    let events = pulled["events"].as_array().ok_or("events array")?;
    assert!(events
        .iter()
        .any(|event| event["type"].as_str() == Some(marker.as_str())));
    Ok(())
}

#[test]
fn auto_squash_audit_emits_triggered_and_completed() -> TestResult {
    let fixture = Fixture::new("auto_squash_completed")?;
    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let expected_hash = eos_protocol::manifest_root_hash(&manifest);
    let invocation_id = format!("autosquash-completed-{}", unique_suffix());
    let request = Request {
        op: "api.v1.write_file".to_owned(),
        invocation_id: invocation_id.clone(),
        args: json!({"layer_stack_root": &fixture.root}),
    };
    let response = json!({
        "timings": {
            "layer_stack.auto_squash.depth_before": 101.0,
            "layer_stack.auto_squash.depth_after": 3.0,
            "layer_stack.auto_squash.total_s": 0.25,
            "layer_stack.auto_squash.manifest_version": i64_to_f64_saturating(manifest.version),
        }
    });
    let after_seq = audit_after_seq()?;

    emit_auto_squash_audit(&request, &response);

    let events = layer_stack_events_after(after_seq, &invocation_id)?;
    assert_eq!(
        event_types(&events),
        vec![
            "layer_stack.squash_triggered",
            "layer_stack.squash_completed"
        ]
    );
    assert_eq!(
        events[0]["payload"]["layer_stack"]["squash_trigger_reason"],
        "post_publish_depth"
    );
    assert_eq!(
        events[0]["payload"]["layer_stack"]["squash_input_layers"],
        101
    );
    assert_eq!(
        events[1]["payload"]["layer_stack"]["squash_result_layers"],
        3
    );
    assert_eq!(
        events[1]["payload"]["layer_stack"]["manifest_root_hash"],
        expected_hash
    );
    Ok(())
}

#[test]
fn auto_squash_audit_emits_triggered_and_failed_for_race() -> TestResult {
    let invocation_id = format!("autosquash-raced-{}", unique_suffix());
    let request = Request {
        op: "api.v1.write_file".to_owned(),
        invocation_id: invocation_id.clone(),
        args: json!({}),
    };
    let response = json!({
        "timings": {
            "layer_stack.auto_squash.depth_before": 102.0,
            "layer_stack.auto_squash.total_s": 0.10,
            "layer_stack.auto_squash.raced": 1.0,
        }
    });
    let after_seq = audit_after_seq()?;

    emit_auto_squash_audit(&request, &response);

    let events = layer_stack_events_after(after_seq, &invocation_id)?;
    assert_eq!(
        event_types(&events),
        vec!["layer_stack.squash_triggered", "layer_stack.squash_failed"]
    );
    assert_eq!(
        events[1]["payload"]["layer_stack"]["squash_failure_kind"],
        "raced_or_plan_aborted"
    );
    assert_eq!(
        events[1]["payload"]["layer_stack"]["squash_trigger_reason"],
        "post_publish_depth"
    );
    Ok(())
}

#[test]
fn occ_service_cache_is_bounded_lru() -> TestResult {
    let mut cache = OccServiceCache::default();
    let base = std::env::temp_dir().join(format!("eosd-occ-cache-{}", unique_suffix()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base)?;

    let first = base.join("root-000");
    for index in 0..=OCC_SERVICE_CACHE_MAX {
        let root = base.join(format!("root-{index:03}"));
        std::fs::create_dir_all(&root)?;
        let transaction = LayerStackCommitTransaction { root: root.clone() };
        let service = Arc::new(OccService::new(CommitQueue::new(transaction))?);
        let (lookup, _rejected) = cache.insert_or_get(normalize_root_key(&root), service, 0.0);
        assert!(lookup.cache_created);
    }

    assert_eq!(cache.entries.len(), OCC_SERVICE_CACHE_MAX);
    assert_eq!(cache.stats.evictions_total, 1);

    let transaction = LayerStackCommitTransaction {
        root: first.clone(),
    };
    let service = Arc::new(OccService::new(CommitQueue::new(transaction))?);
    let (recreated, _rejected) = cache.insert_or_get(normalize_root_key(&first), service, 0.0);
    assert!(!recreated.cache_hit);
    assert!(recreated.cache_created);
    assert_eq!(recreated.evicted_count, 1);

    let _ = std::fs::remove_dir_all(base);
    Ok(())
}

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn transaction(fixture: &Fixture) -> LayerStackCommitTransaction {
    LayerStackCommitTransaction {
        root: fixture.root.clone(),
    }
}

fn publish_decision(
    path: &str,
    route: Route,
    base_hash: Option<String>,
) -> TestResult<eos_occ::PublishDecision> {
    Ok(eos_occ::PublishDecision {
        path: lp(path)?,
        route,
        base_hash,
        message: None,
    })
}

fn lp(path: &str) -> TestResult<LayerPath> {
    Ok(LayerPath::parse(path)?)
}

fn read_text(fixture: &Fixture, path: &str) -> TestResult<String> {
    Ok(LayerStack::open(fixture.root.clone())?.read_text(path)?.0)
}

fn timing_f64_value(timings: &serde_json::Map<String, Value>, key: &str) -> f64 {
    timings.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn audit_after_seq() -> TestResult<i64> {
    let snapshot = op_audit_snapshot(&json!({}), DispatchContext::empty())?;
    Ok(snapshot["snapshot"]["daemon"]["next_seq"]
        .as_i64()
        .unwrap_or(0)
        - 1)
}

fn layer_stack_events_after(after_seq: i64, invocation_id: &str) -> TestResult<Vec<Value>> {
    let pulled = op_audit_pull(
        &json!({"after_seq": after_seq, "limit": 128}),
        DispatchContext::empty(),
    )?;
    Ok(pulled["events"]
        .as_array()
        .ok_or("events array")?
        .iter()
        .filter(|event| {
            event["payload"]["layer_stack"]["operation_id"].as_str() == Some(invocation_id)
        })
        .cloned()
        .collect())
}

fn event_types(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| event["type"].as_str())
        .collect()
}

struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        Self::new_with_gitignores(label, &[])
    }

    fn new_with_gitignore(label: &str, gitignore: &str) -> TestResult<Self> {
        let seeds = if gitignore.is_empty() {
            Vec::new()
        } else {
            vec![("", gitignore)]
        };
        Self::new_with_gitignores(label, &seeds)
    }

    /// Seed one base layer with a `.gitignore` per `(dir, contents)` entry
    /// (`""` = workspace root) so nested / depth-sensitive routing is testable.
    fn new_with_gitignores(label: &str, gitignores: &[(&str, &str)]) -> TestResult<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "eosd-occ-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("layer-stack");
        let layer = root.join("layers").join("B000001-base");
        std::fs::create_dir_all(&layer)?;
        std::fs::create_dir_all(root.join("staging"))?;
        std::fs::write(layer.join("README.md"), "# README\n")?;
        for (dir, contents) in gitignores {
            let target = if dir.is_empty() {
                layer.join(".gitignore")
            } else {
                layer.join(dir).join(".gitignore")
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, contents)?;
        }
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "version": 1,
                "layers": [{"layer_id": "B000001-base", "path": "layers/B000001-base"}],
            }))?,
        )?;
        Ok(Self { base, root })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}
