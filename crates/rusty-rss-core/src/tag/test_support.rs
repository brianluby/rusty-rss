//! Shared fixtures for the tagger tests.

use crate::db;
use crate::models::{PostTag, SavedPost};
use crate::rules::{CompiledRuleSet, RuleSet};
use crate::test_support::reset_db_file;
use rusqlite::Connection;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn test_db() -> Connection {
    let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "rusty_rss_tag_test_{}_{}.db",
        std::process::id(),
        id
    ));
    reset_db_file(&path);
    db::init_db(&path).expect("db should initialize")
}

pub(crate) fn insert(
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

pub(crate) fn compiled() -> CompiledRuleSet {
    ruleset().compile().expect("rules should compile")
}

pub(crate) fn tag_one(conn: &Connection, fullname: &str, topic: &str) -> Option<PostTag> {
    db::post_tags_for(conn, fullname)
        .expect("tags should query")
        .into_iter()
        .find(|tag| tag.topic == topic)
}
