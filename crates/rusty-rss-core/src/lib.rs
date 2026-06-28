//! Core library for `rusty-rss`: an agent-first RSS reader and content pipeline.
//!
//! This crate provides the building blocks shared by the CLI and MCP server:
//!
//! - [`config`]: runtime configuration loading and secret redaction.
//! - [`db`]: SQLite persistence for feeds, posts, captures, enrichment, tags,
//!   full-text search, and schema migrations.
//! - [`fetch`] / [`parse`] / [`sync`]: fetching feed HTTP responses, parsing
//!   them into [`models`] types, and synchronizing posts into the database.
//! - [`capture`]: fetching and storing the full article body for a post.
//! - [`enrich`] / [`llm`]: LLM-backed summarization, scoring, and classification
//!   of captured content.
//! - [`rules`] / [`tag`] / [`sort`]: declarative rule evaluation, automatic
//!   tagging, and post ordering.
//!
//! Most callers interact with a single SQLite connection plus a [`config::Config`]
//! loaded from the environment.
#![warn(missing_docs)]

pub mod capture;
pub mod config;
pub mod db;
pub mod enrich;
pub mod fetch;
pub mod llm;
pub mod models;
pub mod parse;
pub mod rules;
pub mod sort;
pub mod sync;
pub mod tag;

#[cfg(test)]
pub(crate) mod test_support;
