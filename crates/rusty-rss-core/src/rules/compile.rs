//! Validate a [`RuleSet`] and lower it into executable [`CompiledRuleSet`] form.

use super::parse::compile_match;
use super::types::{ExcludeRule, Rule, RuleSet};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// A rule lowered to safe, column-scoped FTS5 operands.
///
/// A post fires the rule when it matches at least `min_hits` distinct
/// `operands`; `weight` is then added once.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledRule {
    /// Stable rule id, recorded in tag provenance.
    pub id: String,
    /// Score added once when the rule fires.
    pub weight: f32,
    /// Distinct operands that must match for the rule to fire.
    pub min_hits: usize,
    /// Column-scoped FTS5 operands the rule matches against.
    pub operands: Vec<String>,
}

/// An exclude lowered to FTS5 operands. Veto applies when a post matches any
/// `operands` and (if present) does NOT match any `unless` operand.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledExclude {
    /// Stable exclude id, recorded in tag provenance.
    pub id: String,
    /// FTS5 operands whose match triggers the exclude.
    pub operands: Vec<String>,
    /// Operands that, when matched, cancel the exclude.
    pub unless: Option<Vec<String>>,
}

/// A topic with its rules and excludes lowered to executable form.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledTopic {
    /// Topic name.
    pub name: String,
    /// Minimum total score for the topic to be assigned.
    pub threshold: f32,
    /// Compiled scoring rules.
    pub rules: Vec<CompiledRule>,
    /// Compiled exclude rules.
    pub excludes: Vec<CompiledExclude>,
    /// Subreddit -> additive prior weight.
    pub subreddit_prior: BTreeMap<String, f32>,
}

/// All topics compiled, plus the ruleset version stamped onto written tags.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledRuleSet {
    /// Ruleset version, stamped onto every written tag.
    pub version: String,
    /// Compiled topics.
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
                let normalized = subreddit.to_lowercase();
                if subreddit_prior
                    .insert(normalized.clone(), *weight)
                    .is_some()
                {
                    bail!(
                        "topic '{name}': duplicate subreddit_prior entry '{normalized}' after \
                         case-normalization"
                    );
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::SEED_SNIPPET;

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
    fn rejects_case_only_duplicate_subreddit_prior() {
        // "Rust" and "rust" are distinct TOML keys but collapse to the same key
        // after lowercase normalization; merging them silently would drop one
        // weight, so compilation must fail instead.
        let toml = r#"
[meta]
version = "v"
[topics.t]
threshold = 1.0
rules = [{ id = "r", signal = "title", kind = "fts", match = "a" }]
[topics.t.subreddit_prior]
Rust = 1.0
rust = 2.0
"#;
        let err =
            RuleSet::from_toml(toml).expect_err("case-only duplicate subreddit_prior should fail");
        assert!(format!("{err:#}").contains("subreddit_prior"));
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
        let seed = include_str!("../../../../rules.toml");
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
