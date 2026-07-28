fn main() {
    #[cfg(target_os = "linux")]
    if let Err(error) = linux::main() {
        eprintln!("workspace_session_materialization failed: {error}");
        std::process::exit(1);
    }

    #[cfg(not(target_os = "linux"))]
    println!("workspace_session_materialization: ignored on non-Linux runner");
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::{File, OpenOptions};
    use std::io::{BufRead, Write};
    use std::os::fd::RawFd;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use sandbox_observability_telemetry::Observer;
    use sandbox_runtime::command::{
        CommandOutput, CommandStatus, ExecCommandInput, ReadCommandLinesInput,
        WriteCommandStdinInput,
    };
    use sandbox_runtime::file::{EditInput, EditOp, ListInput, ReadInput, WriteInput};
    use sandbox_runtime::workspace_session::{
        CreateSessionRequest, FinalizePolicy, WorkspaceSessionHandler,
    };
    use sandbox_runtime::{
        CommandRuntimeConfig, FileRuntimeConfig, LayerstackRuntimeConfig, NamespaceExecutionCaps,
        NamespaceExecutionId, NamespaceExecutionRuntimeConfig, NetworkProfile, Rfc1918Egress,
        SandboxRuntimeConfig, SandboxRuntimeOperations, StorageAuthority, StorageRolloutMode,
        WorkspaceResourceCaps, WorkspaceRuntimeConfig, WorkspaceSessionId,
    };
    use sandbox_runtime_layerstack::service::{
        lookup_hidden_candidate_generation, materialize_hidden_candidate,
        CandidateGenerationSelection, NativeRouteCounters,
    };
    use sandbox_runtime_layerstack::{
        build_workspace_base, HiddenValidationPublication, LayerChange, LayerPath, LayerStack,
    };
    use sandbox_runtime_namespace_process::holder::{NamespaceNetwork, NsHolderError};
    use sandbox_runtime_namespace_process::runner::protocol::{NamespaceRunnerRequest, RunResult};
    use serde_json::{json, Value};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    pub(super) fn main() -> TestResult {
        let mut args = std::env::args();
        let _program = args.next();
        let args = args.collect::<Vec<_>>();
        match args.first().map(String::as_str) {
            Some("ns-holder") => run_holder(args.into_iter().skip(1)),
            Some("ns-runner") => run_runner(args.into_iter().skip(1)),
            Some("restart-child") => {
                let root = args
                    .get(1)
                    .ok_or("restart-child requires the fixture root")?;
                restart_child(Path::new(root))
            }
            Some("benchmark-server") => {
                let mode = args
                    .get(1)
                    .ok_or("benchmark-server requires strict or legacy mode")?;
                let depth = args
                    .get(2)
                    .ok_or("benchmark-server requires a layer depth")?
                    .parse::<usize>()?;
                if !matches!(depth, 1 | 64) {
                    return Err(
                        format!("benchmark-server depth must be 1 or 64, got {depth}").into(),
                    );
                }
                run_benchmark_server(BenchmarkMode::parse(mode)?, depth)
            }
            _ => run_cases(&args),
        }
    }

    fn run_cases(args: &[String]) -> TestResult {
        let filter = args.iter().find(|arg| !arg.starts_with('-'));
        let cases: [(&str, fn() -> TestResult); 5] = [
            (
                "strict_native_command_file_pty_and_no_fallback",
                strict_native_command_file_pty_and_no_fallback,
            ),
            (
                "logical_head_switch_keeps_old_session_on_exact_generation",
                logical_head_switch_keeps_old_session_on_exact_generation,
            ),
            (
                "shared_carrier_has_private_upper_work_and_mount_namespace",
                shared_carrier_has_private_upper_work_and_mount_namespace,
            ),
            (
                "restart_reaps_exact_persisted_admission_then_readmits",
                restart_reaps_exact_persisted_admission_then_readmits,
            ),
            (
                "missing_and_corrupt_candidates_fail_before_workspace_mutation",
                missing_and_corrupt_candidates_fail_before_workspace_mutation,
            ),
        ];
        let selected = cases
            .into_iter()
            .filter(|(name, _)| filter.is_none_or(|filter| name.contains(filter)))
            .collect::<Vec<_>>();
        println!("running {} tests", selected.len());
        for (name, case) in selected {
            case().map_err(|error| format!("{name}: {error}"))?;
            println!("test {name} ... ok");
        }
        Ok(())
    }

    fn strict_native_command_file_pty_and_no_fallback() -> TestResult {
        let fixture = Fixture::materialized("native-traffic")?;
        let expected = fixture.current_selection()?;
        let operations = fixture.operations();
        let route_before = operations.observe_layerstack()?.route;
        let session = create_session(&operations)?;
        assert_eq!(
            session.handle.snapshot.layer_paths,
            vec![expected.carrier_path.clone()]
        );
        let admission = fixture.admission_for(&session)?;
        assert_exact_admission(&admission, &expected)?;

        let command = operations.command.exec_command(ExecCommandInput {
            workspace_session_id: Some(session.workspace_session_id.clone()),
            cmd: "printf 'native-command\\n'; test -d /eos; test -z \"$(find /eos -mindepth 1 -maxdepth 1 -print -quit)\"".to_owned(),
            timeout_ms: Some(5_000),
            yield_time_ms: Some(5_000),
        })?;
        let command = await_terminal(&operations, command)?;
        assert_eq!(command.status, CommandStatus::Ok);
        assert!(command.output.contains("native-command"));

        let listed = operations.file.list(
            &operations.layerstack,
            &operations.workspace_session,
            ListInput {
                path: None,
                limit: Some(32),
                workspace_session_id: Some(session.workspace_session_id.clone()),
            },
        )?;
        assert!(listed.entries.iter().any(|entry| entry.name == "README.md"));
        let read = read_file(&operations, &session, "README.md")?;
        assert_eq!(read, "candidate-v1");
        operations.file.write(
            &operations.layerstack,
            &operations.workspace_session,
            WriteInput {
                path: "private.txt".to_owned(),
                content: "private-one\n".to_owned(),
                request_id: "stage04-private-write".to_owned(),
                workspace_session_id: Some(session.workspace_session_id.clone()),
            },
        )?;
        operations.file.edit(
            &operations.layerstack,
            &operations.workspace_session,
            EditInput {
                path: "private.txt".to_owned(),
                edits: vec![EditOp {
                    old_string: "one".to_owned(),
                    new_string: "edited".to_owned(),
                    replace_all: false,
                }],
                request_id: "stage04-private-edit".to_owned(),
                workspace_session_id: Some(session.workspace_session_id.clone()),
            },
        )?;
        assert_eq!(
            read_file(&operations, &session, "private.txt")?,
            "private-edited"
        );

        let interactive = operations.command.exec_command(ExecCommandInput {
            workspace_session_id: Some(session.workspace_session_id.clone()),
            cmd: "printf 'pty-ready\\n'; read line; printf 'pty:%s\\n' \"$line\"; sleep 30"
                .to_owned(),
            timeout_ms: Some(60_000),
            yield_time_ms: Some(100),
        })?;
        assert_eq!(interactive.status, CommandStatus::Running);
        assert!(interactive.output.contains("pty-ready"));
        let command_session_id = interactive
            .command_session_id
            .clone()
            .ok_or("running command omitted its command session id")?;
        let after_stdin = operations
            .command
            .write_command_stdin(WriteCommandStdinInput {
                command_session_id: command_session_id.clone(),
                stdin: "hello\n".to_owned(),
                yield_time_ms: Some(250),
            })?;
        assert_eq!(after_stdin.status, CommandStatus::Running);
        assert!(after_stdin.output.contains("pty:hello"));
        let cancelled = operations
            .command
            .write_command_stdin(WriteCommandStdinInput {
                command_session_id,
                stdin: "\u{3}".to_owned(),
                yield_time_ms: Some(1_000),
            })?;
        assert_eq!(cancelled.status, CommandStatus::Cancelled);

        assert_eq!(
            std::fs::read_to_string(expected.carrier_path.join("README.md"))?,
            "candidate-v1\n"
        );
        assert_eq!(fixture.current_selection()?, expected);
        assert_strict_route(&operations)?;
        let route_after = operations.observe_layerstack()?.route;
        assert!(route_after.native_route.lookup_count > route_before.native_route.lookup_count);
        assert!(
            route_after.native_route.validation_count > route_before.native_route.validation_count
        );
        assert_eq!(
            route_after.native_route.admission_count,
            route_before.native_route.admission_count + 1
        );
        assert_eq!(
            route_after.native_route.mount_count,
            route_before.native_route.mount_count + 1
        );
        assert_zero_forbidden_work(&route_before.native_route, &route_after.native_route);
        println!(
            "stage04-evidence:{}",
            serde_json::to_string(&json!({
                "case": "strict_native_command_file_pty_and_no_fallback",
                "selection": {
                    "root_id": expected.root_id,
                    "materialization_id": expected.materialization_id,
                    "generation": expected.generation,
                    "fence": expected.fence,
                },
                "route_before": route_before.native_route,
                "route_after": route_after.native_route,
                "authority": {
                    "configured_mode": "strict_candidate",
                    "read_authority": "legacy_v1",
                    "write_authority": "legacy_v1",
                    "fallback_count": route_after.fallback_count,
                },
            }))?
        );
        assert_forbidden_legacy_paths_absent(&fixture);
        destroy_session(&operations, session, 0)?;
        assert!(operations.shutdown().is_complete());
        Ok(())
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum BenchmarkMode {
        Strict,
        Legacy,
    }

    impl BenchmarkMode {
        fn parse(value: &str) -> TestResult<Self> {
            match value {
                "strict" => Ok(Self::Strict),
                "legacy" => Ok(Self::Legacy),
                other => Err(format!("unknown benchmark mode {other:?}").into()),
            }
        }

        fn label(self) -> &'static str {
            match self {
                Self::Strict => "strict",
                Self::Legacy => "legacy",
            }
        }

        fn rollout(self) -> StorageRolloutMode {
            match self {
                Self::Strict => StorageRolloutMode::StrictCandidate,
                Self::Legacy => StorageRolloutMode::Legacy,
            }
        }
    }

    struct BenchmarkServer {
        fixture: Fixture,
        operations: SandboxRuntimeOperations,
        mode: BenchmarkMode,
        session: Option<WorkspaceSessionHandler>,
        interactive_command_id: Option<NamespaceExecutionId>,
        route_before: NativeRouteCounters,
        write_index: u64,
    }

    impl BenchmarkServer {
        fn open_at_depth(mode: BenchmarkMode, depth: usize) -> TestResult<Self> {
            let fixture =
                Fixture::materialized_at_depth(&format!("benchmark-{}", mode.label()), depth)?;
            let mut config = fixture.config(mode.rollout());
            // S045-B05 requires matched 128-KiB and 1-MiB public file requests.
            // Keep the shipped 256-KiB default intact while giving both benchmark
            // arms the same explicit response cap required by this fixture.
            config.file.max_output_bytes = 1024 * 1024;
            let operations = SandboxRuntimeOperations::from_config(config, Observer::disabled());
            let route_before = operations.observe_layerstack_route().native_route;
            Ok(Self {
                fixture,
                operations,
                mode,
                session: None,
                interactive_command_id: None,
                route_before,
                write_index: 0,
            })
        }

        fn handle(&mut self, operation: &str) -> TestResult<Value> {
            match operation {
                "activate" => self.activate(),
                "deactivate" => {
                    self.deactivate()?;
                    Ok(json!({"status": "ok", "operation": operation}))
                }
                "command" => self.command(),
                "file-read" => self.file_read(),
                "file-write" => self.file_write(),
                "file-read-128k" => {
                    self.file_read_sized(operation, "benchmark-128k.txt", 128 * 1024)
                }
                "file-read-1m" => self.file_read_sized(operation, "benchmark-1m.txt", 1024 * 1024),
                "file-write-128k" => self.file_write_sized(operation, 128 * 1024),
                "file-write-1m" => self.file_write_sized(operation, 1024 * 1024),
                "file-random-read" => self.file_random_read(),
                "file-random-write" => self.file_write_sized(operation, 4 * 1024),
                "file-verify-writes" => self.file_verify_writes(),
                "file-metadata" => self.file_metadata(),
                "many-small-file" => self.many_small_file(),
                "observe-route" => self.observe_route(),
                "pty-create" => self.pty_create(),
                "pty-drain" => self.pty_drain(),
                "pty-write" => self.pty_write(),
                "pty-create-cycle" => self.pty_create_cycle(),
                "pty-drain-cycle" => self.pty_drain_cycle(),
                "pty-write-cycle" => self.pty_write_cycle(),
                "pty-control-c-cycle" => self.pty_control_cycle(operation, "\u{3}"),
                "pty-control-d-cycle" => self.pty_control_cycle(operation, "\u{4}"),
                "pty-cancel" => {
                    self.cancel_interactive()?;
                    Ok(json!({"status": "ok", "operation": operation}))
                }
                "shutdown" => {
                    self.shutdown()?;
                    Ok(json!({"status": "ok", "operation": operation}))
                }
                other => Err(format!("unknown benchmark operation {other:?}").into()),
            }
        }

        fn activate(&mut self) -> TestResult<Value> {
            if self.session.is_some() {
                return Err("benchmark session is already active".into());
            }
            let started = Instant::now();
            let session = create_session(&self.operations)?;
            let latency_ns = elapsed_ns(started);
            if self.mode == BenchmarkMode::Strict {
                let expected = self.fixture.current_selection()?;
                assert_exact_admission(&self.fixture.admission_for(&session)?, &expected)?;
            }
            self.session = Some(session);
            Ok(measured_response("activate", latency_ns))
        }

        fn deactivate(&mut self) -> TestResult {
            self.cancel_interactive()?;
            if let Some(session) = self.session.take() {
                destroy_session(&self.operations, session, 0)?;
            }
            Ok(())
        }

        fn command(&self) -> TestResult<Value> {
            let session_id = self.session_id()?;
            let started = Instant::now();
            let output = self.operations.command.exec_command(ExecCommandInput {
                workspace_session_id: Some(session_id),
                cmd: "ls".to_owned(),
                timeout_ms: Some(5_000),
                yield_time_ms: Some(5_000),
            })?;
            let output = await_terminal(&self.operations, output)?;
            let latency_ns = elapsed_ns(started);
            if output.status != CommandStatus::Ok {
                return Err(format!("benchmark no-op command ended as {:?}", output.status).into());
            }
            Ok(measured_response("command", latency_ns))
        }

        fn file_read(&self) -> TestResult<Value> {
            let session = self
                .session
                .as_ref()
                .ok_or("benchmark session is inactive")?;
            let started = Instant::now();
            let content = read_file(&self.operations, session, "benchmark.txt")?;
            let latency_ns = elapsed_ns(started);
            if content.len() != 4_096 || !content.bytes().all(|byte| byte == b'x') {
                return Err("benchmark file read returned unexpected content".into());
            }
            Ok(measured_response("file-read", latency_ns))
        }

        fn file_write(&mut self) -> TestResult<Value> {
            self.file_write_sized("file-write", 4_096)
        }

        fn file_read_sized(
            &self,
            operation: &str,
            path: &str,
            expected_bytes: usize,
        ) -> TestResult<Value> {
            let session = self
                .session
                .as_ref()
                .ok_or("benchmark session is inactive")?;
            let started = Instant::now();
            let content = read_file(&self.operations, session, path)?;
            let latency_ns = elapsed_ns(started);
            let content_matches = if path == "benchmark-1m.txt" {
                content.bytes().enumerate().all(|(index, byte)| {
                    if index < 255 * 4_097 && index % 4_097 == 4_096 {
                        byte == b'\n'
                    } else {
                        byte == b'x'
                    }
                })
            } else {
                content.bytes().all(|byte| byte == b'x')
            };
            if content.len() != expected_bytes || !content_matches {
                return Err(format!(
                    "benchmark sized file read {path:?} returned unexpected content ({} bytes; expected {expected_bytes})",
                    content.len()
                )
                .into());
            }
            Ok(work_response(operation, latency_ns, expected_bytes, 1))
        }

        fn file_write_sized(&mut self, operation: &str, bytes: usize) -> TestResult<Value> {
            let session_id = self.session_id()?;
            self.write_index += 1;
            let path = format!("benchmark-write-{bytes}.txt");
            let content = "y".repeat(bytes);
            let started = Instant::now();
            let output = self.operations.file.write(
                &self.operations.layerstack,
                &self.operations.workspace_session,
                WriteInput {
                    path: path.clone(),
                    content,
                    request_id: format!("stage04-benchmark-write-{}", self.write_index),
                    workspace_session_id: Some(session_id),
                },
            )?;
            let latency_ns = elapsed_ns(started);
            if output.bytes_written != bytes || output.path != path {
                return Err("benchmark file write returned an invalid result".into());
            }
            Ok(work_response(operation, latency_ns, bytes, 1))
        }

        fn file_verify_writes(&self) -> TestResult<Value> {
            let session = self
                .session
                .as_ref()
                .ok_or("benchmark session is inactive")?;
            for bytes in [4 * 1024, 128 * 1024, 1024 * 1024] {
                let path = format!("benchmark-write-{bytes}.txt");
                let content = read_file(&self.operations, session, &path)?;
                if content.len() != bytes || !content.bytes().all(|byte| byte == b'y') {
                    return Err(
                        format!("benchmark file write verification failed for {path:?}").into(),
                    );
                }
            }
            Ok(json!({"status": "ok", "operation": "file-verify-writes"}))
        }

        fn file_random_read(&self) -> TestResult<Value> {
            let session_id = self.session_id()?;
            let started = Instant::now();
            let output = self.operations.file.read(
                &self.operations.layerstack,
                &self.operations.workspace_session,
                ReadInput {
                    path: "benchmark-1m.txt".to_owned(),
                    offset: Some(128),
                    limit: Some(1),
                    workspace_session_id: Some(session_id),
                },
            )?;
            let latency_ns = elapsed_ns(started);
            if output.content.len() != 4 * 1024 || !output.content.bytes().all(|byte| byte == b'x')
            {
                return Err("benchmark random file read returned unexpected content".into());
            }
            Ok(work_response("file-random-read", latency_ns, 4 * 1024, 1))
        }

        fn file_metadata(&self) -> TestResult<Value> {
            let session_id = self.session_id()?;
            let started = Instant::now();
            let listed = self.operations.file.list(
                &self.operations.layerstack,
                &self.operations.workspace_session,
                ListInput {
                    path: None,
                    limit: Some(128),
                    workspace_session_id: Some(session_id),
                },
            )?;
            let latency_ns = elapsed_ns(started);
            if !listed
                .entries
                .iter()
                .any(|entry| entry.name == "benchmark-1m.txt" && entry.size == Some(1024 * 1024))
            {
                return Err("benchmark metadata listing omitted the 1-MiB fixture".into());
            }
            Ok(work_response(
                "file-metadata",
                latency_ns,
                0,
                listed.entries.len(),
            ))
        }

        fn many_small_file(&mut self) -> TestResult<Value> {
            let session_id = self.session_id()?;
            self.write_index += 1;
            let directory = format!("benchmark-small-{}", self.write_index);
            let started = Instant::now();
            for index in 0..16 {
                self.operations.file.write(
                    &self.operations.layerstack,
                    &self.operations.workspace_session,
                    WriteInput {
                        path: format!("{directory}-{index:02}.tmp"),
                        content: format!("small-{index:02}\n"),
                        request_id: format!(
                            "stage04-benchmark-many-small-{}-{index}",
                            self.write_index
                        ),
                        workspace_session_id: Some(session_id.clone()),
                    },
                )?;
            }
            let output = self.operations.command.exec_command(ExecCommandInput {
                workspace_session_id: Some(session_id),
                cmd: format!(
                    "sync; for path in {directory}-*.tmp; do mv \"$path\" \"${{path%.tmp}}.dat\"; done; sync; rm -f {directory}-*.dat"
                ),
                timeout_ms: Some(5_000),
                yield_time_ms: Some(5_000),
            })?;
            let output = await_terminal(&self.operations, output)?;
            let latency_ns = elapsed_ns(started);
            if output.status != CommandStatus::Ok {
                return Err(format!(
                    "benchmark many-small-file mutation ended as {:?}",
                    output.status
                )
                .into());
            }
            Ok(work_response(
                "many-small-file",
                latency_ns,
                16 * "small-00\n".len(),
                16,
            ))
        }

        fn observe_route(&self) -> TestResult<Value> {
            let route = self.operations.observe_layerstack_route();
            Ok(json!({
                "status": "ok",
                "operation": "observe-route",
                "mode": self.mode.label(),
                "configured_mode": route.configured_mode,
                "read_authority": route.read_authority,
                "write_authority": route.write_authority,
                "fallback_count": route.fallback_count,
                "fallback_reason_counts": route.fallback_reason_counts,
                "native_route": route.native_route,
            }))
        }

        fn pty_create(&mut self) -> TestResult<Value> {
            if self.interactive_command_id.is_some() {
                return Err("benchmark interactive command is already active".into());
            }
            let session_id = self.session_id()?;
            let started = Instant::now();
            let output = self.operations.command.exec_command(ExecCommandInput {
                workspace_session_id: Some(session_id),
                cmd:
                    "printf 'pty-ready\\n'; while read line; do printf 'pty:%s\\n' \"$line\"; done"
                        .to_owned(),
                timeout_ms: Some(60_000),
                yield_time_ms: Some(1),
            })?;
            let output = await_output(&self.operations, output, "pty-ready")?;
            let latency_ns = elapsed_ns(started);
            if output.status != CommandStatus::Running {
                return Err("benchmark PTY did not reach its ready boundary".into());
            }
            self.interactive_command_id = Some(
                output
                    .command_session_id
                    .ok_or("benchmark PTY omitted its command session id")?,
            );
            Ok(measured_response("pty-create", latency_ns))
        }

        fn pty_drain(&mut self) -> TestResult<Value> {
            let command_session_id = self
                .interactive_command_id
                .clone()
                .ok_or("benchmark interactive command is inactive")?;
            let started = Instant::now();
            let output = self
                .operations
                .command
                .write_command_stdin(WriteCommandStdinInput {
                    command_session_id,
                    stdin: "hello\n".to_owned(),
                    yield_time_ms: Some(5_000),
                })?;
            let output = await_output(&self.operations, output, "pty:hello")?;
            let latency_ns = elapsed_ns(started);
            if output.status != CommandStatus::Running {
                return Err("benchmark PTY response was not followed by a running command".into());
            }
            Ok(measured_response("pty-drain", latency_ns))
        }

        fn pty_write(&mut self) -> TestResult<Value> {
            let mut response = self.pty_drain()?;
            response["operation"] = json!("pty-write");
            Ok(response)
        }

        fn pty_create_cycle(&mut self) -> TestResult<Value> {
            let mut response = self.pty_create()?;
            self.cancel_interactive()?;
            response["operation"] = json!("pty-create-cycle");
            Ok(response)
        }

        fn pty_drain_cycle(&mut self) -> TestResult<Value> {
            self.pty_create()?;
            let mut response = self.pty_drain()?;
            self.cancel_interactive()?;
            response["operation"] = json!("pty-drain-cycle");
            Ok(response)
        }

        fn pty_write_cycle(&mut self) -> TestResult<Value> {
            self.pty_create()?;
            let mut response = self.pty_drain()?;
            self.cancel_interactive()?;
            response["operation"] = json!("pty-write-cycle");
            Ok(response)
        }

        fn pty_control_cycle(&mut self, operation: &str, control: &str) -> TestResult<Value> {
            self.pty_create()?;
            let command_session_id = self
                .interactive_command_id
                .take()
                .ok_or("benchmark interactive command is inactive")?;
            let started = Instant::now();
            let output = self
                .operations
                .command
                .write_command_stdin(WriteCommandStdinInput {
                    command_session_id,
                    stdin: control.to_owned(),
                    yield_time_ms: Some(1_000),
                })?;
            let latency_ns = elapsed_ns(started);
            if output.status != CommandStatus::Cancelled {
                return Err(format!(
                    "benchmark {operation} did not reach the exact cancelled state"
                )
                .into());
            }
            Ok(measured_response(operation, latency_ns))
        }

        fn cancel_interactive(&mut self) -> TestResult {
            if let Some(command_session_id) = self.interactive_command_id.take() {
                let output =
                    self.operations
                        .command
                        .write_command_stdin(WriteCommandStdinInput {
                            command_session_id,
                            stdin: "\u{3}".to_owned(),
                            yield_time_ms: Some(1_000),
                        })?;
                if output.status != CommandStatus::Cancelled {
                    return Err("benchmark PTY cleanup was not terminal".into());
                }
            }
            Ok(())
        }

        fn session_id(&self) -> TestResult<WorkspaceSessionId> {
            Ok(self
                .session
                .as_ref()
                .ok_or("benchmark session is inactive")?
                .workspace_session_id
                .clone())
        }

        fn shutdown(&mut self) -> TestResult {
            self.deactivate()?;
            if self.mode == BenchmarkMode::Strict {
                assert_strict_route(&self.operations)?;
                let route_after = self.operations.observe_layerstack_route().native_route;
                assert_zero_forbidden_work(&self.route_before, &route_after);
            }
            if !self.operations.shutdown().is_complete() {
                return Err("benchmark runtime shutdown was incomplete".into());
            }
            Ok(())
        }
    }

    fn measured_response(operation: &str, latency_ns: u64) -> Value {
        json!({
            "status": "ok",
            "operation": operation,
            "latency_ns": latency_ns,
        })
    }

    fn work_response(operation: &str, latency_ns: u64, bytes: usize, operations: usize) -> Value {
        json!({
            "status": "ok",
            "operation": operation,
            "latency_ns": latency_ns,
            "bytes": bytes,
            "operations": operations,
        })
    }

    fn elapsed_ns(started: Instant) -> u64 {
        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn await_output(
        operations: &SandboxRuntimeOperations,
        initial: CommandOutput,
        expected: &str,
    ) -> TestResult<CommandOutput> {
        let command_session_id = initial
            .command_session_id
            .clone()
            .ok_or("running command omitted its command session id")?;
        if initial.output.contains(expected) {
            return Ok(initial);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let output = operations
                .command
                .read_command_lines(ReadCommandLinesInput {
                    command_session_id: command_session_id.clone(),
                    start_offset: Some(0),
                    limit: Some(200),
                });
            if output.output.contains(expected) {
                return Ok(output);
            }
            if output.status != CommandStatus::Running {
                return Err(format!(
                    "benchmark command became {:?} before output {expected:?}",
                    output.status
                )
                .into());
            }
            std::thread::yield_now();
        }
        Err(format!("benchmark command did not emit {expected:?}").into())
    }

    fn run_benchmark_server(mode: BenchmarkMode, depth: usize) -> TestResult {
        let mut server = BenchmarkServer::open_at_depth(mode, depth)?;
        println!(
            "stage04-benchmark-ready:{}",
            serde_json::to_string(&json!({
                "schema_version": 1,
                "mode": mode.label(),
                "depth": depth,
            }))?
        );
        std::io::stdout().flush()?;
        for line in std::io::stdin().lock().lines() {
            let operation = line?;
            let shutdown = operation == "shutdown";
            match server.handle(&operation) {
                Ok(response) => println!("{}", serde_json::to_string(&response)?),
                Err(error) => {
                    println!(
                        "{}",
                        serde_json::to_string(&json!({
                            "status": "error",
                            "operation": operation,
                            "error": error.to_string(),
                        }))?
                    );
                    std::io::stdout().flush()?;
                    return Err(error);
                }
            }
            std::io::stdout().flush()?;
            if shutdown {
                return Ok(());
            }
        }
        Err("benchmark protocol input closed before shutdown".into())
    }

    fn logical_head_switch_keeps_old_session_on_exact_generation() -> TestResult {
        let fixture = Fixture::materialized("current-switch")?;
        let first = fixture.current_selection()?;
        let operations = fixture.operations();
        let old = create_session(&operations)?;
        let old_admission = fixture.admission_for(&old)?;

        operations.file.write(
            &operations.layerstack,
            &operations.workspace_session,
            WriteInput {
                path: "public-only.txt".to_owned(),
                content: "legacy-public-head\n".to_owned(),
                request_id: "stage04-public-head-change".to_owned(),
                workspace_session_id: None,
            },
        )?;
        assert!(read_file(&operations, &old, "public-only.txt").is_err());
        assert_eq!(fixture.current_selection()?, first);

        fixture.write_source("generation-two.txt", "candidate-v2\n")?;
        fixture.publish(&["generation-two.txt"])?;
        let built =
            materialize_hidden_candidate(&fixture.layer_stack_root, Duration::from_secs(30))?;
        let second = built.selection;
        assert_ne!(second.root_id, first.root_id);
        assert_ne!(second.materialization_id, first.materialization_id);
        let new = create_session(&operations)?;
        let new_admission = fixture.admission_for(&new)?;

        assert_eq!(
            old.handle.snapshot.layer_paths,
            vec![first.carrier_path.clone()]
        );
        assert_eq!(
            new.handle.snapshot.layer_paths,
            vec![second.carrier_path.clone()]
        );
        assert_eq!(read_file(&operations, &old, "README.md")?, "candidate-v1");
        assert!(read_file(&operations, &old, "generation-two.txt").is_err());
        assert_eq!(
            read_file(&operations, &new, "generation-two.txt")?,
            "candidate-v2"
        );
        assert_exact_admission(&old_admission, &first)?;
        assert_exact_admission(&new_admission, &second)?;
        assert_ne!(
            old_admission["lease"]["lease_id"],
            new_admission["lease"]["lease_id"]
        );
        assert_strict_route(&operations)?;
        destroy_session(&operations, old, 1)?;
        destroy_session(&operations, new, 0)?;
        assert!(operations.shutdown().is_complete());
        Ok(())
    }

    fn shared_carrier_has_private_upper_work_and_mount_namespace() -> TestResult {
        let fixture = Fixture::materialized("private-state")?;
        let expected = fixture.current_selection()?;
        let operations = fixture.operations();
        let left = create_session(&operations)?;
        let right = create_session(&operations)?;
        let left_record = fixture.record_for(&left.workspace_session_id.0)?;
        let right_record = fixture.record_for(&right.workspace_session_id.0)?;

        assert_eq!(
            left_record["candidate_admission"]["selection"]["carrier_path"],
            right_record["candidate_admission"]["selection"]["carrier_path"]
        );
        assert_eq!(
            left.handle.snapshot.layer_paths,
            vec![expected.carrier_path.clone()]
        );
        assert_eq!(
            right.handle.snapshot.layer_paths,
            vec![expected.carrier_path.clone()]
        );
        assert_ne!(left_record["upperdir"], right_record["upperdir"]);
        assert_ne!(left_record["workdir"], right_record["workdir"]);
        assert_ne!(
            mount_namespace_inode(left.handle.holder_pid)?,
            mount_namespace_inode(right.handle.holder_pid)?
        );

        for (session, content, request_id) in [
            (&left, "left-private\n", "stage04-left-write"),
            (&right, "right-private\n", "stage04-right-write"),
        ] {
            operations.file.write(
                &operations.layerstack,
                &operations.workspace_session,
                WriteInput {
                    path: "isolated.txt".to_owned(),
                    content: content.to_owned(),
                    request_id: request_id.to_owned(),
                    workspace_session_id: Some(session.workspace_session_id.clone()),
                },
            )?;
        }
        assert_eq!(
            read_file(&operations, &left, "isolated.txt")?,
            "left-private"
        );
        assert_eq!(
            read_file(&operations, &right, "isolated.txt")?,
            "right-private"
        );

        let left_id = left.workspace_session_id.clone();
        let right_id = right.workspace_session_id.clone();
        let left_command = operations.command.clone();
        let right_command = operations.command.clone();
        let left_thread = std::thread::spawn(move || {
            left_command.exec_command(ExecCommandInput {
                workspace_session_id: Some(left_id),
                cmd: "printf left-command".to_owned(),
                timeout_ms: Some(5_000),
                yield_time_ms: Some(5_000),
            })
        });
        let right_thread = std::thread::spawn(move || {
            right_command.exec_command(ExecCommandInput {
                workspace_session_id: Some(right_id),
                cmd: "printf right-command".to_owned(),
                timeout_ms: Some(5_000),
                yield_time_ms: Some(5_000),
            })
        });
        assert!(left_thread
            .join()
            .map_err(|_| "left command thread panicked")??
            .output
            .contains("left-command"));
        assert!(right_thread
            .join()
            .map_err(|_| "right command thread panicked")??
            .output
            .contains("right-command"));
        assert_eq!(
            std::fs::read_to_string(expected.carrier_path.join("README.md"))?,
            "candidate-v1\n"
        );
        assert_strict_route(&operations)?;
        destroy_session(&operations, left, 1)?;
        destroy_session(&operations, right, 0)?;
        assert!(operations.shutdown().is_complete());
        Ok(())
    }

    fn restart_reaps_exact_persisted_admission_then_readmits() -> TestResult {
        let fixture = Fixture::materialized("restart-reap")?;
        let child = std::process::Command::new(std::env::current_exe()?)
            .arg("restart-child")
            .arg(&fixture.root)
            .status()?;
        if !child.success() {
            return Err(format!("restart child exited with {child}").into());
        }
        let marker: Value =
            serde_json::from_slice(&std::fs::read(fixture.root.join("restart-child.json"))?)?;
        let stale_record = marker
            .get("record")
            .ok_or("restart child marker omitted the persisted record")?;
        let stale_run_dir = PathBuf::from(
            stale_record["scratch_dir"]
                .as_str()
                .ok_or("persisted scratch_dir is not text")?,
        );
        let stale_candidate_lease = stale_record["candidate_admission"]["lease"]["lease_id"]
            .as_str()
            .ok_or("persisted candidate lease id is not text")?;
        let stale_candidate_lease_path = fixture
            .layer_stack_root
            .join("refs/leases")
            .join(format!("materialization-{stale_candidate_lease}"));
        assert!(stale_run_dir.exists());
        assert!(stale_candidate_lease_path.exists());

        let operations = fixture.operations();
        assert!(!stale_run_dir.exists());
        assert!(!stale_candidate_lease_path.exists());
        assert!(fixture.manager_handles()?.is_empty());
        let current = fixture.current_selection()?;
        let admitted = create_session(&operations)?;
        assert_exact_admission(&fixture.admission_for(&admitted)?, &current)?;
        let output = operations.command.exec_command(ExecCommandInput {
            workspace_session_id: Some(admitted.workspace_session_id.clone()),
            cmd: "printf restart-readmitted".to_owned(),
            timeout_ms: Some(5_000),
            yield_time_ms: Some(5_000),
        })?;
        assert_eq!(output.status, CommandStatus::Ok);
        assert!(output.output.contains("restart-readmitted"));
        assert_strict_route(&operations)?;
        destroy_session(&operations, admitted, 0)?;
        assert!(operations.shutdown().is_complete());
        Ok(())
    }

    fn missing_and_corrupt_candidates_fail_before_workspace_mutation() -> TestResult {
        let missing = Fixture::unmaterialized("missing-candidate")?;
        let missing_operations = missing.operations();
        let missing_error = missing_operations
            .workspace_session
            .create_workspace_session(create_request())
            .expect_err("strict admission without CURRENT must fail");
        assert!(missing_error
            .to_string()
            .contains("prebuilt native generation"));
        assert!(missing.manager_handles()?.is_empty());
        assert_no_workspace_run_dirs(&missing)?;
        assert_strict_route(&missing_operations)?;
        assert!(missing_operations.shutdown().is_complete());

        let corrupt = Fixture::materialized("corrupt-candidate")?;
        let selection = corrupt.current_selection()?;
        std::fs::remove_dir_all(&selection.carrier_path)?;
        std::fs::write(&selection.carrier_path, "not-a-native-carrier\n")?;
        let corrupt_operations = corrupt.operations();
        let corrupt_error = corrupt_operations
            .workspace_session
            .create_workspace_session(create_request())
            .expect_err("strict admission with a corrupt carrier must fail");
        assert!(corrupt_error
            .to_string()
            .contains("strict candidate exact admission"));
        assert!(corrupt.manager_handles()?.is_empty());
        assert_no_workspace_run_dirs(&corrupt)?;
        assert_strict_route(&corrupt_operations)?;
        assert_forbidden_legacy_paths_absent(&corrupt);
        assert!(corrupt_operations.shutdown().is_complete());
        Ok(())
    }

    fn restart_child(root: &Path) -> TestResult {
        let fixture = Fixture::existing(root)?;
        let operations = fixture.operations();
        let session = create_session(&operations)?;
        let output = operations.command.exec_command(ExecCommandInput {
            workspace_session_id: Some(session.workspace_session_id.clone()),
            cmd: "printf restart-child".to_owned(),
            timeout_ms: Some(5_000),
            yield_time_ms: Some(5_000),
        })?;
        if output.status != CommandStatus::Ok || !output.output.contains("restart-child") {
            return Err("restart child command did not complete natively".into());
        }
        let record = fixture.record_for(&session.workspace_session_id.0)?;
        std::fs::write(
            root.join("restart-child.json"),
            serde_json::to_vec_pretty(&json!({
                "workspace_session_id": session.workspace_session_id.0,
                "record": record,
            }))?,
        )?;
        std::process::exit(0);
    }

    fn create_request() -> CreateSessionRequest {
        CreateSessionRequest {
            network: NetworkProfile::Shared,
            finalize_policy: FinalizePolicy::NoOp,
        }
    }

    fn create_session(
        operations: &SandboxRuntimeOperations,
    ) -> TestResult<WorkspaceSessionHandler> {
        Ok(operations
            .workspace_session
            .create_workspace_session(create_request())?)
    }

    fn destroy_session(
        operations: &SandboxRuntimeOperations,
        session: WorkspaceSessionHandler,
        expected_active_leases_after: usize,
    ) -> TestResult {
        let result = operations
            .workspace_session
            .guarded_destroy(session.workspace_session_id, Some(0.1))?;
        assert_eq!(result.lease_released, Some(true));
        assert_eq!(result.active_leases_after, expected_active_leases_after);
        Ok(())
    }

    fn await_terminal(
        operations: &SandboxRuntimeOperations,
        initial: CommandOutput,
    ) -> TestResult<CommandOutput> {
        if initial.status != CommandStatus::Running {
            return Ok(initial);
        }
        let command_session_id = initial
            .command_session_id
            .clone()
            .ok_or("running command omitted its command session id")?;
        for _ in 0..100 {
            let output = operations
                .command
                .read_command_lines(ReadCommandLinesInput {
                    command_session_id: command_session_id.clone(),
                    start_offset: Some(0),
                    limit: Some(200),
                });
            if output.status != CommandStatus::Running {
                return Ok(output);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err("command did not become terminal".into())
    }

    fn read_file(
        operations: &SandboxRuntimeOperations,
        session: &WorkspaceSessionHandler,
        path: &str,
    ) -> TestResult<String> {
        Ok(operations
            .file
            .read(
                &operations.layerstack,
                &operations.workspace_session,
                ReadInput {
                    path: path.to_owned(),
                    offset: None,
                    limit: None,
                    workspace_session_id: Some(session.workspace_session_id.clone()),
                },
            )?
            .content)
    }

    fn assert_exact_admission(
        admission: &Value,
        expected: &CandidateGenerationSelection,
    ) -> TestResult {
        assert_eq!(
            admission["selection"]["materialization_id"],
            expected.materialization_id
        );
        assert_eq!(admission["selection"]["generation"], expected.generation);
        assert_eq!(admission["selection"]["fence"], expected.fence);
        assert_eq!(
            admission["lease"]["materialization_id"],
            expected.materialization_id
        );
        assert_eq!(admission["lease"]["generation"], expected.generation);
        assert_eq!(admission["lease"]["fence"], expected.fence);
        assert_eq!(
            admission["selection"]["carrier_path"],
            expected.carrier_path.to_string_lossy().as_ref()
        );
        Ok(())
    }

    fn assert_strict_route(operations: &SandboxRuntimeOperations) -> TestResult {
        let route = operations.observe_layerstack()?.route;
        assert_eq!(route.configured_mode, StorageRolloutMode::StrictCandidate);
        assert_eq!(route.write_authority, StorageAuthority::LegacyV1);
        assert_eq!(route.read_authority, StorageAuthority::LegacyV1);
        assert_eq!(route.fallback_count, 0);
        assert!(route.fallback_reason_counts.is_empty());
        Ok(())
    }

    fn assert_zero_forbidden_work(before: &NativeRouteCounters, after: &NativeRouteCounters) {
        assert_eq!(after.cdc_count, before.cdc_count);
        assert_eq!(after.object_traversal_count, before.object_traversal_count);
        assert_eq!(after.hash_count, before.hash_count);
        assert_eq!(after.locator_merge_count, before.locator_merge_count);
        assert_eq!(after.compaction_count, before.compaction_count);
        assert_eq!(after.pack_count, before.pack_count);
        assert_eq!(after.gc_count, before.gc_count);
        assert_eq!(after.squash_count, before.squash_count);
        assert_eq!(after.materialization_count, before.materialization_count);
        assert_eq!(after.fallback_count, before.fallback_count);
    }

    fn assert_forbidden_legacy_paths_absent(fixture: &Fixture) {
        assert!(!fixture.root.join("legacy").exists());
        assert!(!fixture.layer_stack_root.join("refs/legacy").exists());
        assert!(!fixture.root.join("namespace_execution").exists());
    }

    fn assert_no_workspace_run_dirs(fixture: &Fixture) -> TestResult {
        if !fixture.scratch_root.exists() {
            return Ok(());
        }
        let entries = std::fs::read_dir(&fixture.scratch_root)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        assert!(entries
            .iter()
            .all(|name| name == "manager.json" || name == "storage"));
        Ok(())
    }

    fn mount_namespace_inode(pid: i32) -> TestResult<u64> {
        Ok(std::fs::metadata(format!("/proc/{pid}/ns/mnt"))?.ino())
    }

    struct Fixture {
        root: PathBuf,
        layer_stack_root: PathBuf,
        workspace_root: PathBuf,
        scratch_root: PathBuf,
        owns_paths: bool,
    }

    impl Fixture {
        fn materialized(label: &str) -> TestResult<Self> {
            Self::materialized_at_depth(label, 1)
        }

        fn materialized_at_depth(label: &str, depth: usize) -> TestResult<Self> {
            let fixture = Self::unmaterialized(label)?;
            for index in 1..depth {
                let path = format!("depth-{index:02}.txt");
                fixture.write_source(&path, &format!("depth-{index}\n"))?;
                fixture.publish(&[&path])?;
            }
            materialize_hidden_candidate(&fixture.layer_stack_root, Duration::from_secs(30))?;
            Ok(fixture)
        }

        fn unmaterialized(label: &str) -> TestResult<Self> {
            let suffix = unique_suffix();
            let root = PathBuf::from(format!(
                "/eos/stage04-workspace-materialization-{label}-{}-{suffix}",
                std::process::id()
            ));
            let workspace_root = PathBuf::from(format!(
                "/tmp/stage04-workspace-materialization-{label}-{}-{suffix}",
                std::process::id()
            ));
            let layer_stack_root = root.join("layer-stack");
            let scratch_root = root.join("workspace-scratch");
            std::fs::create_dir_all(&workspace_root)?;
            std::fs::write(workspace_root.join("README.md"), "candidate-v1\n")?;
            std::fs::write(workspace_root.join("base-only.txt"), "base\n")?;
            std::fs::write(workspace_root.join("benchmark.txt"), "x".repeat(4_096))?;
            std::fs::write(
                workspace_root.join("benchmark-128k.txt"),
                "x".repeat(128 * 1024),
            )?;
            let mut benchmark_1m = String::with_capacity(1024 * 1024);
            for _ in 0..255 {
                benchmark_1m.push_str(&"x".repeat(4 * 1024));
                benchmark_1m.push('\n');
            }
            benchmark_1m.push_str(&"x".repeat(1024 * 1024 - benchmark_1m.len()));
            std::fs::write(workspace_root.join("benchmark-1m.txt"), benchmark_1m)?;
            build_workspace_base(&layer_stack_root, &workspace_root, false)?;
            let fixture = Self {
                root,
                layer_stack_root,
                workspace_root,
                scratch_root,
                owns_paths: true,
            };
            std::fs::write(
                fixture.root.join("workspace-root"),
                fixture.workspace_root.to_string_lossy().as_bytes(),
            )?;
            fixture.publish(&[
                "README.md",
                "base-only.txt",
                "benchmark.txt",
                "benchmark-128k.txt",
                "benchmark-1m.txt",
            ])?;
            Ok(fixture)
        }

        fn existing(root: &Path) -> TestResult<Self> {
            Ok(Self {
                root: root.to_path_buf(),
                layer_stack_root: root.join("layer-stack"),
                workspace_root: PathBuf::from(
                    std::fs::read_to_string(root.join("workspace-root"))?.trim(),
                ),
                scratch_root: root.join("workspace-scratch"),
                owns_paths: false,
            })
        }

        fn operations(&self) -> SandboxRuntimeOperations {
            self.operations_with_mode(StorageRolloutMode::StrictCandidate)
        }

        fn operations_with_mode(
            &self,
            rollout_mode: StorageRolloutMode,
        ) -> SandboxRuntimeOperations {
            std::fs::write(
                self.root.join("workspace-root"),
                self.workspace_root.to_string_lossy().as_bytes(),
            )
            .expect("persist fixture workspace root");
            SandboxRuntimeOperations::from_config(self.config(rollout_mode), Observer::disabled())
        }

        fn config(&self, rollout_mode: StorageRolloutMode) -> SandboxRuntimeConfig {
            SandboxRuntimeConfig {
                workspace: WorkspaceRuntimeConfig {
                    workspace_root: self.workspace_root.clone(),
                    layer_stack_root: self.layer_stack_root.clone(),
                    scratch_root: self.scratch_root.clone(),
                    caps: WorkspaceResourceCaps {
                        setup_timeout_s: 10.0,
                        exit_grace_s: 0.1,
                        rfc1918_egress: Rfc1918Egress::Allow,
                        freeze_budget_s: 0.5,
                    },
                },
                namespace_execution: NamespaceExecutionRuntimeConfig {
                    scratch_root: None,
                    caps: NamespaceExecutionCaps::default(),
                },
                layerstack: LayerstackRuntimeConfig {
                    rollout_mode,
                    autosquash_squash_at_n_layers: None,
                    ..LayerstackRuntimeConfig::default()
                },
                command: CommandRuntimeConfig::default(),
                file: FileRuntimeConfig::default(),
                cgroup_root: None,
                workload_cgroup_limits: None,
                workload_cgroup_unavailable_reason: None,
                mpla_storage_admin_profile:
                    sandbox_runtime::StorageAdminCapabilityProfile::Production,
            }
        }

        fn write_source(&self, path: &str, content: &str) -> TestResult {
            std::fs::write(self.workspace_root.join(path), content)?;
            Ok(())
        }

        fn publish(&self, paths: &[&str]) -> TestResult {
            let changes = paths
                .iter()
                .map(|path| {
                    Ok(LayerChange::Write {
                        path: LayerPath::parse(path)?,
                        content: std::fs::read(self.workspace_root.join(path))?,
                    })
                })
                .collect::<TestResult<Vec<_>>>()?;
            let publication = HiddenValidationPublication {
                publication_id: publication_id(),
                changes,
                source_layer_dir: self.workspace_root.clone(),
                public_root_hash: "stage04-private-candidate".to_owned(),
            };
            LayerStack::open(self.layer_stack_root.clone())?
                .publish_hidden_validation(publication)?;
            Ok(())
        }

        fn current_selection(&self) -> TestResult<CandidateGenerationSelection> {
            lookup_hidden_candidate_generation(&self.layer_stack_root)?
                .ok_or_else(|| "candidate CURRENT is missing".into())
        }

        fn manager_handles(&self) -> TestResult<Vec<Value>> {
            let path = self.scratch_root.join("manager.json");
            if !path.exists() {
                return Ok(Vec::new());
            }
            let payload: Value = serde_json::from_slice(&std::fs::read(path)?)?;
            assert_eq!(payload["schema_version"], 2);
            Ok(payload["handles"]
                .as_array()
                .ok_or("manager handles is not an array")?
                .clone())
        }

        fn record_for(&self, workspace_session_id: &str) -> TestResult<Value> {
            self.manager_handles()?
                .into_iter()
                .find(|record| record["workspace_handle_id"] == workspace_session_id)
                .ok_or_else(|| {
                    format!("missing persisted handle for {workspace_session_id}").into()
                })
        }

        fn admission_for(&self, session: &WorkspaceSessionHandler) -> TestResult<Value> {
            Ok(self.record_for(&session.workspace_session_id.0)?["candidate_admission"].clone())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if self.owns_paths {
                let _ = std::fs::remove_dir_all(&self.root);
                let _ = std::fs::remove_dir_all(&self.workspace_root);
            }
        }
    }

    fn publication_id() -> [u8; 16] {
        let suffix = unique_suffix();
        let mut id = [0_u8; 16];
        id[..8].copy_from_slice(&u64::from(std::process::id()).to_be_bytes());
        id[8..].copy_from_slice(&suffix.to_be_bytes());
        id
    }

    fn unique_suffix() -> u64 {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn run_holder(mut args: impl Iterator<Item = String>) -> TestResult {
        let readiness_fd = parse_fd(args.next(), "readiness fd")?;
        let control_fd = parse_fd(args.next(), "control fd")?;
        let network = match args.next().as_deref() {
            Some("shared") => NamespaceNetwork::Shared,
            Some("isolated") => NamespaceNetwork::Isolated,
            value => return Err(format!("invalid holder network mode: {value:?}").into()),
        };
        match sandbox_runtime_namespace_process::holder::run(readiness_fd, control_fd, network) {
            Ok(()) => Ok(()),
            Err(NsHolderError::ControlPipeClosed) => {
                std::process::exit(NsHolderError::CONTROL_CLOSED_EXIT)
            }
            Err(NsHolderError::UnexpectedToken) => {
                std::process::exit(NsHolderError::UNEXPECTED_TOKEN_EXIT)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn run_runner(args: impl Iterator<Item = String>) -> TestResult {
        let mut request_fd = None;
        let mut result_fd = None;
        let mut mode = None;
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--mount-overlay" | "--remount-overlay" | "--file-op" | "--shell" => {
                    mode = Some(arg)
                }
                "--request-fd" => request_fd = Some(parse_fd(args.next(), "request fd")?),
                "--result-fd" => result_fd = Some(parse_fd(args.next(), "result fd")?),
                other => return Err(format!("unexpected ns-runner argument {other:?}").into()),
            }
        }
        let request_fd = request_fd.ok_or_else(|| std::io::Error::other("missing request fd"))?;
        let result_fd = result_fd.ok_or_else(|| std::io::Error::other("missing result fd"))?;
        let request: NamespaceRunnerRequest = serde_json::from_reader(open_fd(request_fd)?)?;
        let hidden_paths = [PathBuf::from("/eos")];
        let result = match mode.as_deref() {
            Some("--mount-overlay") => {
                match sandbox_runtime_namespace_process::runner::setns::setns_overlay_mount(
                    &request,
                    &hidden_paths,
                ) {
                    Ok(()) => RunResult {
                        exit_code: 0,
                        payload: json!({"success": true, "status": "ok"}),
                    },
                    Err(error) => runner_error("overlay mount", error),
                }
            }
            Some("--remount-overlay") => {
                sandbox_runtime_namespace_process::runner::setns::setns_remount_overlay(
                    &request,
                    &hidden_paths,
                )
                .unwrap_or_else(|error| runner_error("overlay remount", error))
            }
            Some("--file-op") => {
                sandbox_runtime_namespace_process::runner::file_op::run_file_op(&request)
            }
            Some("--shell") => sandbox_runtime_namespace_process::runner::run(&request)?,
            mode => return Err(format!("invalid ns-runner mode {mode:?}").into()),
        };
        let mut output = open_fd_for_write(result_fd)?;
        output.write_all(&serde_json::to_vec(&result)?)?;
        Ok(())
    }

    fn runner_error(step: &str, error: impl std::fmt::Display) -> RunResult {
        RunResult {
            exit_code: 1,
            payload: json!({"error": format!("ns-runner {step} failed: {error}")}),
        }
    }

    fn parse_fd(value: Option<String>, name: &str) -> TestResult<RawFd> {
        Ok(value.ok_or_else(|| format!("missing {name}"))?.parse()?)
    }

    fn open_fd(fd: RawFd) -> std::io::Result<File> {
        File::open(format!("/proc/self/fd/{fd}"))
    }

    fn open_fd_for_write(fd: RawFd) -> std::io::Result<File> {
        OpenOptions::new()
            .write(true)
            .open(format!("/proc/self/fd/{fd}"))
    }
}
