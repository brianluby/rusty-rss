//! Public configuration and result types for the tagger.

use crate::models::PostTag;

/// Options controlling a tagging run.
#[derive(Debug, Clone, Default)]
pub struct TagOptions {
    /// Tag only this topic; `None` tags every topic in the rules file.
    pub topic: Option<String>,
    /// Optional debug cap on posts processed; `None` processes the archive.
    pub limit: Option<usize>,
    /// Evaluate and report without writing any `post_tags` rows.
    pub dry_run: bool,
}

/// Outcome counts and computed tags for a completed tagging run.
#[derive(Debug, Clone, Default)]
pub struct TagSummary {
    /// Number of posts evaluated.
    pub selected_posts: usize,
    /// Number of topics evaluated.
    pub topics_evaluated: usize,
    /// Tags produced this run. On a live run this equals the rows written to
    /// `post_tags`; on a `dry_run` it is the number that *would* be written.
    pub rows_written: usize,
    /// Number of computed tags that passed their threshold.
    pub passed_count: usize,
    /// Number of topic assignments suppressed by a veto.
    pub vetoed_count: usize,
    /// The computed tags (persisted unless `dry_run`); powers `--json`.
    pub tags: Vec<PostTag>,
}
