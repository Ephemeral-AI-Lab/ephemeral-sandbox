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

#[cfg(not(target_os = "linux"))]
pub fn set_transparent_huge_pages_disabled(_disabled: bool) -> std::io::Result<()> {
    Ok(())
}
