//! Public configuration and result types for the tagger.

use crate::models::PostTag;

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
