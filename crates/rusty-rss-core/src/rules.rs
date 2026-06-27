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

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

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

/// A rule lowered to safe, column-scoped FTS5 operands.
///
/// A post fires the rule when it matches at least `min_hits` distinct
/// `operands`; `weight` is then added once.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledRule {
    pub id: String,
    pub weight: f32,
    pub min_hits: usize,
    pub operands: Vec<String>,
}

/// An exclude lowered to FTS5 operands. Veto applies when a post matches any
/// `operands` and (if present) does NOT match any `unless` operand.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledExclude {
    pub id: String,
    pub operands: Vec<String>,
    pub unless: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledTopic {
    pub name: String,
    pub threshold: f32,
    pub rules: Vec<CompiledRule>,
    pub excludes: Vec<CompiledExclude>,
    pub subreddit_prior: BTreeMap<String, f32>,
}

/// All topics compiled, plus the ruleset version stamped onto written tags.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledRuleSet {
    pub version: String,
    pub topics: Vec<CompiledTopic>,
}

impl RuleSet {
    /// Parse and structurally validate a `rules.toml` from disk.
    pub fn from_path(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read rules file at {}", path.display()))?;
        Self::from_toml(&contents)
            .with_context(|| format!("invalid rules file at {}", path.display()))
    }

    /// Parse and structurally validate a `rules.toml` from a string.
    pub fn from_toml(contents: &str) -> Result<Self> {
        let ruleset: RuleSet = toml::from_str(contents).context("failed to parse rules TOML")?;
        ruleset.validate()?;
        Ok(ruleset)
    }

    /// Read, parse, and compile a `rules.toml` in one pass (single compilation).
    /// This is the entry point the evaluator uses; `compile` does the full
    /// structural validation, so a malformed file fails here with context.
    pub fn load(path: &Path) -> Result<CompiledRuleSet> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read rules file at {}", path.display()))?;
        let ruleset: RuleSet = toml::from_str(&contents)
            .with_context(|| format!("failed to parse rules file at {}", path.display()))?;
        ruleset
            .compile()
            .with_context(|| format!("invalid rules file at {}", path.display()))
    }

    fn validate(&self) -> Result<()> {
        // All structural validation lives in compile(), so every entry point
        // (from_toml and the CLI load() path) is checked identically.
        self.compile()?;
        Ok(())
    }

    /// Lower every topic to FTS5 operands, validating meta/signals/kinds/shapes.
    pub fn compile(&self) -> Result<CompiledRuleSet> {
        if self.meta.version.trim().is_empty() {
            bail!("[meta].version must not be empty");
        }
        if self.topics.is_empty() {
            bail!("rules file defines no topics");
        }
        let mut topics = Vec::with_capacity(self.topics.len());
        for (name, topic) in &self.topics {
            if !topic.threshold.is_finite() {
                bail!("topic '{name}': threshold must be a finite number");
            }
            let mut rules = Vec::with_capacity(topic.rules.len());
            let mut seen_ids: HashSet<&str> = HashSet::new();
            for rule in &topic.rules {
                if !seen_ids.insert(rule.id.as_str()) {
                    bail!("topic '{name}': duplicate rule id '{}'", rule.id);
                }
                rules.push(
                    compile_rule(name, rule).with_context(|| {
                        format!("topic '{name}': rule '{}' is invalid", rule.id)
                    })?,
                );
            }
            let mut excludes = Vec::with_capacity(topic.exclude.len());
            for exclude in &topic.exclude {
                excludes.push(compile_exclude(name, exclude).with_context(|| {
                    format!("topic '{name}': exclude '{}' is invalid", exclude.id)
                })?);
            }
            // Normalize subreddit prior keys to lowercase so weighting matches
            // case-insensitively (FTS matching is case-insensitive, and Reddit
            // subreddit names are too).
            let mut subreddit_prior = BTreeMap::new();
            for (subreddit, weight) in &topic.subreddit_prior {
                if !weight.is_finite() {
                    bail!("topic '{name}': subreddit_prior['{subreddit}'] must be finite");
                }
                subreddit_prior.insert(subreddit.to_lowercase(), *weight);
            }
            topics.push(CompiledTopic {
                name: name.clone(),
                threshold: topic.threshold,
                rules,
                excludes,
                subreddit_prior,
            });
        }
        Ok(CompiledRuleSet {
            version: self.meta.version.clone(),
            topics,
        })
    }
}

