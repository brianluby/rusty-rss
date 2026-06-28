//! Core domain types: the normalized [`SavedPost`], LLM enrichment shapes
//! ([`EnrichmentOutput`], [`EnrichmentRecord`], [`Classification`],
//! [`RecommendedAction`]), outbound [`OutboundCapture`], rule-engine
//! [`PostTag`], the agent [`ExportRecord`], and the [`SyncResult`] summary.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// Normalized representation of a saved Reddit post from any source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedPost {
    /// Reddit fullname (e.g. `t3_abc123`); the stable primary identifier.
    pub reddit_fullname: String,
    /// Bare id with the type prefix stripped (e.g. `abc123`).
    pub reddit_id: String,
    /// Post title.
    pub title: String,
    /// Post author, if known.
    pub author: Option<String>,
    /// Subreddit the post belongs to, if known.
    pub subreddit: Option<String>,
    /// Permalink to the post on Reddit.
    pub permalink: String,
    /// Outbound link the post points to, if any.
    pub outbound_url: Option<String>,
    /// Post body rendered as markdown, if available.
    pub content_markdown: Option<String>,
    /// Thumbnail image URL, if any.
    pub thumbnail_url: Option<String>,
    /// Original publication time, if known.
    pub published_at: Option<DateTime<Utc>>,
    /// Last-updated time reported by the source, if known.
    pub updated_at: Option<DateTime<Utc>>,
    /// Feed source that produced this post (e.g. `atom`).
    pub source: String,
}

/// What kind of thing a saved post is, as judged during enrichment.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// A written piece making an argument or telling a story.
    Article,
    /// Software, a library, a service, or a product that can be used.
    Tool,
    /// Step-by-step instructions teaching how to do something.
    Tutorial,
    /// Documentation, a spec, or material to look things up in.
    Reference,
    /// An open-ended conversation, debate, or opinion thread.
    Discussion,
    /// A request for help or an answer to one.
    Question,
    /// A time-sensitive report on a recent event or release.
    News,
    /// None of the other classifications fit.
    Other,
}

impl Classification {
    /// The lowercase wire/storage string for this classification.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Article => "article",
            Self::Tool => "tool",
            Self::Tutorial => "tutorial",
            Self::Reference => "reference",
            Self::Discussion => "discussion",
            Self::Question => "question",
            Self::News => "news",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for Classification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Classification {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "article" => Ok(Self::Article),
            "tool" => Ok(Self::Tool),
            "tutorial" => Ok(Self::Tutorial),
            "reference" => Ok(Self::Reference),
            "discussion" => Ok(Self::Discussion),
            "question" => Ok(Self::Question),
            "news" => Ok(Self::News),
            "other" => Ok(Self::Other),
            _ => Err(format!("unknown classification: {value}")),
        }
    }
}

/// The action enrichment recommends taking on a saved post.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedAction {
    /// A tool or technique worth trying hands-on soon.
    ShouldTest,
    /// Something to implement or build a project from.
    ShouldBuild,
    /// Worth reading in full later; no other action needed.
    ReadingQueue,
    /// Keep for future lookup; no reading or action planned.
    ReferenceOnly,
    /// Low value for this archive; safe to drop.
    Discard,
    /// None of the other actions fit.
    Other,
}

impl RecommendedAction {
    /// The lowercase wire/storage string for this action.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShouldTest => "should_test",
            Self::ShouldBuild => "should_build",
            Self::ReadingQueue => "reading_queue",
            Self::ReferenceOnly => "reference_only",
            Self::Discard => "discard",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for RecommendedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RecommendedAction {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "should_test" => Ok(Self::ShouldTest),
            "should_build" => Ok(Self::ShouldBuild),
            "reading_queue" => Ok(Self::ReadingQueue),
            "reference_only" => Ok(Self::ReferenceOnly),
            "discard" => Ok(Self::Discard),
            "other" => Ok(Self::Other),
            _ => Err(format!("unknown recommended_action: {value}")),
        }
    }
}

