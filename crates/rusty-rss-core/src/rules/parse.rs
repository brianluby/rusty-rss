//! Lower `(signal, kind, match)` triples into safe, column-scoped FTS5 operands.

use super::types::{Kind, MatchSpec, Signal};
use anyhow::{Context, Result, bail};
use std::collections::HashSet;

/// Lower one `(signal, kind, match)` triple into safe FTS5 operand strings.
pub(super) fn compile_match(signal: Signal, kind: Kind, spec: &MatchSpec) -> Result<Vec<String>> {
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
    use crate::rules::RuleSet;

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
}
