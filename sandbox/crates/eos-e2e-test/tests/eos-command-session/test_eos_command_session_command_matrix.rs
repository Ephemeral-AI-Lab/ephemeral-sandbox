use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};
use eos_e2e_test::{next_invocation_id, unique_suffix, NodePool};
use eos_protocol::ops;
use serde_json::{json, Value};

use crate::support::{
    array, as_i64, as_str, live_pool_or_skip, stdout, wait_for_active_leases,
    wait_for_session_count,
};

struct CommandFamily {
    name: &'static str,
    variants: Vec<CommandVariant>,
}

struct CommandVariant {
    name: &'static str,
    cmd: String,
    stdout_contains: String,
    changed_paths: Vec<String>,
}

macro_rules! command_family_test {
    ($test_name:ident, $family:literal) => {
        #[test]
        fn $test_name() -> Result<()> {
            run_command_family($family)
        }
    };
}

command_family_test!(command_matrix_builtin_variants, "builtin");
command_family_test!(command_matrix_pipeline_variants, "pipeline");
command_family_test!(command_matrix_redirection_variants, "redirection");
command_family_test!(command_matrix_append_variants, "append");
command_family_test!(command_matrix_heredoc_variants, "heredoc");
command_family_test!(command_matrix_filesystem_variants, "filesystem");
command_family_test!(command_matrix_grep_variants, "grep");
command_family_test!(command_matrix_sed_variants, "sed");
command_family_test!(command_matrix_awk_variants, "awk");
command_family_test!(command_matrix_python_variants, "python");
command_family_test!(command_matrix_stderr_variants, "stderr");
command_family_test!(command_matrix_json_and_bytes_variants, "json-and-bytes");

#[test]
fn command_matrix_inventory_covers_ten_shell_families() -> Result<()> {
    let families = command_families("command-matrix/inventory");
    ensure!(
        families.len() >= 10,
        "command matrix should cover at least ten command families"
    );
    for family in &families {
        ensure!(
            family.variants.len() >= 2,
            "command family {} should have multiple variants",
            family.name
        );
    }
    Ok(())
}