fn compile_rule(topic: &str, rule: &Rule) -> Result<CompiledRule> {
    if rule.id.trim().is_empty() {
        bail!("topic '{topic}': a rule is missing an id");
    }
    if !rule.weight.is_finite() {
        bail!("weight must be a finite number");
    }
    if rule.min_hits < 1 {
        bail!("min_hits must be at least 1");
    }
    let operands = compile_match(rule.signal, rule.kind, &rule.match_spec)?;
    if rule.min_hits > operands.len() {
        bail!(
            "min_hits ({}) exceeds the {} match alternative(s); the rule can never fire",
            rule.min_hits,
            operands.len()
        );
    }
    Ok(CompiledRule {
        id: rule.id.clone(),
        weight: rule.weight,
        min_hits: rule.min_hits,
        operands,
    })
}

fn compile_exclude(topic: &str, exclude: &ExcludeRule) -> Result<CompiledExclude> {
    if exclude.id.trim().is_empty() {
        bail!("topic '{topic}': an exclude is missing an id");
    }
    if !exclude.veto {
        bail!(
            "exclude '{}' must set veto = true (non-veto excludes are not supported in rules-v1)",
            exclude.id
        );
    }
    let operands = compile_match(exclude.signal, exclude.kind, &exclude.match_spec)?;
    let unless = match &exclude.unless {
        Some(guard) => Some(
            compile_match(guard.signal, guard.kind, &guard.match_spec)
                .context("invalid `unless` guard")?,
        ),
        None => None,
    };
    Ok(CompiledExclude {
        id: exclude.id.clone(),
        operands,
        unless,
    })
}

/// Lower one `(signal, kind, match)` triple into safe FTS5 operand strings.
fn compile_match(signal: Signal, kind: Kind, spec: &MatchSpec) -> Result<Vec<String>> {
    let column = column_scope(signal)?;
    match kind {
        Kind::Fts => {
            let MatchSpec::Expression(expr) = spec else {
                bail!("kind = \"fts\" requires `match` to be a single expression string");
            };
            if expr.trim().is_empty() {
                bail!("`match` expression must not be empty");
            }
            Ok(dedup_operands(
                split_top_level_or(expr)
                    .into_iter()
                    .map(|operand| scope_operand(column, &operand))
                    .collect(),
            ))
        }
        Kind::Terms => {
            let MatchSpec::Terms(terms) = spec else {
                bail!("kind = \"terms\" requires `match` to be a list of tokens");
            };
            if terms.is_empty() {
                bail!("`match` term list must not be empty");
            }
            let mut operands = Vec::with_capacity(terms.len());
            for term in terms {
                let escaped = escape_term(term)
                    .with_context(|| format!("invalid term {term:?} in `match` list"))?;
                operands.push(scope_operand(column, &escaped));
            }
            Ok(dedup_operands(operands))
        }
        Kind::Exact => {
            bail!("kind = \"exact\" is not supported in rules-v1; use \"fts\" or \"terms\"")
        }
        Kind::Regex => {
            bail!("kind = \"regex\" is not supported in rules-v1; use \"fts\" or \"terms\"")
        }
    }
}

/// FTS5 column token for a signal, or an error for deferred signals.
fn column_scope(signal: Signal) -> Result<&'static str> {
    match signal {
        Signal::Title => Ok("title"),
        Signal::Body => Ok("content_markdown"),
        Signal::Any => Ok(""),
        Signal::Subreddit => bail!(
            "signal = \"subreddit\" match rules are not supported in rules-v1; \
             use [topics.<name>.subreddit_prior] for subreddit weighting"
        ),
        Signal::Domain => {
            bail!("signal = \"domain\" is deferred to Gate 2 and not supported in rules-v1")
        }
    }
}

