//! Per-topic rule firing and exclude/guard rowid sets over the FTS index.

use crate::db;
use crate::rules::CompiledRule;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

pub(super) struct RuleHit<'a> {
    pub(super) id: &'a str,
    pub(super) weight: f32,
    pub(super) fired: HashSet<i64>,
}

pub(super) struct ExcludeHit<'a> {
    pub(super) id: &'a str,
    pub(super) matched: HashSet<i64>,
    pub(super) guarded: Option<HashSet<i64>>,
}

/// Rowids that fire a rule: those matching at least `min_hits` distinct operands.
pub(super) fn fired_rowids(conn: &Connection, rule: &CompiledRule) -> Result<HashSet<i64>> {
    let mut counts: HashMap<i64, usize> = HashMap::new();
    for operand in &rule.operands {
        for rowid in db::fts_matching_rowids(conn, operand)? {
            *counts.entry(rowid).or_insert(0) += 1;
        }
    }
    Ok(counts
        .into_iter()
        .filter(|&(_, count)| count >= rule.min_hits)
        .map(|(rowid, _)| rowid)
        .collect())
}

/// Rowids matching any of the operands (union semantics, for excludes/guards).
pub(super) fn union_rowids(conn: &Connection, operands: &[String]) -> Result<HashSet<i64>> {
    let mut all = HashSet::new();
    for operand in operands {
        all.extend(db::fts_matching_rowids(conn, operand)?);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use crate::rules::RuleSet;
    use crate::tag::test_support::{compiled, insert, tag_one, test_db};
    use crate::tag::{TagOptions, run_tagging_batch};

    #[test]
    fn worked_example_passes_with_title_and_prior() {
        let conn = test_db();
        insert(
            &conn,
            "t3_opencode",
            "We built a persistent memory plugin for OpenCode",
            Some("opencodeCLI"),
            None,
        );

        run_tagging_batch(&conn, &compiled(), &TagOptions::default()).expect("tagging should run");

        let tag = tag_one(&conn, "t3_opencode", "memory").expect("memory tag should exist");
        assert!(
            tag.passed,
            "title hit + subreddit prior should pass: {tag:?}"
        );
        assert_eq!(tag.score, 4.0);
        assert!(tag.matched_rules.contains(&"title_concept".to_string()));
        assert!(tag.matched_rules.contains(&"prior:opencodeCLI".to_string()));
    }

    #[test]
    fn worked_example_vetoes_hardware_memory() {
        let conn = test_db();
        insert(
            &conn,
            "t3_vram",
            "How much memory do I need, is 24GB VRAM enough for 70B?",
            Some("LocalLLaMA"),
            None,
        );

        run_tagging_batch(&conn, &compiled(), &TagOptions::default()).expect("tagging should run");

        let tag = tag_one(&conn, "t3_vram", "memory").expect("vetoed row should be written");
        assert!(
            !tag.passed,
            "hardware-memory veto should force passed = false"
        );
        assert!(
            tag.matched_rules
                .iter()
                .any(|rule| rule == "veto:hardware_memory"),
            "veto provenance should be recorded: {:?}",
            tag.matched_rules
        );
    }

    #[test]
    fn unless_guard_protects_named_tool() {
        let conn = test_db();
        // VRAM term present, but a named memory tool keeps it a genuine memory post.
        insert(
            &conn,
            "t3_guarded",
            "letta memory server benchmarks on 24GB VRAM",
            Some("opencodeCLI"),
            None,
        );

        run_tagging_batch(&conn, &compiled(), &TagOptions::default()).expect("tagging should run");

        let tag = tag_one(&conn, "t3_guarded", "memory").expect("memory tag should exist");
        assert!(
            !tag.matched_rules
                .iter()
                .any(|rule| rule.starts_with("veto:")),
            "unless guard should suppress the veto: {:?}",
            tag.matched_rules
        );
        assert!(tag.passed);
    }

    #[test]
    fn min_hits_requires_distinct_terms() {
        let conn = test_db();
        // Only one body term -> below min_hits=2 -> no scoring rule fires -> no row.
        insert(&conn, "t3_one", "Notes", None, Some("I use retrieval only"));
        // Two distinct body terms -> body_concept fires (near-miss row, passed=false).
        insert(
            &conn,
            "t3_two",
            "Notes",
            None,
            Some("I use retrieval and embeddings daily"),
        );

        run_tagging_batch(&conn, &compiled(), &TagOptions::default()).expect("tagging should run");

        assert!(
            tag_one(&conn, "t3_one", "memory").is_none(),
            "one term should not tag"
        );
        let two = tag_one(&conn, "t3_two", "memory").expect("two terms should tag");
        assert!(!two.passed);
        assert_eq!(two.score, 0.5);
    }

    #[test]
    fn subreddit_prior_is_case_insensitive() {
        let conn = test_db();
        // Config key is "opencodeCLI"; the stored subreddit differs only in case.
        insert(
            &conn,
            "t3_case",
            "a persistent memory plugin",
            Some("OPENCODECLI"),
            None,
        );

        run_tagging_batch(&conn, &compiled(), &TagOptions::default()).expect("tagging should run");

        let tag = tag_one(&conn, "t3_case", "memory").expect("memory tag should exist");
        assert_eq!(
            tag.score, 4.0,
            "a case-mismatched subreddit should still receive its prior"
        );
        assert!(tag.passed);
    }

    #[test]
    fn score_equal_to_threshold_passes() {
        let conn = test_db();
        // agents threshold is 2.0; a title hit alone scores exactly 2.0.
        insert(&conn, "t3_eq", "multi agent orchestration", None, None);

        run_tagging_batch(&conn, &compiled(), &TagOptions::default()).expect("tagging should run");

        let tag = tag_one(&conn, "t3_eq", "agents").expect("agents tag should exist");
        assert_eq!(tag.score, 2.0);
        assert!(tag.passed, "score exactly equal to threshold must pass");
    }

    #[test]
    fn negative_weight_writes_failing_row() {
        let conn = test_db();
        insert(&conn, "t3_neg", "memory note", None, None);
        let rules = RuleSet::from_toml(
            r#"
[meta]
version = "neg"
[topics.memory]
threshold = 1.0
rules = [{ id = "penalty", signal = "title", kind = "fts", weight = -1.0, match = "memor*" }]
"#,
        )
        .expect("parse")
        .compile()
        .expect("compile");

        run_tagging_batch(&conn, &rules, &TagOptions::default()).expect("tagging should run");

        let tag = tag_one(&conn, "t3_neg", "memory")
            .expect("a fired rule writes a row even with a negative score");
        assert_eq!(tag.score, -1.0);
        assert!(!tag.passed);
        assert!(tag.matched_rules.contains(&"penalty".to_string()));
    }

    #[test]
    fn prior_only_topic_writes_no_rows() {
        let conn = test_db();
        insert(
            &conn,
            "t3_prioronly",
            "totally unrelated title",
            Some("opencodeCLI"),
            None,
        );
        let rules = RuleSet::from_toml(
            r#"
[meta]
version = "prioronly"
[topics.memory]
threshold = 1.0
[topics.memory.subreddit_prior]
opencodeCLI = 5.0
"#,
        )
        .expect("parse")
        .compile()
        .expect("compile");

        run_tagging_batch(&conn, &rules, &TagOptions::default()).expect("tagging should run");

        assert!(
            tag_one(&conn, "t3_prioronly", "memory").is_none(),
            "a subreddit prior alone must never create a row"
        );
    }

    #[test]
    fn signals_round_trip_through_db() {
        let conn = test_db();
        insert(
            &conn,
            "t3_sig",
            "a persistent memory plugin",
            Some("opencodeCLI"),
            None,
        );

        run_tagging_batch(&conn, &compiled(), &TagOptions::default()).expect("tagging should run");

        let tag = tag_one(&conn, "t3_sig", "memory").expect("memory tag should exist");
        assert_eq!(tag.signals.get("title_concept"), Some(&2.0));
        assert_eq!(tag.signals.get("prior"), Some(&2.0));
    }
}
