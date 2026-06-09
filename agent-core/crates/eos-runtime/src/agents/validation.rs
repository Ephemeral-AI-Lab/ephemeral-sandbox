//! The *pure* fragments of profile validation that need no other crate: the
//! skill-file terminal-silence scanner (`skills/loader.py`).
//!
//! The cyclic edge in the Rust source is broken by injection (GC-eos-agent-def-05):
//! terminal keys are passed into the scanner as data rather than imported from
//! `eos-tool`.

/// Terminal-silence lint over a skill body (`agents/skills/loader.py`).
pub mod skill_lint {
    /// Scan a skill body for terminal-tool mentions, returning one
    /// human-readable violation per hit (empty means clean).
    ///
    /// `terminal_keys` are injected as data (the `TERMINAL_DESCRIPTORS` keys are
    /// owned by `eos-tool`, GC-eos-agent-def-05). The caller passes an already
    /// frontmatter-stripped `body` so author metadata cannot false-positive.
    #[must_use]
    pub fn scan_skill_file(body: &str, terminal_keys: &[&str]) -> Vec<String> {
        let submit_hits = find_submit_tokens(body);
        let mut violations: Vec<String> = submit_hits
            .iter()
            .map(|hit| {
                format!(
                    "skill body mentions terminal-tool name {hit:?}; row 4 must be \
                     terminal-silent (row 3 owns the catalog)"
                )
            })
            .collect();

        // Catch terminal keys that escape the `submit_*` pattern.
        for key in terminal_keys {
            if submit_hits.iter().any(|hit| hit == key) {
                continue;
            }
            if body.contains(*key) {
                violations.push(format!(
                    "skill body mentions terminal descriptor key {key:?}; row 4 must be \
                     terminal-silent (row 3 owns the catalog)"
                ));
            }
        }
        violations
    }

    /// Sorted, de-duplicated `submit_<identifier>` tokens (port of the
    /// `submit_[A-Za-z0-9_]+` regex without a regex dependency).
    fn find_submit_tokens(body: &str) -> Vec<String> {
        const PREFIX: &str = "submit_";
        let bytes = body.as_bytes();
        let mut tokens = Vec::new();
        let mut cursor = 0;
        while let Some(rel) = body[cursor..].find(PREFIX) {
            let start = cursor + rel;
            let extend_from = start + PREFIX.len();
            let mut end = extend_from;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            // The regex `+` requires at least one identifier char after the
            // underscore; a bare `submit_` followed by a non-word char is no match.
            if end > extend_from {
                tokens.push(body[start..end].to_owned());
            }
            cursor = end.max(extend_from);
        }
        tokens.sort();
        tokens.dedup();
        tokens
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // unwrap is permitted in tests (err-no-unwrap-prod)
    use super::skill_lint::scan_skill_file;

    // AC-eos-agent-def-08: submit_* tokens and injected keys are flagged; a
    // terminal-silent body returns empty.
    #[test]
    fn skill_lint_detects_terminals() {
        let keys = ["submit_planner_outcome", "finish_run"];

        let submit = scan_skill_file("call submit_planner_outcome when done", &keys);
        assert_eq!(submit.len(), 1, "{submit:?}");
        assert!(submit[0].contains("submit_planner_outcome"));

        let key_only = scan_skill_file("then perform the finish_run step", &keys);
        assert_eq!(key_only.len(), 1, "{key_only:?}");
        assert!(key_only[0].contains("finish_run"));

        let clean = scan_skill_file("reach the decision point and submit once", &keys);
        assert!(clean.is_empty(), "{clean:?}");
    }

    #[test]
    fn skill_lint_dedupes_and_ignores_bare_prefix() {
        // Repeated token reported once; bare `submit_` (no trailing word char) ignored.
        let hits = scan_skill_file("submit_x then submit_x and a bare submit_ here", &[]);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(hits[0].contains("submit_x"));
    }
}
