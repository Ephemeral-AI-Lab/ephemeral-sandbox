//! Process policy for anonymous transparent huge pages.

#[cfg(target_os = "linux")]
pub fn set_transparent_huge_pages_disabled(disabled: bool) -> std::io::Result<()> {
    rustix::thread::disable_transparent_huge_pages(disabled)?;
    let observed = transparent_huge_pages_disabled()?;
    if observed == disabled {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "kernel did not apply the requested transparent-huge-page policy",
        ))
    }
}

#[cfg(target_os = "linux")]
pub fn transparent_huge_pages_disabled() -> std::io::Result<bool> {
    Ok(rustix::thread::transparent_huge_pages_are_disabled()?)
}

#[cfg(target_os = "linux")]
pub fn prepare_server_policy() {
    use std::os::unix::process::CommandExt;

    match transparent_huge_pages_disabled() {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("sandbox-daemon: failed to read transparent huge page policy: {error}");
            return;
        }
    }

    if let Err(error) = set_transparent_huge_pages_disabled(true) {
        eprintln!("sandbox-daemon: failed to disable transparent huge pages: {error}");
        return;
    }

    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => {
            eprintln!("sandbox-daemon: failed to locate executable for THP-safe restart: {error}");
            return;
        }
    };
    let error = std::process::Command::new(executable)
        .args(std::env::args_os().skip(1))
        .exec();
    eprintln!("sandbox-daemon: failed to restart with transparent huge pages disabled: {error}");
}

#[cfg(not(target_os = "linux"))]
pub fn set_transparent_huge_pages_disabled(_disabled: bool) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn prepare_server_policy() {}
