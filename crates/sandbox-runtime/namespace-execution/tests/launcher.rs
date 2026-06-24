pub mod error {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/error.rs"));
}

#[allow(dead_code)]
pub mod pty {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/pty.rs"));
}

#[allow(dead_code)]
pub mod launcher {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/launcher.rs"));

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn piped_completion_timeout_terminates_and_reaps_child() {
            let (result_read, result_write) = result_pipe().expect("result pipe");
            drop(result_write);
            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg("trap 'exit 0' TERM; while true; do sleep 1; done")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            install_pgid_leader_hook(&mut command);
            let child = command.spawn().expect("spawn child");
            let mut runner = ForkRunnerChild {
                child,
                result_read,
                mode_flag: Some("--mount-overlay"),
                setup_timeout_s: 0.01,
            };

            let error = runner.wait_completion().expect_err("timeout");

            assert!(error
                .to_string()
                .contains("ns-runner --mount-overlay timed out"));
            assert!(runner.child.try_wait().expect("child state").is_some());
        }

        #[test]
        fn zero_status_without_valid_result_is_completion_error() {
            let (result_read, result_write) = result_pipe().expect("result pipe");
            drop(result_write);
            let child = Command::new("sh")
                .arg("-c")
                .arg("true")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn child");
            let mut runner = ForkRunnerChild {
                child,
                result_read,
                mode_flag: None,
                setup_timeout_s: 0.0,
            };

            let error = runner
                .wait_completion()
                .expect_err("missing success result is an execution error");

            assert!(matches!(
                error,
                crate::error::NamespaceExecutionError::Completion(_)
            ));
        }

        #[test]
        fn nonzero_status_without_valid_result_synthesizes_failure() {
            let (result_read, result_write) = result_pipe().expect("result pipe");
            drop(result_write);
            let child = Command::new("sh")
                .arg("-c")
                .arg("exit 17")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn child");
            let mut runner = ForkRunnerChild {
                child,
                result_read,
                mode_flag: None,
                setup_timeout_s: 0.0,
            };

            let result = runner
                .wait_completion()
                .expect("nonzero child status yields synthesized result");

            assert_eq!(result.exit_code, 17);
            assert_eq!(result.payload["status"], "error");
        }
    }
}
