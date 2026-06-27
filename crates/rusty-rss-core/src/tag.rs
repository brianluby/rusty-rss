//! Gate 1 evaluator: the cheap, deterministic, re-runnable tagger.
//!
//! For each topic, every rule is compiled to column-scoped FTS5 operands
//! ([`crate::rules`]); we run each operand once over `posts_fts` to get its
//! matching-rowid set, then do all scoring, `min_hits` counting, subreddit
//! priors, vetoes and thresholds in Rust over those sets. Results are
//! materialized into `post_tags` with full provenance.
//!
//! See `docs/prd/rule-engine.md` for the model and worked examples.

use crate::db;
use crate::models::PostTag;
use crate::rules::{CompiledRule, CompiledRuleSet, CompiledTopic};
use anyhow::{Result, bail};
use chrono::Utc;
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Default)]
pub struct TagOptions {
    /// Tag only this topic; `None` tags every topic in the rules file.
    pub topic: Option<String>,
    /// Optional debug cap on posts processed; `None` processes the archive.
    pub limit: Option<usize>,
    /// Evaluate and report without writing any `post_tags` rows.
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TagSummary {
    pub selected_posts: usize,
    pub topics_evaluated: usize,
    /// Tags produced this run. On a live run this equals the rows written to
    /// `post_tags`; on a `dry_run` it is the number that *would* be written.
    pub rows_written: usize,
    pub passed_count: usize,
    pub vetoed_count: usize,
    /// The computed tags (persisted unless `dry_run`); powers `--json`.
    pub tags: Vec<PostTag>,
}

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

struct RuleHit<'a> {
    id: &'a str,
    weight: f32,
    fired: HashSet<i64>,
}

struct ExcludeHit<'a> {
    id: &'a str,
    matched: HashSet<i64>,
    guarded: Option<HashSet<i64>>,
}

/// Rowids that fire a rule: those matching at least `min_hits` distinct operands.
fn fired_rowids(conn: &Connection, rule: &CompiledRule) -> Result<HashSet<i64>> {
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
fn union_rowids(conn: &Connection, operands: &[String]) -> Result<HashSet<i64>> {
    let mut all = HashSet::new();
    for operand in operands {
        all.extend(db::fts_matching_rowids(conn, operand)?);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SavedPost;
    use crate::rules::RuleSet;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_db() -> Connection {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rusty_rss_tag_test_{}_{}.db",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_file(&path);
        db::init_db(&path).expect("db should initialize")
    }

    fn insert(
        conn: &Connection,
        fullname: &str,
        title: &str,
        subreddit: Option<&str>,
        body: Option<&str>,
    ) {
        let mut post = SavedPost::new(
            fullname.to_string(),
            title.to_string(),
            format!("https://reddit.com/r/x/comments/{fullname}/"),
            "atom".to_string(),
        );
        post.subreddit = subreddit.map(ToString::to_string);
        post.content_markdown = body.map(ToString::to_string);
        db::upsert_post(conn, &post).expect("post should insert");
    }

    const RULES: &str = r#"
[meta]
version = "rules-test-v1"

[topics.memory]
threshold = 3.0
rules = [
  { id = "title_concept", signal = "title", kind = "fts", weight = 2.0, match = 'memor* OR "knowledge graph" OR persisten*' },
  { id = "body_concept", signal = "body", kind = "fts", weight = 0.5, min_hits = 2, match = 'memor* OR retriev* OR embedding*' },
]

[topics.memory.subreddit_prior]
opencodeCLI = 2.0

[[topics.memory.exclude]]
id = "hardware_memory"
signal = "title"
kind = "fts"
match = 'vram OR "gpu memory"'
unless = { signal = "any", kind = "terms", match = ["mem0", "letta"] }
veto = true

[topics.agents]
threshold = 2.0
rules = [
  { id = "title_concept", signal = "title", kind = "fts", weight = 2.0, match = 'agent* OR subagent*' },
]
"#;

    fn ruleset() -> RuleSet {
        RuleSet::from_toml(RULES).expect("rules should parse")
    }

    fn compiled() -> CompiledRuleSet {
        ruleset().compile().expect("rules should compile")
    }

    fn tag_one(conn: &Connection, fullname: &str, topic: &str) -> Option<PostTag> {
        db::post_tags_for(conn, fullname)
            .expect("tags should query")
            .into_iter()
            .find(|tag| tag.topic == topic)
    }

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
        let seed = RuleSet::from_toml(include_str!("../../../rules.toml"))
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