fn run_command_family(family_name: &str) -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "command-matrix/{family_name}/{}",
        unique_suffix().replace('-', "_")
    );
    let timeout_s = workload_timeout_s(&pool);
    let family = command_families(&dir)
        .into_iter()
        .find(|family| family.name == family_name)
        .with_context(|| format!("unknown command family {family_name}"))?;

    let started = Instant::now();
    let mut executed = 0;
    ensure!(
        family.variants.len() >= 2,
        "command family {} should have multiple variants",
        family.name
    );
    for variant in &family.variants {
        let call_started = Instant::now();
        let response = lease.call_ok(
            ops::API_V1_EXEC_COMMAND,
            json!({
                "cmd": variant.cmd,
                "yield_time_ms": 1000,
                "timeout_seconds": timeout_s,
                "max_output_tokens": 6000
            }),
        )?;
        let elapsed = call_started.elapsed();
        assert_command_ok(&response, family.name, variant.name)?;
        ensure!(
            output_contains(&response, &variant.stdout_contains),
            "{}:{} stdout should contain {:?}: {}",
            family.name,
            variant.name,
            variant.stdout_contains,
            response
        );
        assert_changed_paths(&response, &variant.changed_paths)?;
        assert_bounded_command_resources(&response, elapsed, timeout_s)?;
        executed += 1;
    }

    ensure!(
        executed >= 2,
        "command family {} should run at least two command variants, got {executed}",
        family.name
    );
    ensure!(
        started.elapsed() < Duration::from_secs(timeout_s + 10),
        "command family {} should stay bounded by the per-command timeout",
        family.name
    );
    wait_for_session_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn stdin_prompt_cursor_collect_and_cancel_variants() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let prompt_cmd = concat!(
        "python3 -u -c '",
        "import sys,time; ",
        "print(\"prompt:one\", flush=True); ",
        "first=sys.stdin.readline().strip(); ",
        "print(\"reply:one:\" + first, flush=True); ",
        "print(\"prompt:two\", flush=True); ",
        "second=sys.stdin.readline().strip(); ",
        "print(\"reply:two:\" + second, flush=True); ",
        "time.sleep(60)'"
    );
    let started = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": prompt_cmd,
            "yield_time_ms": 500,
            "timeout_seconds": workload_timeout_s(&pool) + 60,
            "max_output_tokens": 2000
        }),
    )?;
    ensure!(
        output_contains(&started, "prompt:one"),
        "prompt session should expose the first prompt: {started}"
    );
    let session_id = as_str(&started, "command_session_id")?.to_owned();

    let body = (|| -> Result<()> {
        let first = lease.call_ok(
            ops::API_V1_WRITE_STDIN,
            json!({
                "command_session_id": session_id,
                "chars": "alpha payload\n",
                "yield_time_ms": 1500,
                "max_output_tokens": 2000
            }),
        )?;
        ensure!(
            output_contains(&first, "reply:one:alpha payload")
                && output_contains(&first, "prompt:two"),
            "first stdin write should answer prompt one and surface prompt two: {first}"
        );
        ensure!(
            !output_contains(&first, "prompt:one"),
            "stdin output cursor must not replay the first prompt: {first}"
        );

        // The command-session read surface is cursor polling with an empty
        // `write_stdin`, plus `collect_completed` for terminal messages.
        let quiet_poll = lease.call_ok(
            ops::API_V1_WRITE_STDIN,
            json!({
                "command_session_id": session_id,
                "chars": "",
                "yield_time_ms": 250,
                "max_output_tokens": 2000
            }),
        )?;
        ensure!(
            !output_contains(&quiet_poll, "reply:one")
                && !output_contains(&quiet_poll, "prompt:two"),
            "empty cursor poll must not replay consumed prompt output: {quiet_poll}"
        );

        let second = lease.call_ok(
            ops::API_V1_WRITE_STDIN,
            json!({
                "command_session_id": session_id,
                "chars": "beta payload\n",
                "yield_time_ms": 1500,
                "max_output_tokens": 2000
            }),
        )?;
        ensure!(
            output_contains(&second, "reply:two:beta payload"),
            "second stdin write should answer prompt two: {second}"
        );
        ensure!(
            !output_contains(&second, "reply:one:alpha payload"),
            "second stdin write must not replay the first answer: {second}"
        );

        let not_done = lease.call_ok(
            ops::API_V1_COMMAND_COLLECT_COMPLETED,
            json!({"command_session_ids": [session_id.clone()]}),
        )?;
        ensure!(
            array(&not_done, "completions")?.is_empty(),
            "sleeping prompt session must not produce a completion before cancellation: {not_done}"
        );

        let cancel = lease.call(
            ops::API_V1_COMMAND_CANCEL,
            json!({"command_session_id": session_id, "max_output_tokens": 2000}),
        )?;
        ensure_terminalish_status(&cancel)?;
        wait_for_session_count(&lease, 0)?;
        wait_for_active_leases(&lease, 0)?;
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(
            ops::API_V1_COMMAND_CANCEL,
            json!({"command_session_id": session_id, "max_output_tokens": 2000}),
        );
        let _ = wait_for_session_count(&lease, 0);
    }
    body
}

#[test]
fn parallel_command_matrix_load_stays_bounded() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let levels = pool.workload().concurrency_levels.clone();
    ensure!(
        levels == [1, 3, 6, 12],
        "command-session workload.concurrency_levels should use [1, 3, 6, 12], got {levels:?}"
    );
    let lease = pool.acquire()?;
    let timeout_s = workload_timeout_s(&pool);

    for level in levels {
        let dir = format!(
            "command-load/level-{level}/{}",
            unique_suffix().replace('-', "_")
        );
        let barrier = Arc::new(Barrier::new(level));
        let handles: Vec<_> = (0..level)
            .map(|index| {
                let client = lease.client().clone();
                let root = lease.root().to_owned();
                let caller_id = lease.caller_id().to_owned();
                let barrier = Arc::clone(&barrier);
                let cmd = parallel_command(&dir, level, index);
                thread::spawn(move || -> Result<(usize, Value, Duration)> {
                    barrier.wait();
                    let started = Instant::now();
                    let response = request_with_identity(
                        &client,
                        ops::API_V1_EXEC_COMMAND,
                        &root,
                        &caller_id,
                        json!({
                            "cmd": cmd,
                            "yield_time_ms": 1000,
                            "timeout_seconds": timeout_s,
                            "max_output_tokens": 3000
                        }),
                    )?;
                    Ok((index, response, started.elapsed()))
                })
            })
            .collect();

        for handle in handles {
            let (index, response, elapsed) = handle
                .join()
                .map_err(|_| anyhow!("parallel command worker panicked"))??;
            assert_command_ok(&response, "parallel-load", "worker")?;
            ensure!(
                output_contains(&response, &format!("worker:{level}:{index}")),
                "parallel worker stdout should include its marker: {response}"
            );
            assert_changed_paths(&response, &[format!("{dir}/worker-{index}/result.txt")])?;
            assert_bounded_command_resources(&response, elapsed, timeout_s)?;
        }

        for index in 0..level {
            let read = lease.call_ok(
                ops::API_V1_READ_FILE,
                json!({"path": format!("{dir}/worker-{index}/result.txt")}),
            )?;
            ensure!(
                as_str(&read, "content")?.contains(&format!("worker:{level}:{index}")),
                "parallel worker publish should be durable: {read}"
            );
        }
        let metrics = wait_for_active_leases(&lease, 0)?;
        ensure!(
            as_i64(&metrics, "active_leases")? == 0,
            "parallel command level {level} should not leak leases: {metrics}"
        );
    }
    wait_for_session_count(&lease, 0)?;
    Ok(())
}

