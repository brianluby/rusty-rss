//! Gate 1 rule engine configuration: rules-as-data.
//!
//! `rules.toml` is parsed into a typed [`RuleSet`], validated fail-fast, and
//! compiled into column-scoped FTS5 expressions that the evaluator runs against
//! the `posts_fts` index. The config is the source of truth; the evaluator and
//! FTS5 are swappable around it.
//!
//! Division of labor (see `docs/prd/rule-engine.md`):
//! - FTS5 matches keywords (binary hit per compiled operand, stemmed).
//! - The evaluator does additive scoring, subreddit priors, vetoes, thresholds.
//! - This module holds the rules and turns them into safe FTS expressions.
//!
//! Organized by layer: the [`types`] config model, the FTS-operand [`parse`]r,
//! and [`compile`] (validation + lowering). This root only re-exports the public
//! API so callers continue to use `rules::*`.

mod compile;
mod parse;
mod types;

#[cfg(test)]
mod test_support;

pub use compile::{CompiledExclude, CompiledRule, CompiledRuleSet, CompiledTopic};
pub use types::{ExcludeRule, Guard, Kind, MatchSpec, Meta, Rule, RuleSet, Signal, Topic};
