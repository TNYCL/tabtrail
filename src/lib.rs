//! Tabtrail: discovery-based dynamic completion for existing shells.

pub mod app;
pub mod cache;
pub mod completion;
pub mod config;
pub mod context;
pub mod discovery;
pub mod fsutil;
pub mod help;
pub mod install;
pub mod man;
pub mod probe;
pub mod providers;
pub mod render;
pub mod resolve;
pub mod sanitize;
pub mod schema;
pub mod shellword;

#[cfg(test)]
pub(crate) mod testutil;