#[test]
fn parallel_prompt_sessions_ladder_stays_isolated_and_bounded() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let levels = pool.workload().concurrency_levels.clone();
    ensure!(
        levels == [1, 3, 6, 12],
        "command-session workload.concurrency_levels should use [1, 3, 6, 12], got {levels:?}"
    );
    let lease = pool.acquire()?;
    let timeout_s = workload_timeout_s(&pool);

    for level in levels {
        let barrier = Arc::new(Barrier::new(level));
        let handles: Vec<_> = (0..level)
            .map(|index| {
                let client = lease.client().clone();
                let root = lease.root().to_owned();
                let caller_id = lease.caller_id().to_owned();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || -> Result<()> {
                    barrier.wait();
                    let marker = format!("prompt-worker:{level}:{index}");
                    let prompt_cmd = format!(
                        "python3 -u -c 'import sys,time; \
marker={marker:?}; \
print(\"prompt:\" + marker, flush=True); \
payload=sys.stdin.readline().strip(); \
print(\"reply:\" + marker + \":\" + payload, flush=True); \
time.sleep(60)'"
                    );
                    let started = request_with_identity(
                        &client,
                        ops::API_V1_EXEC_COMMAND,
                        &root,
                        &caller_id,
                        json!({
                            "cmd": prompt_cmd,
                            "yield_time_ms": 500,
                            "timeout_seconds": timeout_s + 60,
                            "max_output_tokens": 2000
                        }),
                    )?;
                    ensure!(
                        as_str(&started, "status")? == "running",
                        "parallel prompt worker should stay running: {started}"
                    );
                    ensure!(
                        output_contains(&started, &format!("prompt:{marker}")),
                        "parallel prompt worker should expose its own prompt: {started}"
                    );
                    let session_id = as_str(&started, "command_session_id")?.to_owned();
                    let prompt_needle = format!("prompt:{marker}");
                    let reply_needle = format!("reply:{marker}:payload-{level}-{index}");

                    let answered = request_with_identity(
                        &client,
                        ops::API_V1_WRITE_STDIN,
                        &root,
                        &caller_id,
                        json!({
                            "command_session_id": &session_id,
                            "chars": format!("payload-{level}-{index}\n"),
                            "yield_time_ms": 1500,
                            "max_output_tokens": 2000
                        }),
                    )?;
                    ensure!(
                        !output_contains(&answered, &prompt_needle),
                        "parallel prompt stdin cursor must not replay prompt output: {answered}"
                    );
                    let reply = if output_contains(&answered, &reply_needle) {
                        answered
                    } else {
                        poll_stdin_cursor_until_contains(
                            &client,
                            &root,
                            &caller_id,
                            &session_id,
                            &reply_needle,
                            &prompt_needle,
                            Instant::now() + Duration::from_secs(timeout_s.min(15)),
                        )?
                    };
                    ensure!(
                        output_contains(&reply, &reply_needle),
                        "parallel prompt worker should echo its own payload: {reply}"
                    );

                    let quiet = request_with_identity(
                        &client,
                        ops::API_V1_WRITE_STDIN,
                        &root,
                        &caller_id,
                        json!({
                            "command_session_id": &session_id,
                            "chars": "",
                            "yield_time_ms": 250,
                            "max_output_tokens": 2000
                        }),
                    )?;
                    ensure!(
                        !output_contains(&quiet, &format!("reply:{marker}:payload")),
                        "parallel prompt empty poll must not replay consumed output: {quiet}"
                    );

                    let cancel = request_with_identity(
                        &client,
                        ops::API_V1_COMMAND_CANCEL,
                        &root,
                        &caller_id,
                        json!({"command_session_id": &session_id, "max_output_tokens": 2000}),
                    )?;
                    ensure_terminalish_status(&cancel)?;
                    Ok(())
                })
            })
            .collect();

        for handle in handles {
            handle
                .join()
                .map_err(|_| anyhow!("parallel prompt worker panicked"))??;
        }
        wait_for_session_count(&lease, 0)?;
        let metrics = wait_for_active_leases(&lease, 0)?;
        ensure!(
            as_i64(&metrics, "active_leases")? == 0,
            "parallel prompt level {level} should not leak leases: {metrics}"
        );
    }
    Ok(())
}

