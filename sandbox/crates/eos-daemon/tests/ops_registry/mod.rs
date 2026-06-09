use std::collections::BTreeSet;

use eos_protocol::ops::{BUILTIN_DAEMON_OPS, BUILTIN_DAEMON_OP_SPECS};

use super::*;

#[test]
fn builtin_registry_matches_protocol_ops() {
    let registered = BUILTIN_OPS.iter().map(BuiltinOp::wire).collect::<Vec<_>>();
    assert_eq!(registered, BUILTIN_DAEMON_OPS);
}

#[test]
fn builtin_registry_matches_protocol_catalog() {
    let registered = BUILTIN_OPS.iter().map(|op| op.spec).collect::<Vec<_>>();
    assert_eq!(registered, BUILTIN_DAEMON_OP_SPECS);
}

#[test]
fn builtin_registry_has_no_duplicate_wires() {
    let registered = BUILTIN_OPS.iter().map(BuiltinOp::wire).collect::<Vec<_>>();
    let unique = registered.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(unique.len(), registered.len());
}