/// Wrap a sub-expression in its column scope. The column comes from a validated
/// enum, never from config text, so the scope cannot be injected.
fn scope_operand(column: &str, sub_expr: &str) -> String {
    if column.is_empty() {
        format!("({sub_expr})")
    } else {
        format!("{column} : ({sub_expr})")
    }
}

/// Turn a literal term into a quoted FTS5 phrase, neutralizing operators.
///
/// A trailing `*` is treated as a prefix match and placed outside the quotes
/// (`memor*` -> `"memor"*`). Mirrors the escaping in
/// [`crate::db`]'s search-query handling.
fn escape_term(term: &str) -> Result<String> {
    let (body, prefix) = match term.strip_suffix('*') {
        Some(stripped) => (stripped, true),
        None => (term, false),
    };
    if !body.chars().any(|ch| ch.is_alphanumeric()) {
        bail!("term must contain at least one alphanumeric character");
    }
    let quoted = format!("\"{}\"", body.replace('"', "\"\""));
    if prefix {
        Ok(format!("{quoted}*"))
    } else {
        Ok(quoted)
    }
}

/// Split an FTS5 expression on top-level ` OR `, ignoring `OR` inside double
/// quotes or parentheses. Each piece becomes one min_hits alternative.
fn split_top_level_or(expr: &str) -> Vec<String> {
    let chars: Vec<char> = expr.chars().collect();
    let n = chars.len();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth: i32 = 0;
    let mut in_quote = false;
    let mut i = 0usize;

    while i < n {
        let c = chars[i];
        if c == '"' {
            in_quote = !in_quote;
            i += 1;
        } else if in_quote {
            i += 1;
        } else if c == '(' {
            depth += 1;
            i += 1;
        } else if c == ')' {
            depth -= 1;
            i += 1;
        } else if depth == 0
            && c == 'O'
            && i + 1 < n
            && chars[i + 1] == 'R'
            && (i == 0 || chars[i - 1].is_whitespace())
            && (i + 2 >= n || chars[i + 2].is_whitespace())
        {
            push_trimmed(&mut parts, &chars[start..i]);
            i += 2;
            start = i;
        } else {
            i += 1;
        }
    }
    push_trimmed(&mut parts, &chars[start..]);

    if parts.is_empty() {
        parts.push(expr.trim().to_string());
    }
    parts
}

fn push_trimmed(parts: &mut Vec<String>, chars: &[char]) {
    let Some(start) = chars.iter().position(|ch| !ch.is_whitespace()) else {
        return;
    };
    let end = chars
        .iter()
        .rposition(|ch| !ch.is_whitespace())
        .unwrap_or(start);
    parts.push(chars[start..=end].iter().collect());
}

