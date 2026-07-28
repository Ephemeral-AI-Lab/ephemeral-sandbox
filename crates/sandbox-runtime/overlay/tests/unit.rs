#![forbid(unsafe_code)]

#[path = "../src/lib.rs"]
mod overlay;

pub use overlay::*;

#[cfg(target_os = "linux")]
pub(crate) use overlay::kernel_mount::{
    legacy_lowerdir_value, overlay_mount_attributes, ValidatedMountInputs,
};

#[cfg(target_os = "linux")]
#[path = "unit/kernel_mount.rs"]
mod kernel_mount_tests;

#[path = "unit/writable_dirs.rs"]
mod writable_dirs_tests;
