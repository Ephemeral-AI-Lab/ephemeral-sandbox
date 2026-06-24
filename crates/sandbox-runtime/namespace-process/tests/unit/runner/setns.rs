use crate::runner::protocol::{Fd, NsFds};

#[cfg(target_os = "linux")]
#[test]
fn namespace_order_is_user_mnt_pid_net_and_skips_missing_fds() {
    let ns_fds = NsFds {
        user: Some(Fd(10)),
        mnt: Some(Fd(11)),
        pid: None,
        net: Some(Fd(12)),
    };
    let order: Vec<(&str, i32)> = super::namespace_fd_order_with_types(&ns_fds)
        .into_iter()
        .map(|(name, fd, _nstype)| (name, fd))
        .collect();
    assert_eq!(order, vec![("user", 10), ("mnt", 11), ("net", 12)]);
}