/// Drop duplicate operands while preserving order, so `min_hits` counts
/// genuinely distinct alternatives even if the config repeats one. Comparison is
/// case-insensitive because FTS matching is, so `mem0`/`MEM0` and
/// `memor*`/`MEMOR*` collapse to a single alternative.
fn dedup_operands(operands: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    operands
        .into_iter()
        .filter(|operand| seen.insert(operand.to_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // TOML 1.0 forbids multi-line inline tables, so each rule sits on one line.
    // The shipped seed uses the [[topics.<name>.rules]] form for readability.
    const SEED_SNIPPET: &str = r#"
[meta]
version = "rules-v1"

[topics.memory]
threshold = 3.0
rules = [
  { id = "title_concept", signal = "title", kind = "fts", weight = 2.0, match = 'memor* OR "second brain" OR obsidian OR persisten*' },
  { id = "body_concept", signal = "body", kind = "fts", weight = 0.5, min_hits = 2, match = 'memor* OR "knowledge graph" OR retriev* OR embedding*' },
  { id = "named_tool", signal = "any", kind = "terms", weight = 3.0, match = ["mem0", "letta", "memgpt", "zep", "cognee"] },
]

[topics.memory.subreddit_prior]
opencodeCLI = 2.0
mcp = 2.0

[[topics.memory.exclude]]
id = "hardware_memory"
signal = "title"
kind = "fts"
match = 'vram OR "gpu memory" OR oom'
unless = { signal = "any", kind = "terms", match = ["mem0", "letta"] }
veto = true
"#;

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

    #[test]
    fn rejects_domain_signal() {
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "domain", kind = "fts", match = "github" }]
"#;
        let err = RuleSet::from_toml(toml).expect_err("domain signal should fail");
        assert!(format!("{err:#}").contains("domain"));
    }

    #[test]
    fn rejects_subreddit_match_rule() {
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "subreddit", kind = "terms", match = ["mcp"] }]
"#;
        let err = RuleSet::from_toml(toml).expect_err("subreddit match rule should fail");
        assert!(format!("{err:#}").contains("subreddit"));
    }

    #[test]
    fn rejects_exact_and_regex_kinds() {
        for kind in ["exact", "regex"] {
            let toml = format!(
                r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{{ id = "r", signal = "any", kind = "{kind}", match = "x" }}]
"#
            );
            let err = RuleSet::from_toml(&toml).expect_err("deferred kind should fail");
            assert!(format!("{err:#}").contains(kind));
        }
    }

    #[test]
    fn rejects_min_hits_exceeding_alternatives() {
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "body", kind = "fts", min_hits = 3, match = "a OR b" }]
"#;
        let err = RuleSet::from_toml(toml).expect_err("impossible min_hits should fail");
        assert!(format!("{err:#}").contains("min_hits"));
    }

    #[test]
    fn rejects_non_alphanumeric_term() {
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "any", kind = "terms", match = ["!!!"] }]
"#;
        let err = RuleSet::from_toml(toml).expect_err("noise term should fail");
        assert!(format!("{err:#}").contains("alphanumeric"));
    }

    #[test]
    fn rejects_non_veto_exclude() {
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "title", kind = "fts", match = "x" }]
[[topics.t.exclude]]
id = "e"
signal = "title"
kind = "fts"
match = "vram"
"#;
        let err = RuleSet::from_toml(toml).expect_err("non-veto exclude should fail");
        assert!(format!("{err:#}").contains("veto"));
    }

    #[test]
    fn rejects_duplicate_rule_id() {
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [
  { id = "dup", signal = "title", kind = "fts", match = "a" },
  { id = "dup", signal = "title", kind = "fts", match = "b" },
]
"#;
        let err = RuleSet::from_toml(toml).expect_err("duplicate rule id should fail");
        assert!(format!("{err:#}").contains("duplicate rule id"));
    }

    #[test]
    fn dedups_repeated_fts_operands() {
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "title", kind = "fts", match = "memor* OR memor*" }]
"#;
        let compiled = RuleSet::from_toml(toml)
            .expect("parse")
            .compile()
            .expect("compile");
        assert_eq!(
            compiled.topics[0].rules[0].operands,
            vec!["title : (memor*)"],
            "identical OR-alternatives collapse to one operand"
        );
    }

    #[test]
    fn dedup_operands_is_case_insensitive() {
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "any", kind = "terms", match = ["mem0", "MEM0", "letta"] }]
"#;
        let compiled = RuleSet::from_toml(toml)
            .expect("parse")
            .compile()
            .expect("compile");
        // mem0 and MEM0 match the same posts under FTS, so they collapse.
        assert_eq!(compiled.topics[0].rules[0].operands.len(), 2);
    }

    #[test]
    fn collapsed_duplicate_operands_cannot_meet_min_hits() {
        // After dedup, two identical tokens are one alternative, so min_hits=2
        // can never be satisfied and the rule is rejected at compile time.
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "any", kind = "terms", min_hits = 2, match = ["mem0", "mem0"] }]
"#;
        let err = RuleSet::from_toml(toml).expect_err("collapsed operands should fail min_hits");
        assert!(format!("{err:#}").contains("min_hits"));
    }

    #[test]
    fn split_top_level_or_respects_quotes_and_parens() {
        assert_eq!(
            split_top_level_or(r#"memor* OR "second brain" OR obsidian"#),
            vec!["memor*", "\"second brain\"", "obsidian"]
        );
        // OR inside a quoted phrase is not a split point.
        assert_eq!(
            split_top_level_or(r#""this OR that" OR other"#),
            vec!["\"this OR that\"", "other"]
        );
        // OR inside parentheses is not a top-level split point.
        assert_eq!(split_top_level_or("(a OR b) AND c"), vec!["(a OR b) AND c"]);
        // A bare expression with no OR is a single operand.
        assert_eq!(split_top_level_or("mcp*"), vec!["mcp*"]);
        // "OR" embedded in a token is not a split point.
        assert_eq!(split_top_level_or("orchestrate*"), vec!["orchestrate*"]);
    }

    #[test]
    fn compiles_fts_rule_to_scoped_operands() {
        let ruleset = RuleSet::from_toml(SEED_SNIPPET).expect("parse");
        let compiled = ruleset.compile().expect("compile");
        let memory = compiled
            .topics
            .iter()
            .find(|t| t.name == "memory")
            .expect("memory");
        let title = &memory.rules[0];
        assert_eq!(
            title.operands,
            vec![
                "title : (memor*)",
                "title : (\"second brain\")",
                "title : (obsidian)",
                "title : (persisten*)",
            ]
        );
    }

    #[test]
    fn compiles_terms_rule_with_prefix_and_phrase() {
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "any", kind = "terms", match = ["mem0", "memor*", "knowledge graph"] }]
"#;
        let compiled = RuleSet::from_toml(toml)
            .expect("parse")
            .compile()
            .expect("compile");
        let operands = &compiled.topics[0].rules[0].operands;
        assert_eq!(
            operands,
            &vec![
                "(\"mem0\")".to_string(),
                "(\"memor\"*)".to_string(),
                "(\"knowledge graph\")".to_string(),
            ]
        );
    }

    #[test]
    fn load_rejects_rules_file_with_no_topics() {
        // A topic-less file must fail on the CLI load() path too, otherwise a
        // full-archive `tag` run would delete every post_tags row and write none.
        let path =
            std::env::temp_dir().join(format!("rusty_rss_rules_empty_{}.toml", std::process::id()));
        std::fs::write(&path, "[meta]\nversion = \"v\"\n").expect("write temp rules");

        let err = RuleSet::load(&path).expect_err("empty-topics rules must be rejected");
        assert!(format!("{err:#}").contains("defines no topics"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn shipped_seed_parses_and_compiles() {
        let seed = include_str!("../../../rules.toml");
        let ruleset = RuleSet::from_toml(seed).expect("shipped rules.toml should parse");
        let compiled = ruleset
            .compile()
            .expect("shipped rules.toml should compile");
        assert!(
            compiled.topics.iter().any(|topic| topic.name == "memory"),
            "seed should define the memory topic"
        );
        assert!(
            compiled.topics.len() >= 6,
            "seed should ship the derived topics"
        );
    }

    #[test]
    fn compiles_exclude_with_unless_guard() {
        let ruleset = RuleSet::from_toml(SEED_SNIPPET).expect("parse");
        let compiled = ruleset.compile().expect("compile");
        let memory = compiled
            .topics
            .iter()
            .find(|t| t.name == "memory")
            .expect("memory");
        assert_eq!(memory.excludes.len(), 1);
        let exclude = &memory.excludes[0];
        assert_eq!(exclude.id, "hardware_memory");
        assert_eq!(
            exclude.operands,
            vec![
                "title : (vram)",
                "title : (\"gpu memory\")",
                "title : (oom)"
            ]
        );
        let unless = exclude.unless.as_ref().expect("unless compiled");
        assert_eq!(
            unless,
            &vec!["(\"mem0\")".to_string(), "(\"letta\")".to_string()]
        );
    }
}
