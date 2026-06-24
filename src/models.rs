use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Normalized representation of a saved Reddit post from any source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPost {
    pub reddit_fullname: String,
    pub reddit_id: String,
    pub title: String,
    pub author: Option<String>,
    pub subreddit: Option<String>,
    pub permalink: String,
    pub outbound_url: Option<String>,
    pub content_html: Option<String>,
    pub thumbnail_url: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub source: String,
}

impl SavedPost {
    pub fn new(fullname: String, title: String, permalink: String, source: String) -> Self {
        let reddit_id = fullname
            .strip_prefix("t3_")
            .or_else(|| fullname.strip_prefix("t2_"))
            .unwrap_or(&fullname)
            .to_string();

        Self {
            reddit_fullname: fullname,
            reddit_id,
            title,
            author: None,
            subreddit: None,
            permalink,
            outbound_url: None,
            content_html: None,
            thumbnail_url: None,
            published_at: None,
            updated_at: None,
            source,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SyncResult {
    pub fetched_count: usize,
    pub inserted_count: usize,
    pub updated_count: usize,
    pub unchanged_count: usize,
    pub parse_errors: Vec<String>,
}

impl SyncResult {
    pub fn new() -> Self {
        Self {
            fetched_count: 0,
            inserted_count: 0,
            updated_count: 0,
            unchanged_count: 0,
            parse_errors: Vec::new(),
        }
    }
}
