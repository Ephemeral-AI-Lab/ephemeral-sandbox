#![forbid(unsafe_code)]

#[path = "../src/config.rs"]
mod config;
#[path = "../src/contract.rs"]
#[allow(dead_code)]
mod contract;

pub use config::CommandConfig;