fn poll_stdin_cursor_until_contains(
    client: &eos_e2e_test::client::ProtocolClient,
    root: &str,
    caller_id: &str,
    session_id: &str,
    needle: &str,
    forbidden_replay: &str,
    deadline: Instant,
) -> Result<Value> {
    let mut last = None;
    while Instant::now() < deadline {
        let poll = request_with_identity(
            client,
            ops::API_V1_WRITE_STDIN,
            root,
            caller_id,
            json!({
                "command_session_id": session_id,
                "chars": "",
                "yield_time_ms": 250,
                "max_output_tokens": 2000
            }),
        )?;
        ensure!(
            !output_contains(&poll, forbidden_replay),
            "stdin cursor poll must not replay prompt output: {poll}"
        );
        if output_contains(&poll, needle) {
            return Ok(poll);
        }
        last = Some(poll);
    }
    bail!("stdin cursor did not surface {needle:?} before deadline; last poll: {last:?}");
}

fn command_families(dir: &str) -> Vec<CommandFamily> {
    vec![
        CommandFamily {
            name: "builtin",
            variants: vec![
                variant("printf", "printf 'builtin-a\n'", "builtin-a", []),
                variant(
                    "sh-argv",
                    "sh -c 'printf \"builtin-b:%s\\n\" \"$0\"' matrix_arg",
                    "builtin-b:matrix_arg",
                    [],
                ),
            ],
        },
        CommandFamily {
            name: "pipeline",
            variants: vec![
                variant("sort", "printf 'b\na\n' | sort", "a\nb", []),
                variant("wc", "printf 'aa\nbb\n' | wc -l", "2", []),
            ],
        },
        CommandFamily {
            name: "redirection",
            variants: vec![
                variant(
                    "write-cat",
                    format!(
                        "mkdir -p {dir}/redir && printf 'redir-a\n' > {dir}/redir/a.txt && cat {dir}/redir/a.txt"
                    ),
                    "redir-a",
                    [format!("{dir}/redir/a.txt")],
                ),
                variant(
                    "empty-file",
                    format!(
                        "mkdir -p {dir}/redir && : > {dir}/redir/empty.txt && test -f {dir}/redir/empty.txt && printf 'empty-ok\n'"
                    ),
                    "empty-ok",
                    [format!("{dir}/redir/empty.txt")],
                ),
            ],
        },
        CommandFamily {
            name: "append",
            variants: vec![
                variant(
                    "append-tail",
                    format!(
                        "mkdir -p {dir}/append && printf 'one\n' > {dir}/append/log.txt && printf 'two\n' >> {dir}/append/log.txt && tail -n 1 {dir}/append/log.txt"
                    ),
                    "two",
                    [format!("{dir}/append/log.txt")],
                ),
                variant(
                    "append-count",
                    format!(
                        "mkdir -p {dir}/append && printf 'a\n' > {dir}/append/count.txt && printf 'b\n' >> {dir}/append/count.txt && wc -l < {dir}/append/count.txt"
                    ),
                    "2",
                    [format!("{dir}/append/count.txt")],
                ),
            ],
        },
        CommandFamily {
            name: "heredoc",
            variants: vec![
                variant("stdout-doc", "cat <<'EOS'\nhere-a\nEOS", "here-a", []),
                variant(
                    "file-doc",
                    format!(
                        "mkdir -p {dir}/here && cat > {dir}/here/doc.txt <<'EOS'\nhere-file\nEOS\ncat {dir}/here/doc.txt"
                    ),
                    "here-file",
                    [format!("{dir}/here/doc.txt")],
                ),
            ],
        },
        CommandFamily {
            name: "filesystem",
            variants: vec![
                variant(
                    "find",
                    format!(
                        "mkdir -p {dir}/fs/nested && touch {dir}/fs/nested/a {dir}/fs/b && find {dir}/fs -type f | sort"
                    ),
                    format!("{dir}/fs/nested/a"),
                    [format!("{dir}/fs/nested/a"), format!("{dir}/fs/b")],
                ),
                variant(
                    "symlink",
                    format!(
                        "mkdir -p {dir}/fs && ln -sf target {dir}/fs/link && readlink {dir}/fs/link"
                    ),
                    "target",
                    [format!("{dir}/fs/link")],
                ),
            ],
        },
        CommandFamily {
            name: "grep",
            variants: vec![
                variant("pipe-grep", "printf 'alpha\nbeta\n' | grep '^beta$'", "beta", []),
                variant(
                    "file-grep",
                    format!(
                        "mkdir -p {dir}/grep && printf 'red\nblue\n' > {dir}/grep/colors.txt && grep blue {dir}/grep/colors.txt"
                    ),
                    "blue",
                    [format!("{dir}/grep/colors.txt")],
                ),
            ],
        },
        CommandFamily {
            name: "sed",
            variants: vec![
                variant("replace", "printf 'red\n' | sed 's/red/blue/'", "blue", []),
                variant(
                    "extract",
                    "printf 'prefix:42\n' | sed -n 's/^prefix://p'",
                    "42",
                    [],
                ),
            ],
        },
        CommandFamily {
            name: "awk",
            variants: vec![
                variant(
                    "sum",
                    "printf '1 2\n3 4\n' | awk '{s+=$1+$2} END {print s}'",
                    "10",
                    [],
                ),
                variant(
                    "split",
                    "printf 'a,b\n' | awk -F, '{print $2 \":\" $1}'",
                    "b:a",
                    [],
                ),
            ],
        },
        CommandFamily {
            name: "python",
            variants: vec![
                variant("stdout", "python3 - <<'PY'\nprint('py-a')\nPY", "py-a", []),
                variant(
                    "file-write",
                    format!(
                        "python3 - <<'PY'\nfrom pathlib import Path\npath = Path({:?})\npath.parent.mkdir(parents=True, exist_ok=True)\npath.write_text('py-file\\n')\nprint('py-b')\nPY",
                        format!("{dir}/python/out.txt")
                    ),
                    "py-b",
                    [format!("{dir}/python/out.txt")],
                ),
            ],
        },
        CommandFamily {
            name: "stderr",
            variants: vec![
                variant(
                    "stderr-stdout",
                    "sh -c 'printf \"stderr-ok\\n\" >&2; printf \"stdout-ok\\n\"'",
                    "stdout-ok",
                    [],
                ),
                variant(
                    "stderr-pipe",
                    "sh -c 'printf \"pipe-ok\\n\"; printf \"noise\\n\" >&2' | grep pipe",
                    "pipe-ok",
                    [],
                ),
            ],
        },
        CommandFamily {
            name: "json-and-bytes",
            variants: vec![
                variant(
                    "json",
                    "python3 - <<'PY'\nimport json\nprint(json.dumps({'n': 3, 'ok': True}, sort_keys=True))\nPY",
                    "\"ok\": true",
                    [],
                ),
                variant("byte-count", "printf 'abc' | wc -c", "3", []),
            ],
        },
    ]
}

