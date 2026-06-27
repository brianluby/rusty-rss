//! The typed `rules.toml` configuration model and its TOML deserialization.

use serde::Deserialize;
use std::collections::BTreeMap;

/// A parsed `rules.toml`. Topics are keyed by name (multi-label by design).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    pub meta: Meta,
    #[serde(default)]
    pub topics: BTreeMap<String, Topic>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    /// Stamped onto every `post_tags` row as `ruleset_version`.
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Topic {
    pub threshold: f32,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Subreddit -> additive weight. The only scoring path for the subreddit
    /// signal; `signal = "subreddit"` match rules are rejected in rules-v1.
    #[serde(default)]
    pub subreddit_prior: BTreeMap<String, f32>,
    #[serde(default)]
    pub exclude: Vec<ExcludeRule>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub signal: Signal,
    pub kind: Kind,
    #[serde(default = "default_weight")]
    pub weight: f32,
    #[serde(default = "default_min_hits")]
    pub min_hits: usize,
    #[serde(rename = "match")]
    pub match_spec: MatchSpec,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExcludeRule {
    pub id: String,
    pub signal: Signal,
    pub kind: Kind,
    #[serde(rename = "match")]
    pub match_spec: MatchSpec,
    /// If the guard matches, the exclude does NOT veto (keeps genuine matches).
    #[serde(default)]
    pub unless: Option<Guard>,
    #[serde(default)]
    pub veto: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Guard {
    pub signal: Signal,
    pub kind: Kind,
    #[serde(rename = "match")]
    pub match_spec: MatchSpec,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Title,
    Body,
    Subreddit,
    Domain,
    Any,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Fts,
    Terms,
    Exact,
    Regex,
}

/// `match` is either a single FTS5 expression (`kind = "fts"`) or a list of
/// literal tokens (`kind = "terms"`). Untagged so TOML authors write the
/// natural shape without a discriminant.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MatchSpec {
    Expression(String),
    Terms(Vec<String>),
}

fn default_weight() -> f32 {
    1.0
}

fn default_min_hits() -> usize {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::SEED_SNIPPET;

    #[test]
    fn parses_seed_snippet_and_round_trips_fields() {
        let ruleset = RuleSet::from_toml(SEED_SNIPPET).expect("seed snippet should parse");
        assert_eq!(ruleset.meta.version, "rules-v1");
        let memory = ruleset.topics.get("memory").expect("memory topic exists");
        assert_eq!(memory.threshold, 3.0);
        assert_eq!(memory.rules.len(), 3);
        assert_eq!(memory.rules[0].id, "title_concept");
        assert_eq!(memory.rules[0].weight, 2.0);
        assert_eq!(memory.rules[1].min_hits, 2);
        assert_eq!(memory.subreddit_prior.get("opencodeCLI"), Some(&2.0));
        assert_eq!(memory.exclude.len(), 1);
        assert!(memory.exclude[0].veto);
        assert!(memory.exclude[0].unless.is_some());
    }

    #[test]
    fn default_weight_and_min_hits_apply() {
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "any", kind = "fts", match = "agent*" }]
"#;
        let ruleset = RuleSet::from_toml(toml).expect("should parse");
        let rule = &ruleset.topics["t"].rules[0];
        assert_eq!(rule.weight, 1.0);
        assert_eq!(rule.min_hits, 1);
    }

    #[test]
    fn rejects_unknown_fields() {
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "any", kind = "fts", match = "x", surprise = true }]
"#;
        let err = RuleSet::from_toml(toml).expect_err("unknown field should fail");
        assert!(err.to_string().contains("parse") || format!("{err:#}").contains("surprise"));
    }
}
