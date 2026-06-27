//! Orchestration: evaluate the ruleset over the archive and materialize tags.

use super::score::{ExcludeHit, RuleHit, fired_rowids, union_rowids};
use super::types::{TagOptions, TagSummary};
use crate::db;
use crate::models::PostTag;
use crate::rules::{CompiledRuleSet, CompiledTopic};
use anyhow::{Result, bail};
use chrono::Utc;
use rusqlite::Connection;
use std::collections::BTreeMap;

/// Evaluate the ruleset over the archive and materialize `post_tags`.
///
/// The run is authoritative for its scope: on persist, stale rows for the
/// processed posts and topics are deleted before the fresh rows are inserted,
/// so a post that no longer matches a topic loses its tag.
pub fn run_tagging_batch(
    conn: &Connection,
    compiled: &CompiledRuleSet,
    options: &TagOptions,
) -> Result<TagSummary> {
    let version = compiled.version.clone();
    let topics: Vec<&CompiledTopic> = match &options.topic {
        Some(name) => {
            let filtered: Vec<&CompiledTopic> = compiled
                .topics
                .iter()
                .filter(|topic| &topic.name == name)
                .collect();
            if filtered.is_empty() {
                bail!("unknown topic '{name}' (not defined in the rules file)");
            }
            filtered
        }
        None => compiled.topics.iter().collect(),
    };

    // Fail-closed: smoke-test every compiled operand against the FTS index
    // before doing any work, so one malformed rule aborts the run cleanly
    // instead of surfacing mid-sweep.
    for topic in &topics {
        for rule in &topic.rules {
            for operand in &rule.operands {
                db::validate_fts_expr(conn, operand)?;
            }
        }
        for exclude in &topic.excludes {
            for operand in &exclude.operands {
                db::validate_fts_expr(conn, operand)?;
            }
            if let Some(unless) = &exclude.unless {
                for operand in unless {
                    db::validate_fts_expr(conn, operand)?;
                }
            }
        }
    }

    let posts = db::list_taggable_posts(conn, options.limit)?;
    let tagged_at = Utc::now().to_rfc3339();
    let mut tags: Vec<PostTag> = Vec::new();
    let mut passed_count = 0usize;
    let mut vetoed_count = 0usize;

    for topic in &topics {
        // Precompute rule fired-sets and exclude/guard sets once per topic.
        let mut rule_hits: Vec<RuleHit<'_>> = Vec::with_capacity(topic.rules.len());
        for rule in &topic.rules {
            rule_hits.push(RuleHit {
                id: rule.id.as_str(),
                weight: rule.weight,
                fired: fired_rowids(conn, rule)?,
            });
        }
        let mut exclude_hits: Vec<ExcludeHit<'_>> = Vec::with_capacity(topic.excludes.len());
        for exclude in &topic.excludes {
            let matched = union_rowids(conn, &exclude.operands)?;
            let guarded = match &exclude.unless {
                Some(operands) => Some(union_rowids(conn, operands)?),
                None => None,
            };
            exclude_hits.push(ExcludeHit {
                id: exclude.id.as_str(),
                matched,
                guarded,
            });
        }

        for post in &posts {
            let mut score = 0.0f32;
            let mut matched_rules: Vec<String> = Vec::new();
            let mut signals: BTreeMap<String, f32> = BTreeMap::new();

            for hit in &rule_hits {
                if hit.fired.contains(&post.rowid) {
                    score += hit.weight;
                    matched_rules.push(hit.id.to_string());
                    signals.insert(hit.id.to_string(), hit.weight);
                }
            }

            // A row is written only when at least one scoring rule fired: the
            // subreddit prior is a contextual boost, not topical evidence.
            if matched_rules.is_empty() {
                continue;
            }

            if let Some(subreddit) = &post.subreddit
                && let Some(prior) = topic.subreddit_prior.get(&subreddit.to_lowercase())
                && *prior != 0.0
            {
                score += *prior;
                matched_rules.push(format!("prior:{subreddit}"));
                signals.insert("prior".to_string(), *prior);
            }

            let mut vetoed = false;
            for exclude in &exclude_hits {
                let guard_protected = exclude
                    .guarded
                    .as_ref()
                    .is_some_and(|guard| guard.contains(&post.rowid));
                if exclude.matched.contains(&post.rowid) && !guard_protected {
                    vetoed = true;
                    matched_rules.push(format!("veto:{}", exclude.id));
                }
            }

            let passed = score >= topic.threshold && !vetoed;
            if passed {
                passed_count += 1;
            }
            if vetoed {
                vetoed_count += 1;
            }

            tags.push(PostTag {
                reddit_fullname: post.reddit_fullname.clone(),
                topic: topic.name.clone(),
                score,
                threshold: topic.threshold,
                passed,
                matched_rules,
                signals,
                ruleset_version: version.clone(),
                tagged_at: tagged_at.clone(),
            });
        }
    }

    let rows_written = tags.len();
    if !options.dry_run {
        let processed_fullnames: Vec<&str> = posts
            .iter()
            .map(|post| post.reddit_fullname.as_str())
            .collect();
        db::replace_post_tags(
            conn,
            options.topic.as_deref(),
            options.limit.is_none(),
            &processed_fullnames,
            &tags,
        )?;
    }

    Ok(TagSummary {
        selected_posts: posts.len(),
        topics_evaluated: topics.len(),
        rows_written,
        passed_count,
        vetoed_count,
        tags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RuleSet;
    use crate::tag::test_support::{compiled, insert, tag_one, test_db};

    #[test]
    fn multi_label_writes_one_row_per_topic() {
        let conn = test_db();
        insert(
            &conn,
            "t3_both",
            "agentic memory system",
            Some("opencodeCLI"),
            None,
        );

        run_tagging_batch(&conn, &compiled(), &TagOptions::default()).expect("tagging should run");

        let tags = db::post_tags_for(&conn, "t3_both").expect("tags should query");
        let topics: Vec<&str> = tags.iter().map(|tag| tag.topic.as_str()).collect();
        assert!(topics.contains(&"memory"), "should tag memory: {topics:?}");
        assert!(topics.contains(&"agents"), "should tag agents: {topics:?}");
    }

    #[test]
    fn dry_run_writes_nothing() {
        let conn = test_db();
        insert(
            &conn,
            "t3_dry",
            "persistent memory plugin",
            Some("opencodeCLI"),
            None,
        );

        let summary = run_tagging_batch(
            &conn,
            &compiled(),
            &TagOptions {
                dry_run: true,
                ..TagOptions::default()
            },
        )
        .expect("dry run should run");

        assert!(
            summary.rows_written > 0,
            "dry run should report would-be rows"
        );
        assert!(
            db::post_tags_for(&conn, "t3_dry")
                .expect("query")
                .is_empty(),
            "dry run must not persist rows"
        );
    }

    #[test]
    fn re_tag_removes_stale_rows() {
        let conn = test_db();
        insert(
            &conn,
            "t3_stale",
            "a persistent memory system",
            Some("opencodeCLI"),
            None,
        );
        run_tagging_batch(&conn, &compiled(), &TagOptions::default()).expect("first tagging");
        assert!(tag_one(&conn, "t3_stale", "memory").is_some());

        // A ruleset that no longer matches the post must clear its old tag.
        let narrowed = RuleSet::from_toml(
            r#"
[meta]
version = "rules-test-v2"
[topics.memory]
threshold = 3.0
rules = [{ id = "title_concept", signal = "title", kind = "fts", weight = 2.0, match = "kubernetes" }]
"#,
        )
        .expect("narrowed rules parse")
        .compile()
        .expect("narrowed rules compile");
        run_tagging_batch(&conn, &narrowed, &TagOptions::default()).expect("second tagging");

        assert!(
            tag_one(&conn, "t3_stale", "memory").is_none(),
            "stale memory tag should be removed on re-tag"
        );
    }

    #[test]
    fn re_tag_drops_rows_for_removed_topic() {
        let conn = test_db();
        insert(
            &conn,
            "t3_drop",
            "agentic memory",
            Some("opencodeCLI"),
            None,
        );
        run_tagging_batch(&conn, &compiled(), &TagOptions::default()).expect("first tagging");
        assert!(tag_one(&conn, "t3_drop", "agents").is_some());

        // A ruleset that no longer defines the agents topic must clear its rows.
        let only_memory = RuleSet::from_toml(
            r#"
[meta]
version = "v2"
[topics.memory]
threshold = 3.0
rules = [{ id = "title_concept", signal = "title", kind = "fts", weight = 2.0, match = "memor*" }]
[topics.memory.subreddit_prior]
opencodeCLI = 2.0
"#,
        )
        .expect("parse")
        .compile()
        .expect("compile");
        run_tagging_batch(&conn, &only_memory, &TagOptions::default()).expect("second tagging");

        assert!(
            tag_one(&conn, "t3_drop", "memory").is_some(),
            "memory tag should be refreshed"
        );
        assert!(
            tag_one(&conn, "t3_drop", "agents").is_none(),
            "a topic removed from the ruleset should lose its tags on a full re-tag"
        );
    }

    #[test]
    fn topic_filter_scopes_to_one_topic() {
        let conn = test_db();
        insert(
            &conn,
            "t3_scope",
            "agentic memory",
            Some("opencodeCLI"),
            None,
        );

        run_tagging_batch(
            &conn,
            &compiled(),
            &TagOptions {
                topic: Some("agents".to_string()),
                ..TagOptions::default()
            },
        )
        .expect("scoped tagging");

        let tags = db::post_tags_for(&conn, "t3_scope").expect("query");
        let topics: Vec<&str> = tags.iter().map(|tag| tag.topic.as_str()).collect();
        assert_eq!(
            topics,
            vec!["agents"],
            "only the agents topic should be written"
        );
    }

    #[test]
    fn shipped_seed_rules_validate_and_tag() {
        let conn = test_db();
        insert(
            &conn,
            "t3_seed",
            "We built a persistent memory plugin for OpenCode",
            Some("opencodeCLI"),
            None,
        );
        let seed = RuleSet::from_toml(include_str!("../../../../rules.toml"))
            .expect("shipped seed parses")
            .compile()
            .expect("shipped seed compiles");

        let summary =
            run_tagging_batch(&conn, &seed, &TagOptions::default()).expect("seed should tag");

        assert!(summary.topics_evaluated >= 6);
        let tag = tag_one(&conn, "t3_seed", "memory").expect("memory tag from shipped seed");
        assert!(
            tag.passed,
            "shipped seed should pass the worked example: {tag:?}"
        );
    }

    #[test]
    fn limit_preserves_unprocessed_post_tags() {
        let conn = test_db();
        insert(
            &conn,
            "t3_old",
            "persistent memory",
            Some("opencodeCLI"),
            None,
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        insert(
            &conn,
            "t3_new",
            "persistent memory",
            Some("opencodeCLI"),
            None,
        );

        run_tagging_batch(&conn, &compiled(), &TagOptions::default()).expect("full tagging");
        assert!(tag_one(&conn, "t3_old", "memory").is_some());
        assert!(tag_one(&conn, "t3_new", "memory").is_some());

        // Re-tag only the newest post; the unprocessed older post keeps its tag.
        run_tagging_batch(
            &conn,
            &compiled(),
            &TagOptions {
                limit: Some(1),
                ..TagOptions::default()
            },
        )
        .expect("limited tagging");

        assert!(
            tag_one(&conn, "t3_old", "memory").is_some(),
            "a --limit re-tag must not clear tags for unprocessed posts"
        );
        assert!(tag_one(&conn, "t3_new", "memory").is_some());
    }

    #[test]
    fn unknown_topic_filter_errors() {
        let conn = test_db();
        let err = run_tagging_batch(
            &conn,
            &compiled(),
            &TagOptions {
                topic: Some("nope".to_string()),
                ..TagOptions::default()
            },
        )
        .expect_err("unknown topic should error");
        assert!(format!("{err:#}").contains("unknown topic"));
    }
}
