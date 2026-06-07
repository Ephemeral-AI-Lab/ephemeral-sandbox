//! Plugin op-name helpers.
//!
//! Builds the public op name `plugin.<plugin>.<op>` the daemon dispatcher
//! registers, and validates plugin identifiers.

/// Build the public op name the daemon dispatcher registers: `plugin.<plugin>.<op>`.
#[must_use]
pub fn public_op_name(plugin_name: &str, op_name: &str) -> String {
    format!("plugin.{plugin_name}.{op_name}")
}

/// Whether `name` matches the Rust `_PLUGIN_NAME_RE` (`^[A-Za-z_][A-Za-z0-9_]*$`).
pub(crate) fn is_valid_plugin_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_op_name_format() {
        assert_eq!(public_op_name("generic", "hover"), "plugin.generic.hover");
    }

    #[test]
    fn plugin_name_validation() {
        assert!(is_valid_plugin_name("generic"));
        assert!(is_valid_plugin_name("_x9"));
        assert!(!is_valid_plugin_name("9plugin"));
        assert!(!is_valid_plugin_name(""));
        assert!(!is_valid_plugin_name("ls-p"));
    }
}