fn variant(
    name: &'static str,
    cmd: impl Into<String>,
    stdout_contains: impl Into<String>,
    changed_paths: impl Into<Vec<String>>,
) -> CommandVariant {
    CommandVariant {
        name,
        cmd: cmd.into(),
        stdout_contains: stdout_contains.into(),
        changed_paths: changed_paths.into(),
    }
}

fn parallel_command(dir: &str, level: usize, index: usize) -> String {
    let marker = format!("worker:{level}:{index}");
    let path = format!("{dir}/worker-{index}/result.txt");
    match index % 10 {
        0 => format!("mkdir -p {dir}/worker-{index} && printf '{marker}\n' > {path} && cat {path}"),
        1 => format!("mkdir -p {dir}/worker-{index} && printf '{marker}\n' | tee {path}"),
        2 => format!(
            "mkdir -p {dir}/worker-{index} && printf 'z\n{marker}\n' | sort | grep worker > {path} && cat {path}"
        ),
        3 => format!(
            "mkdir -p {dir}/worker-{index} && python3 - <<'PY'\nfrom pathlib import Path\nPath({path:?}).write_text({marker:?} + '\\n')\nprint({marker:?})\nPY"
        ),
        4 => format!(
            "mkdir -p {dir}/worker-{index} && printf '{level} {index}\n' | awk '{{print \"{marker}\"}}' > {path} && cat {path}"
        ),
        5 => format!(
            "mkdir -p {dir}/worker-{index} && printf 'raw:{marker}\n' | sed 's/^raw://' > {path} && cat {path}"
        ),
        6 => format!(
            "mkdir -p {dir}/worker-{index} && printf '{marker}\n' > {path} && grep 'worker' {path}"
        ),
        7 => format!(
            "mkdir -p {dir}/worker-{index} && cat > {path} <<'EOS'\n{marker}\nEOS\ncat {path}"
        ),
        8 => format!(
            "mkdir -p {dir}/worker-{index} && printf '{marker}\n' > {path} && test -s {path} && cat {path}"
        ),
        _ => format!(
            "mkdir -p {dir}/worker-{index} && sh -c 'printf \"$1\\n\" > \"$2\" && cat \"$2\"' sh {marker:?} {path:?}"
        ),
    }
}

