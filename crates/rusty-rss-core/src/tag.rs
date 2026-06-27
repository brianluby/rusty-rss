//! Gate 1 evaluator: the cheap, deterministic, re-runnable tagger.
//!
//! For each topic, every rule is compiled to column-scoped FTS5 operands
//! ([`crate::rules`]); we run each operand once over `posts_fts` to get its
//! matching-rowid set, then do all scoring, `min_hits` counting, subreddit
//! priors, vetoes and thresholds in Rust over those sets. Results are
//! materialized into `post_tags` with full provenance.
//!
//! See `docs/prd/rule-engine.md` for the model and worked examples.
//!
//! This root keeps no logic of its own; it groups the tagger by concern —
//! public [`types`], FTS [`score`]-ing helpers, and the [`run`] orchestration —
//! and re-exports the public API so callers continue to use `tag::*`.

mod run;
mod score;
mod types;

#[cfg(test)]
mod test_support;

pub use run::run_tagging_batch;
pub use types::{TagOptions, TagSummary};
