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
    /// Best-effort: any missing/unreadable required file degrades to an
    /// unavailable sample carrying the path and the first failure reason.
    #[must_use]
    pub fn read(cgroup_dir: &Path) -> Self {
        let path_text = cgroup_dir.to_string_lossy().into_owned();
        let sample = || -> Result<Self, String> {
            let cpu_usage_usec = read_cpu_usage_usec(cgroup_dir)?;
            let memory_current_bytes = read_u64_file(&cgroup_dir.join("memory.current"))?;
            let (memory_max_bytes, memory_max_unlimited) = read_memory_max(cgroup_dir)?;
            Ok(Self {
                cgroup_path: Some(path_text.clone()),
                cgroup_available: true,
                cgroup_error: None,
                cpu_usage_usec: Some(cpu_usage_usec),
                memory_current_bytes: Some(memory_current_bytes),
                memory_max_bytes,
                memory_max_unlimited: Some(memory_max_unlimited),
            })
        };
        match sample() {
            Ok(sample) => sample,
            Err(error) => Self {
                cgroup_path: Some(path_text),
                ..Self::unavailable(error)
            },
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