/// The structured result the LLM returns for a single post.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnrichmentOutput {
    /// What kind of content the post is.
    pub classification: Classification,
    /// Free-form topic tags assigned to the post.
    pub tags: Vec<String>,
    /// Concise neutral summary of the post.
    pub summary: String,
    /// Personal-interest score in `[0.0, 1.0]`.
    pub joy_value: f32,
    /// Build/learn-utility score in `[0.0, 1.0]`.
    pub work_value: f32,
    /// The recommended next action for the post.
    pub recommended_action: RecommendedAction,
    /// One-sentence justification for the recommended action.
    pub rationale: String,
    /// Model certainty in the classification, in `[0.0, 1.0]`.
    pub confidence: f32,
}

impl EnrichmentOutput {
    /// Validate semantic constraints not expressible in the type: summary and
    /// rationale must be non-empty, and each score must be a finite number in
    /// `[0.0, 1.0]`. Returns a human-readable error describing the first failure.
    pub fn validate(&self) -> Result<(), String> {
        if self.summary.trim().is_empty() {
            return Err("summary is required".to_string());
        }
        if self.rationale.trim().is_empty() {
            return Err("rationale is required".to_string());
        }
        if !(0.0..=1.0).contains(&self.joy_value) || !self.joy_value.is_finite() {
            return Err("joy_value must be a finite number from 0.0 to 1.0".to_string());
        }
        if !(0.0..=1.0).contains(&self.work_value) || !self.work_value.is_finite() {
            return Err("work_value must be a finite number from 0.0 to 1.0".to_string());
        }
        if !(0.0..=1.0).contains(&self.confidence) || !self.confidence.is_finite() {
            return Err("confidence must be a finite number from 0.0 to 1.0".to_string());
        }

        Ok(())
    }
}

/// A persisted enrichment run for a post, success or failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnrichmentRecord {
    /// Database row id.
    pub id: i64,
    /// Reddit fullname of the enriched post.
    pub reddit_fullname: String,
    /// Provider that produced the run (e.g. `openai`).
    pub provider: String,
    /// Model used for the run.
    pub model: String,
    /// Prompt version in effect for the run.
    pub prompt_version: String,
    /// Outcome status, `"success"` or `"error"`.
    pub status: String,
    /// Raw provider response text, if retained.
    pub raw_response: Option<String>,
    /// Parsed output, present only for successful runs.
    pub output: Option<EnrichmentOutput>,
    /// Error message, present only for failed runs.
    pub error: Option<String>,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
}

/// A post paired with its latest enrichment, for triage listings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TriageItem {
    /// Reddit fullname of the post.
    pub reddit_fullname: String,
    /// Post title.
    pub title: String,
    /// Subreddit the post belongs to, if known.
    pub subreddit: Option<String>,
    /// Post author, if known.
    pub author: Option<String>,
    /// Permalink to the post.
    pub permalink: String,
    /// Outbound URL of the post, if any.
    pub outbound_url: Option<String>,
    /// The post's latest enrichment run, if any.
    pub enrichment: Option<EnrichmentRecord>,
}

/// The stored result of capturing a post's outbound link.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutboundCapture {
    /// Reddit fullname of the owning post.
    pub reddit_fullname: String,
    /// The outbound URL that was requested.
    pub original_url: String,
    /// Final URL after resolution, if the fetch succeeded.
    pub final_url: Option<String>,
    /// Canonical URL declared by the page, if any.
    pub canonical_url: Option<String>,
    /// Extracted page title, if any.
    pub title: Option<String>,
    /// Extracted page description, if any.
    pub description: Option<String>,
    /// Extracted site name, if any.
    pub site_name: Option<String>,
    /// Extracted preview image URL, if any.
    pub preview_image_url: Option<String>,
    /// Markdown snapshot of the page body, if any.
    pub content_markdown: Option<String>,
    /// `sha256:`-prefixed hash of the markdown content, if any.
    pub content_hash: Option<String>,
    /// Outcome status, `"success"` or `"error"`.
    pub status: String,
    /// HTTP status code of the response, if one was received.
    pub http_status: Option<i64>,
    /// Error message when the capture failed.
    pub error: Option<String>,
    /// Timestamp of the most recent capture attempt (RFC 3339).
    pub fetched_at: String,
    /// Number of capture attempts made for this post.
    pub attempt_count: i64,
}

