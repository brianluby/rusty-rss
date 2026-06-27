//! SQLite persistence for rusty-rss, organized by domain.
//!
//! Schema and migrations live in [`schema`]; everything else is grouped by the
//! table it serves — [`posts`], full-text [`search`], [`enrichment`] and triage,
//! outbound [`captures`], agent [`export`], and Gate 1 [`tags`]. This root keeps
//! no logic of its own; it just re-exports the public API so callers use `db::*`.

mod captures;
mod enrichment;
mod export;
mod posts;
mod schema;
mod search;
mod tags;

#[cfg(test)]
mod test_support;

pub use captures::{
    OutboundCaptureCandidate, OutboundCaptureUpsert, latest_outbound_capture,
    list_outbound_capture_candidates, upsert_outbound_capture,
};
pub use enrichment::{
    TriageView, latest_enrichment, list_enrichment_candidates, list_triage_items,
    record_enrichment_failure, record_enrichment_success,
};
pub use export::{ExportFilters, list_export_records};
pub use posts::{SavedPostRow, UpsertResult, count_posts, get_post, list_posts, upsert_post};
pub use schema::init_db;
pub use search::{SearchFilters, SearchHit, search_posts};
pub use tags::{
    TaggablePost, fts_matching_rowids, list_post_tags, list_taggable_posts, post_tags_for,
    replace_post_tags, validate_fts_expr,
};
