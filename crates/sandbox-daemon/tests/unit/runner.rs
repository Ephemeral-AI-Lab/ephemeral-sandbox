use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use anyhow::{bail, Context, Result};

use crate::runner_cli::{mount_overlay::mount_overlay_result, open_fd_for_write, RunnerCliConfig};

#[test]
fn runner_cli_accepts_explicit_mode_and_request_result_fds() -> Result<()> {
    let _config = RunnerCliConfig::parse(vec![
        "--shell".to_owned(),
        "--request-fd".to_owned(),
        "3".to_owned(),
        "--result-fd".to_owned(),
        "4".to_owned(),
    ])?;

    Ok(())
}

#[test]
fn runner_cli_requires_exactly_one_mode() -> Result<()> {
    let missing = match RunnerCliConfig::parse(vec![
        "--request-fd".to_owned(),
        "3".to_owned(),
        "--result-fd".to_owned(),
        "4".to_owned(),
    ]) {
        Ok(_) => bail!("no mode flag unexpectedly accepted"),
        Err(error) => error,
    };
    assert!(
        missing.to_string().contains("exactly one mode flag"),
        "{missing}"
    );

    let duplicate = match RunnerCliConfig::parse(vec![
        "--shell".to_owned(),
        "--file-op".to_owned(),
        "--request-fd".to_owned(),
        "3".to_owned(),
        "--result-fd".to_owned(),
        "4".to_owned(),
    ]) {
        Ok(_) => bail!("two mode flags unexpectedly accepted"),
        Err(error) => error,
    };
    assert!(
        duplicate.to_string().contains("only one mode flag"),
        "{duplicate}"
    );
    Ok(())
}

#[test]
fn runner_cli_rejects_missing_result_fd() -> Result<()> {
    let error = match RunnerCliConfig::parse(vec!["--request-fd".to_owned(), "3".to_owned()]) {
        Ok(_) => bail!("missing output path unexpectedly accepted"),
        Err(error) => error,
    };

    assert!(
        error.to_string().contains("requires --result-fd FD"),
        "{error}"
    );
    Ok(())
}

#[test]
fn runner_cli_rejects_positional_request_fd() -> Result<()> {
    let error = match RunnerCliConfig::parse(vec!["3".to_owned()]) {
        Ok(_) => bail!("positional request fd unexpectedly accepted"),
        Err(error) => error,
    };

    assert!(
        error
            .to_string()
            .contains("unexpected ns-runner positional argument"),
        "{error}"
    );
    Ok(())
}

#[test]
fn result_fd_writer_writes_to_fd_peer() -> Result<()> {
    let (mut read_end, write_end) = UnixStream::pair().context("create result pair")?;
    let mut writer = open_fd_for_write(write_end.as_raw_fd()).context("open result fd")?;

    writer.write_all(b"{\"exit_code\":0}")?;
    drop(writer);
    drop(write_end);

    let mut payload = String::new();
    read_end.read_to_string(&mut payload)?;
    assert_eq!(payload, "{\"exit_code\":0}");
    Ok(())
}

#[test]
fn mount_overlay_result_maps_success_and_failure() {
    let success = mount_overlay_result(Ok::<(), &str>(()));

    assert_eq!(success.exit_code, 0);
    assert_eq!(success.payload["success"], true);
    assert_eq!(success.payload["status"], "ok");

    let failure = mount_overlay_result(Err("boom"));

    assert_eq!(failure.exit_code, 1);
    let error = failure.payload["error"]
        .as_str()
        .expect("failure payload contains error string");
    assert!(error.contains("ns-runner setns overlay mount failed"));
    assert!(error.contains("boom"));
}
