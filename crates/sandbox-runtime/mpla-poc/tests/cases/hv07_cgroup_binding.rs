use std::path::Path;

pub(crate) fn validate_hv07_storage_cgroup_binding<'a>(
    cgroup_procs_path: Option<&'a Path>,
    storage_cgroup_dir: Option<&'a Path>,
) -> Result<(&'a Path, &'a Path), &'static str> {
    let storage_cgroup_dir =
        storage_cgroup_dir.ok_or("HV-07 physical session/activation has no storage cgroup")?;
    let cgroup_procs_path = cgroup_procs_path
        .ok_or("HV-07 physical session/activation has no workload cgroup membership")?;
    if cgroup_procs_path != storage_cgroup_dir.join("cgroup.procs") {
        return Err("HV-07 storage cgroup and workload membership disagree");
    }
    Ok((cgroup_procs_path, storage_cgroup_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_accepts_only_exact_storage_cgroup_membership_pair() {
        let storage = Path::new("/sys/fs/cgroup/eos-workload");
        let membership = storage.join("cgroup.procs");
        let exact = validate_hv07_storage_cgroup_binding(Some(&membership), Some(storage))
            .expect("accept exact cgroup binding");
        assert_eq!(exact, (membership.as_path(), storage));

        let missing_storage = validate_hv07_storage_cgroup_binding(Some(&membership), None)
            .expect_err("reject missing storage cgroup");
        assert!(missing_storage.contains("no storage cgroup"));

        let missing_membership = validate_hv07_storage_cgroup_binding(None, Some(storage))
            .expect_err("reject missing workload membership");
        assert!(missing_membership.contains("no workload cgroup membership"));

        let conflicting = validate_hv07_storage_cgroup_binding(
            Some(Path::new("/sys/fs/cgroup/other/cgroup.procs")),
            Some(storage),
        )
        .expect_err("reject conflicting workload membership");
        assert!(conflicting.contains("membership disagree"));
    }
}
