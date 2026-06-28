//! Read-only Model Context Protocol (MCP) server for the local rusty-rss archive.
//!
//! The crate ships both a library (this module tree) and a thin `main` binary.
//! The library exposes [`config`] for startup wiring and [`server`] for the
//! `rmcp`-based [`RustyRssServer`], which integration tests drive in-process over
//! an `rmcp` duplex transport.

pub mod config;
pub mod server;

pub use server::RustyRssServer;
