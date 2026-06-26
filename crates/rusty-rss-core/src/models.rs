use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

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
    pub content_markdown: Option<String>,
    pub thumbnail_url: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub source: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Article,
    Tool,
    Tutorial,
    Reference,
    Discussion,
    Question,
    News,
    Other,
}

impl Classification {
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedAction {
    ShouldTest,
    ShouldBuild,
    ReadingQueue,
    ReferenceOnly,
    Discard,
    Other,
}

impl RecommendedAction {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnrichmentOutput {
    pub classification: Classification,
    pub tags: Vec<String>,
    pub summary: String,
    pub joy_value: f32,
    pub work_value: f32,
    pub recommended_action: RecommendedAction,
    pub rationale: String,
    pub confidence: f32,
}

impl EnrichmentOutput {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnrichmentRecord {
    pub id: i64,
    pub reddit_fullname: String,
    pub provider: String,
    pub model: String,
    pub prompt_version: String,
    pub status: String,
    pub raw_response: Option<String>,
    pub output: Option<EnrichmentOutput>,
    pub error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TriageItem {
    pub reddit_fullname: String,
    pub title: String,
    pub subreddit: Option<String>,
    pub author: Option<String>,
    pub permalink: String,
    pub outbound_url: Option<String>,
    pub enrichment: Option<EnrichmentRecord>,
}

impl SavedPost {
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

#[derive(Debug, Clone, Default)]
pub struct SyncResult {
    pub fetched_count: usize,
    pub inserted_count: usize,
    pub updated_count: usize,
    pub unchanged_count: usize,
    pub page_count: usize,
    pub parse_errors: Vec<String>,
}

impl SyncResult {
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
}
