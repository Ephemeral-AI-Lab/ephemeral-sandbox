//! Runtime-owned agent profile loading and validation.
//!
//! Passive agent DTOs and the read-only registry live in `eos-types`; this
//! module owns the filesystem loader and loader-local validation while
//! `eos-runtime` remains the staging facade before the `eos-agent-core` fold.

mod error;
mod loader;
mod model;
#[cfg(test)]
mod validation;

pub(crate) use loader::load_agents_tree;
