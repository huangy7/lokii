pub mod cache;
pub mod config;
pub mod filter;
pub mod indexer;
pub mod metrics;
pub mod path_utils;
pub mod permissions;
pub mod scanner;
pub mod search;
pub mod startup;
pub mod watcher;

mod ffi;

uniffi::setup_scaffolding!();
