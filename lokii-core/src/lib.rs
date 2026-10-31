pub mod cache;
pub mod compact_index;
pub mod config;
pub mod filter;
pub mod indexer;
pub mod metrics;
pub mod path_utils;
pub mod permissions;
pub mod pinyin;
pub mod pinyin_data;
pub mod scanner;
pub mod search;
pub mod startup;
pub mod watcher;

mod ffi;
mod string_pool;

uniffi::setup_scaffolding!();
