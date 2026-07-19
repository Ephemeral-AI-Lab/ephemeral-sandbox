//! Pure cgroup v2 accounting reads with no runtime or daemon dependency.

use std::path::Path;

/// A cgroup v2 accounting reading, or an unavailable marker carrying the path and
/// the first failure reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CgroupSample {
    pub cgroup_path: Option<String>,
    pub cgroup_available: bool,
    pub cgroup_error: Option<String>,
    pub cpu_usage_usec: Option<i64>,
    pub memory_current_bytes: Option<i64>,
    pub memory_max_bytes: Option<i64>,
    pub memory_max_unlimited: Option<bool>,
    pub io_read_bytes: Option<i64>,
    pub io_write_bytes: Option<i64>,
    pub pids_current: Option<i64>,
}

impl CgroupSample {
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            cgroup_available: false,
            cgroup_error: Some(message.into()),
            ..Self::default()
        }
    }

    /// Sample cgroup v2 accounting from `cgroup_dir`'s controller files.
    /// Best-effort: each controller file is independent. Unsupported values
    /// remain absent instead of being synthesized as zero.
    #[must_use]
    pub fn read(cgroup_dir: &Path) -> Self {
        let path_text = cgroup_dir.to_string_lossy().into_owned();
        let mut errors = Vec::new();
        let cpu_usage_usec = capture(&mut errors, read_cpu_usage_usec(cgroup_dir));
        let memory_current_bytes = capture(
            &mut errors,
            read_u64_file(&cgroup_dir.join("memory.current")),
        );
        let memory_max = capture(&mut errors, read_memory_max(cgroup_dir));
        let io = capture(&mut errors, read_io_bytes(cgroup_dir));
        let pids_current = capture(&mut errors, read_u64_file(&cgroup_dir.join("pids.current")));
        let (memory_max_bytes, memory_max_unlimited) = memory_max
            .map(|(value, unlimited)| (value, Some(unlimited)))
            .unwrap_or((None, None));
        let (io_read_bytes, io_write_bytes) = io.unwrap_or((None, None));
        let cgroup_available = cpu_usage_usec.is_some()
            || memory_current_bytes.is_some()
            || memory_max_unlimited.is_some()
            || io_read_bytes.is_some()
            || io_write_bytes.is_some()
            || pids_current.is_some();
        Self {
            cgroup_path: Some(path_text),
            cgroup_available,
            cgroup_error: (!errors.is_empty()).then(|| errors.join("; ")),
            cpu_usage_usec,
            memory_current_bytes,
            memory_max_bytes,
            memory_max_unlimited,
            io_read_bytes,
            io_write_bytes,
            pids_current,
        }
    }
}

fn capture<T>(errors: &mut Vec<String>, result: Result<T, String>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            errors.push(error);
            None
        }
    }
}

fn read_cpu_usage_usec(cgroup_dir: &Path) -> Result<i64, String> {
    let path = cgroup_dir.join("cpu.stat");
    let contents = read_file(&path)?;
    contents
        .lines()
        .find_map(|line| line.strip_prefix("usage_usec "))
        .ok_or_else(|| format!("usage_usec missing in {}", path.display()))
        .and_then(|value| parse_i64(value.trim(), &path))
}

fn read_memory_max(cgroup_dir: &Path) -> Result<(Option<i64>, bool), String> {
    let path = cgroup_dir.join("memory.max");
    let contents = read_file(&path)?;
    let trimmed = contents.trim();
    if trimmed == "max" {
        Ok((None, true))
    } else {
        Ok((Some(parse_i64(trimmed, &path)?), false))
    }
}

fn read_io_bytes(cgroup_dir: &Path) -> Result<(Option<i64>, Option<i64>), String> {
    let path = cgroup_dir.join("io.stat");
    let contents = read_file(&path)?;
    let mut read = None::<i64>;
    let mut write = None::<i64>;
    for field in contents
        .lines()
        .flat_map(|line| line.split_whitespace().skip(1))
    {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };
        let target = match name {
            "rbytes" => &mut read,
            "wbytes" => &mut write,
            _ => continue,
        };
        let parsed = parse_i64(value, &path)?;
        *target = Some(target.unwrap_or(0).saturating_add(parsed));
    }
    Ok((read, write))
}

fn read_u64_file(path: &Path) -> Result<i64, String> {
    parse_i64(read_file(path)?.trim(), path)
}

fn read_file(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn parse_i64(value: &str, path: &Path) -> Result<i64, String> {
    value
        .parse::<i64>()
        .map_err(|error| format!("{}: {error}", path.display()))
}