fn request_with_identity(
    client: &eos_e2e_test::client::ProtocolClient,
    op: &str,
    root: &str,
    caller_id: &str,
    args: Value,
) -> Result<Value> {
    let mut args = args
        .as_object()
        .cloned()
        .with_context(|| format!("request args should be an object: {args}"))?;
    args.entry("layer_stack_root".to_owned())
        .or_insert_with(|| json!(root));
    args.entry("caller_id".to_owned())
        .or_insert_with(|| json!(caller_id));
    client.request(op, &next_invocation_id(), &Value::Object(args))
}

fn assert_command_ok(response: &Value, family: &str, variant: &str) -> Result<()> {
    ensure!(
        as_str(response, "status")? == "ok",
        "{family}:{variant} should complete foreground: {response}"
    );
    ensure!(
        as_i64(response, "exit_code")? == 0,
        "{family}:{variant} should exit 0: {response}"
    );
    ensure!(
        response.get("command_session_id").is_none(),
        "{family}:{variant} should not leave a command session handle: {response}"
    );
    Ok(())
}

fn assert_changed_paths(response: &Value, expected: &[String]) -> Result<()> {
    let changed = array(response, "changed_paths")?;
    if expected.is_empty() {
        ensure!(
            changed.is_empty(),
            "read-only command should not publish changed paths: {response}"
        );
        return Ok(());
    }
    for expected_path in expected {
        ensure!(
            changed
                .iter()
                .any(|path| path.as_str() == Some(expected_path.as_str())),
            "command should publish {expected_path}: {response}"
        );
    }
    Ok(())
}

fn assert_bounded_command_resources(
    response: &Value,
    elapsed: Duration,
    timeout_s: u64,
) -> Result<()> {
    ensure!(
        elapsed < Duration::from_secs(timeout_s + 10),
        "command exceeded bounded wall time {elapsed:?}: {response}"
    );
    if let Some(upperdir) = timing(response, "resource.command_exec.upperdir_tree_bytes") {
        ensure!(
            upperdir < 2_000_000.0,
            "command upperdir should stay delta-sized (<2MB), got {upperdir}: {response}"
        );
    }
    if let Some(run_dir) = timing(response, "resource.command_exec.run_dir_tree_bytes") {
        ensure!(
            run_dir < 4_000_000.0,
            "command run dir should stay bounded (<4MB), got {run_dir}: {response}"
        );
    }
    if let Some(memory_current) = timing(response, "resource.cgroup.memory_current_bytes") {
        ensure!(
            memory_current > 0.0 && memory_current < 64e9,
            "cgroup memory.current should be sane, got {memory_current}: {response}"
        );
    }
    if let Some(rss) = timing(response, "resource.process.rss_bytes") {
        ensure!(
            rss > 0.0 && rss < 64e9,
            "process RSS should be sane, got {rss}: {response}"
        );
    }
    Ok(())
}

fn ensure_terminalish_status(response: &Value) -> Result<()> {
    let status = as_str(response, "status")?;
    if matches!(status, "cancelled" | "ok" | "error") {
        return Ok(());
    }
    bail!("cancel should return a terminal-ish status: {response}");
}

fn output_contains(response: &Value, needle: &str) -> bool {
    stdout(response).replace("\r\n", "\n").contains(needle)
}

fn timing(response: &Value, key: &str) -> Option<f64> {
    response
        .get("timings")
        .and_then(Value::as_object)
        .and_then(|timings| timings.get(key))
        .and_then(Value::as_f64)
}

fn workload_timeout_s(pool: &NodePool) -> u64 {
    pool.workload().timeout.as_secs().max(10)
}
