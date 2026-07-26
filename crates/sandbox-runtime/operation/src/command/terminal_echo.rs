pub(crate) fn max_terminal_echo_bytes(stdin: &str) -> u64 {
    u64::try_from(stdin.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(2)
}