/// A Gate 1 rule-engine tag: one row per `(post, topic)` that scored, with the
/// provenance needed to explain and tune the gate. Materialized output of the
/// `tag` command (see `docs/prd/rule-engine.md`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostTag {
    /// Reddit fullname of the tagged post.
    pub reddit_fullname: String,
    /// Topic this tag scores the post against.
    pub topic: String,
    /// Computed score for the topic.
    pub score: f32,
    /// Score threshold the topic required to pass.
    pub threshold: f32,
    /// Whether the score met or exceeded the threshold.
    pub passed: bool,
    /// Rule ids that fired, plus `prior:<subreddit>` and `veto:<id>` markers.
    pub matched_rules: Vec<String>,
    /// Per-signal score breakdown, keyed by rule id (and `prior`).
    pub signals: BTreeMap<String, f32>,
    /// Version of the ruleset that produced this tag.
    pub ruleset_version: String,
    /// Timestamp the tag was computed (RFC 3339).
    pub tagged_at: String,
}

/// A self-describing agent export bundling a post with its derived data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportRecord {
    /// Export schema version (e.g. `rusty-rss.export.v1`).
    pub schema_version: String,
    /// The saved post.
    pub saved_post: SavedPost,
    /// The post's latest enrichment, if any.
    pub latest_enrichment: Option<EnrichmentRecord>,
    /// The post's outbound capture, if any.
    pub outbound_capture: Option<OutboundCapture>,
}

impl SavedPost {
    /// Construct a post from its core fields, deriving [`reddit_id`](Self::reddit_id)
    /// by stripping the `t1_`/`t2_`/`t3_` fullname prefix and leaving optional
    /// fields empty.
    pub fn new(fullname: String, title: String, permalink: String, source: String) -> Self {
        let reddit_id = fullname
            .strip_prefix("t3_")
            .or_else(|| fullname.strip_prefix("t1_"))
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
            content_markdown: None,
            thumbnail_url: None,
            published_at: None,
            updated_at: None,
            source,
        }
    }
}

/// Summary counts for a completed feed sync.
#[derive(Debug, Clone, Default)]
pub struct SyncResult {
    /// Number of posts fetched from the feed.
    pub fetched_count: usize,
    /// Number of new posts inserted.
    pub inserted_count: usize,
    /// Number of existing posts updated.
    pub updated_count: usize,
    /// Number of existing posts seen again unchanged.
    pub unchanged_count: usize,
    /// Number of feed pages walked.
    pub page_count: usize,
    /// Non-fatal per-entry parse errors encountered during the sync.
    pub parse_errors: Vec<String>,
}

impl SyncResult {
    /// Create an empty result with all counts at zero.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_ids_for_posts_and_comments() {
        let post = SavedPost::new(
            "t3_post123".to_string(),
            "post".to_string(),
            "https://reddit.com/post".to_string(),
            "atom".to_string(),
        );
        let comment = SavedPost::new(
            "t1_comment123".to_string(),
            "comment".to_string(),
            "https://reddit.com/comment".to_string(),
            "atom".to_string(),
        );

        assert_eq!(post.reddit_id, "post123");
        assert_eq!(comment.reddit_id, "comment123");
    }

    #[test]
    fn validates_enrichment_scores() {
        let output = EnrichmentOutput {
            classification: Classification::Reference,
            tags: vec!["rust".to_string()],
            summary: "Useful Rust reference".to_string(),
            joy_value: 0.2,
            work_value: 1.2,
            recommended_action: RecommendedAction::ReferenceOnly,
            rationale: "Relevant to future Rust work".to_string(),
            confidence: 0.8,
        };

        let err = output.validate().expect_err("invalid score should fail");
        assert!(err.contains("work_value"));
    }

    #[test]
    fn rejects_unknown_enrichment_fields() {
        let json = r#"{
            "classification": "reference",
            "tags": ["rust"],
            "summary": "Useful",
            "joy_value": 0.2,
            "work_value": 0.7,
            "recommended_action": "reference_only",
            "rationale": "Useful later",
            "confidence": 0.8,
            "surprise": true
        }"#;

        let err =
            serde_json::from_str::<EnrichmentOutput>(json).expect_err("unknown fields should fail");
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn export_schema_sample_validates() {
        let sample = include_str!("../../../docs/export-record-v1.sample.json");
        let record: ExportRecord =
            serde_json::from_str(sample).expect("sample export record should validate");

        assert_eq!(record.schema_version, "rusty-rss.export.v1");
        assert_eq!(record.saved_post.reddit_fullname, "t3_sample");
        assert!(record.outbound_capture.is_some());
    }
}
