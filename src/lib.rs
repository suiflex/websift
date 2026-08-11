//! Core library for Websift.
//!
//! The MCP boundary is runnable; retrieval behavior remains intentionally absent.

pub mod adapters;
pub mod application;
pub mod config;
pub mod crawl;
pub mod fetch;
pub mod policy;
pub mod storage;
pub mod update;
pub mod worker;

/// Worker protocol implemented by both the Rust core and TypeScript worker.
pub const WORKER_PROTOCOL_VERSION: u8 = 1;
