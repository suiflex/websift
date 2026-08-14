//! Core library for Websift.
//!
//! Bounded web retrieval behind an MCP boundary: search, research, mapping, scraping, and
//! crawling, each gated by the shared robots and public-destination policy in [`policy`] and
//! persisted per profile by [`storage`].

pub mod adapters;
pub mod application;
pub mod config;
pub mod crawl;
pub mod fetch;
pub mod observe;
pub mod policy;
pub mod research;
pub mod robots;
pub mod storage;
#[cfg(test)]
pub(crate) mod testing;
pub mod update;
pub mod worker;

/// Worker protocol implemented by both the Rust core and TypeScript worker.
pub const WORKER_PROTOCOL_VERSION: u8 = 1;
